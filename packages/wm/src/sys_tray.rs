use std::{
  fmt::{self, Display},
  path::Path,
  str::FromStr,
  sync::{Arc, Mutex},
  time::{Duration, Instant},
};

use anyhow::Context;
use auto_launch::AutoLaunch;
use tokio::sync::mpsc;
use tray_icon::{
  menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem},
  Icon, TrayIcon, TrayIconBuilder,
};
#[cfg(target_os = "windows")]
use wm_platform::DispatcherExtWindows;
use wm_platform::{Dispatcher, ThreadBound};

#[derive(Debug, Clone, Eq, PartialEq)]
enum TrayMenuId {
  ReloadConfig,
  ShowConfigFolder,
  #[cfg(target_os = "windows")]
  ToggleWindowAnimations,
  RunOnStartup,
  Exit,
}

impl Display for TrayMenuId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      TrayMenuId::ReloadConfig => write!(f, "reload_config"),
      TrayMenuId::ShowConfigFolder => write!(f, "show_config_folder"),
      #[cfg(target_os = "windows")]
      TrayMenuId::ToggleWindowAnimations => {
        write!(f, "toggle_window_animations")
      }
      TrayMenuId::RunOnStartup => write!(f, "run_on_startup"),
      TrayMenuId::Exit => write!(f, "exit"),
    }
  }
}

impl FromStr for TrayMenuId {
  type Err = anyhow::Error;

  fn from_str(event: &str) -> Result<Self, Self::Err> {
    match event {
      "show_config_folder" => Ok(Self::ShowConfigFolder),
      "reload_config" => Ok(Self::ReloadConfig),
      #[cfg(target_os = "windows")]
      "toggle_window_animations" => Ok(Self::ToggleWindowAnimations),
      "run_on_startup" => Ok(Self::RunOnStartup),
      "exit" => Ok(Self::Exit),
      _ => anyhow::bail!("Invalid tray menu event: {}", event),
    }
  }
}

/// How often to retry adding the tray icon while the taskbar isn't ready.
const TRAY_RETRY_INTERVAL: Duration = Duration::from_millis(250);

/// How long to wait for the taskbar before giving up on the tray icon.
const TRAY_RETRY_TIMEOUT: Duration = Duration::from_secs(300);

/// How many times to try adding the tray icon once the taskbar is ready.
///
/// Explorer rejects icons for a moment after its notification area appears
/// (e.g. at sign-in); attempts are 250 ms apart, so this allows 10 s.
const TRAY_MAX_ATTEMPTS: u32 = 40;

pub struct SystemTray {
  pub config_reload_rx: mpsc::UnboundedReceiver<()>,
  pub exit_rx: mpsc::UnboundedReceiver<()>,
  _icon_thread: Option<std::thread::JoinHandle<()>>,
  /// The tray icon, once it has been added (see `SystemTray::new`).
  _tray_icon: Arc<Mutex<Option<ThreadBound<TrayIcon>>>>,
}

impl SystemTray {
  /// Install the system tray on the main thread after the run loop starts.
  pub fn new(
    config_path: &Path,
    dispatcher: Dispatcher,
  ) -> anyhow::Result<Self> {
    let (exit_tx, exit_rx) = mpsc::unbounded_channel();
    let (config_reload_tx, config_reload_rx) = mpsc::unbounded_channel();

    let animations_enabled = Arc::new(Mutex::new({
      #[cfg(target_os = "windows")]
      {
        dispatcher.window_animations_enabled().unwrap_or(false)
      }
      #[cfg(not(target_os = "windows"))]
      {
        false
      }
    }));

    let run_on_startup_enabled = Arc::new(Mutex::new(
      auto_launch_instance()
        .and_then(|auto_launch| {
          auto_launch.is_enabled().map_err(Into::into)
        })
        .unwrap_or(false),
    ));

    // The icon lives in the taskbar's notification area, which doesn't
    // exist yet when launched at sign-in, before Explorer has started.
    // Window management doesn't depend on it, so carry on without the icon
    // and add it once the taskbar is up.
    let tray_icon = Arc::new(Mutex::new(None));

    if Self::is_taskbar_ready(&dispatcher) {
      match Self::add_tray_icon(
        &dispatcher,
        &animations_enabled,
        &run_on_startup_enabled,
      ) {
        Ok(icon) => {
          if let Ok(mut slot) = tray_icon.lock() {
            *slot = Some(icon);
          }
        }
        Err(err) => {
          tracing::warn!("Failed to add tray icon, retrying: {}", err);

          Self::retry_add_tray_icon(
            dispatcher.clone(),
            animations_enabled.clone(),
            run_on_startup_enabled.clone(),
            tray_icon.clone(),
            1,
          );
        }
      }
    } else {
      tracing::info!("Taskbar not ready; adding tray icon once it is.");

      Self::retry_add_tray_icon(
        dispatcher.clone(),
        animations_enabled.clone(),
        run_on_startup_enabled.clone(),
        tray_icon.clone(),
        0,
      );
    }

    // Spawn thread to handle tray menu events.
    let config_path = config_path.to_owned();
    let icon_thread = std::thread::spawn(move || {
      let menu_event_rx = MenuEvent::receiver();

      while let Ok(event) = menu_event_rx.recv() {
        if let Ok(menu_event) = TrayMenuId::from_str(event.id.as_ref()) {
          if let Err(err) = Self::handle_menu_event(
            &menu_event,
            &dispatcher,
            &config_path,
            &config_reload_tx,
            &exit_tx,
            &animations_enabled,
            &run_on_startup_enabled,
          ) {
            tracing::warn!("Failed to handle tray menu event: {}", err);
          }
        }
      }
    });

    Ok(Self {
      config_reload_rx,
      exit_rx,
      _icon_thread: Some(icon_thread),
      _tray_icon: tray_icon,
    })
  }

  /// Creates the tray icon on the event loop thread.
  ///
  /// Fails while the taskbar's notification area doesn't exist.
  fn add_tray_icon(
    dispatcher: &Dispatcher,
    animations_enabled: &Arc<Mutex<bool>>,
    run_on_startup_enabled: &Arc<Mutex<bool>>,
  ) -> anyhow::Result<ThreadBound<TrayIcon>> {
    let animations_enabled = animations_enabled
      .lock()
      .map(|value| *value)
      .unwrap_or(false);
    let run_on_startup_enabled = run_on_startup_enabled
      .lock()
      .map(|value| *value)
      .unwrap_or(false);

    dispatcher.dispatch_sync(|| {
      Self::create_tray_icon(animations_enabled, run_on_startup_enabled)
        .map(|icon| ThreadBound::new(icon, dispatcher.clone()))
    })?
  }

  /// Gets whether the taskbar can take a tray icon.
  fn is_taskbar_ready(
    // LINT: `dispatcher` is only used on Windows.
    #[cfg_attr(not(target_os = "windows"), allow(unused_variables))]
    dispatcher: &Dispatcher,
  ) -> bool {
    #[cfg(target_os = "windows")]
    {
      dispatcher.is_taskbar_ready()
    }
    #[cfg(not(target_os = "windows"))]
    {
      true
    }
  }

  /// Adds the tray icon on a background thread once the taskbar is ready.
  ///
  /// Waits for the taskbar (up to `TRAY_RETRY_TIMEOUT`), then tries up to
  /// `TRAY_MAX_ATTEMPTS` times in all, counting `failed_attempts` already
  /// made. Stops if the event loop stops. Once added, the tray icon is
  /// restored by `tray_icon` itself whenever Explorer restarts.
  fn retry_add_tray_icon(
    dispatcher: Dispatcher,
    animations_enabled: Arc<Mutex<bool>>,
    run_on_startup_enabled: Arc<Mutex<bool>>,
    tray_icon: Arc<Mutex<Option<ThreadBound<TrayIcon>>>>,
    failed_attempts: u32,
  ) {
    std::thread::spawn(move || {
      let started = Instant::now();
      let mut attempts = failed_attempts;

      while attempts < TRAY_MAX_ATTEMPTS
        && started.elapsed() < TRAY_RETRY_TIMEOUT
      {
        std::thread::sleep(TRAY_RETRY_INTERVAL);

        if dispatcher.is_stopped() {
          return;
        }

        if !Self::is_taskbar_ready(&dispatcher) {
          continue;
        }

        match Self::add_tray_icon(
          &dispatcher,
          &animations_enabled,
          &run_on_startup_enabled,
        ) {
          Ok(icon) => {
            if let Ok(mut slot) = tray_icon.lock() {
              *slot = Some(icon);
            }

            tracing::info!(
              "Added tray icon after {} ms.",
              started.elapsed().as_millis()
            );
            return;
          }
          Err(err) => {
            attempts += 1;
            tracing::warn!(
              "Failed to add tray icon (attempt {}): {}",
              attempts,
              err
            );
          }
        }
      }

      tracing::warn!("Gave up adding the tray icon.");
    });
  }
  fn create_tray_icon(
    // LINT: `animations_enabled` is only used on Windows.
    #[cfg_attr(not(target_os = "windows"), allow(unused_variables))]
    animations_enabled: bool,
    run_on_startup_enabled: bool,
  ) -> anyhow::Result<TrayIcon> {
    let reload_config_item = MenuItem::with_id(
      TrayMenuId::ReloadConfig,
      "Reload config",
      true,
      None,
    );

    let config_dir_item = MenuItem::with_id(
      TrayMenuId::ShowConfigFolder,
      "Show config folder",
      true,
      None,
    );

    #[cfg(target_os = "windows")]
    let toggle_animations_item = CheckMenuItem::with_id(
      TrayMenuId::ToggleWindowAnimations,
      "Window animations",
      true,
      animations_enabled,
      None,
    );

    let run_on_startup_item = CheckMenuItem::with_id(
      TrayMenuId::RunOnStartup,
      "Run on system startup",
      true,
      run_on_startup_enabled,
      None,
    );

    let exit_item =
      MenuItem::with_id(TrayMenuId::Exit, "Exit", true, None);

    let tray_menu = Menu::new();
    tray_menu.append_items(&[
      &reload_config_item,
      &config_dir_item,
      #[cfg(target_os = "windows")]
      &toggle_animations_item,
      &run_on_startup_item,
      &PredefinedMenuItem::separator(),
      &exit_item,
    ])?;

    let icon = Self::load_icon(include_bytes!(
      "../../../resources/assets/icon.png"
    ))?;

    let tray_icon = TrayIconBuilder::new()
      .with_menu(Box::new(tray_menu))
      .with_tooltip(format!("GlazeWM v{}", env!("VERSION_NUMBER")))
      .with_icon(icon)
      .build()?;

    Ok(tray_icon)
  }

  fn load_icon(bytes: &[u8]) -> anyhow::Result<Icon> {
    let (icon_rgba, icon_width, icon_height) = {
      let image = image::load_from_memory(bytes)
        .context("Failed to to create tray icon image from resource.")?
        .into_rgba8();

      let (width, height) = image.dimensions();
      let rgba = image.into_raw();
      (rgba, width, height)
    };

    Ok(tray_icon::Icon::from_rgba(
      icon_rgba,
      icon_width,
      icon_height,
    )?)
  }

  fn handle_menu_event(
    menu_id: &TrayMenuId,
    dispatcher: &Dispatcher,
    config_path: &Path,
    config_reload_tx: &mpsc::UnboundedSender<()>,
    exit_tx: &mpsc::UnboundedSender<()>,
    // LINT: `animations_enabled` is only used on Windows.
    #[cfg_attr(not(target_os = "windows"), allow(unused_variables))]
    animations_enabled: &Arc<Mutex<bool>>,
    run_on_startup_enabled: &Arc<Mutex<bool>>,
  ) -> anyhow::Result<()> {
    tracing::info!("Processing tray menu event: {:?}", menu_id);

    match menu_id {
      TrayMenuId::ShowConfigFolder => {
        dispatcher.open_file_explorer({
          #[cfg(target_os = "windows")]
          {
            config_path.parent().context("Invalid config path.")?
          }
          #[cfg(target_os = "macos")]
          {
            // On macOS, pass the file path directly since Finder
            // navigates one level too high with the parent directory.
            config_path
          }
        })?;

        Ok(())
      }
      TrayMenuId::ReloadConfig => {
        config_reload_tx.send(())?;
        Ok(())
      }
      #[cfg(target_os = "windows")]
      TrayMenuId::ToggleWindowAnimations => {
        let mut animations_enabled = animations_enabled.lock().unwrap();
        dispatcher.set_window_animations_enabled(!*animations_enabled)?;
        *animations_enabled = !*animations_enabled;
        Ok(())
      }
      TrayMenuId::RunOnStartup => {
        let mut run_on_startup_enabled =
          run_on_startup_enabled.lock().unwrap();

        if *run_on_startup_enabled {
          auto_launch_instance()?.disable()?;
        } else {
          auto_launch_instance()?.enable()?;
        }

        *run_on_startup_enabled = !*run_on_startup_enabled;
        Ok(())
      }
      TrayMenuId::Exit => {
        exit_tx.send(())?;
        Ok(())
      }
    }
  }
}

/// Creates a new [`AutoLaunch`] instance for managing auto-launch at
/// system startup.
fn auto_launch_instance() -> anyhow::Result<AutoLaunch> {
  let exe_path = std::env::current_exe()?.to_string_lossy().to_string();
  let args: [&str; 0] = [];

  #[cfg(target_os = "windows")]
  let instance = AutoLaunch::new("GlazeWM", &exe_path, &args);

  #[cfg(target_os = "macos")]
  let instance = AutoLaunch::new("GlazeWM", &exe_path, false, &args);

  Ok(instance)
}
