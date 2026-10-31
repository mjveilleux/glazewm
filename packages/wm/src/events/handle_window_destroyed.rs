use anyhow::Context;
use tracing::info;
use wm_platform::WindowId;

use crate::{
  commands::{window::unmanage_window, workspace::deactivate_workspace},
  traits::{CommonGetters, WindowGetters},
  user_config::UserConfig,
  wm_state::WmState,
};

// LINT: `config` is only used on Windows.
#[cfg_attr(not(target_os = "windows"), allow(unused_variables))]
pub fn handle_window_destroyed(
  native_window_id: WindowId,
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  let found_window = state
    .windows()
    .into_iter()
    .find(|window| window.native().id() == native_window_id);

  // Unmanage the window if it's currently managed.
  if let Some(window) = found_window {
    let workspace = window.workspace().context("No workspace.")?;

    info!("Window closed: {window}");

    // Closed mid-slide: the slide's snapshot plays the close animation.
    #[cfg(target_os = "windows")]
    {
      let frame_rate = window
        .monitor()
        .and_then(|monitor| monitor.native_properties().refresh_rate)
        .unwrap_or(60);

      state.animation_manager.close_sliding_window(
        &window.id(),
        config,
        frame_rate,
        &state.dispatcher,
      );
    }

    unmanage_window(window, state)?;

    // Destroy parent workspace if window was killed while its workspace
    // was not displayed (e.g. via task manager).
    if !workspace.config().keep_alive
      && !workspace.has_children()
      && !workspace.is_displayed()
    {
      deactivate_workspace(workspace, state)?;
    }
  }

  Ok(())
}
