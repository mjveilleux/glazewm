use tracing::info;
use wm_common::{DisplayState, HideMethod};
use wm_platform::NativeWindow;

#[cfg(target_os = "windows")]
use crate::traits::CommonGetters;
use crate::{
  commands::window::manage_window, traits::WindowGetters,
  user_config::UserConfig, wm_state::WmState,
};

pub fn handle_window_shown(
  native_window: &NativeWindow,
  state: &mut WmState,
  config: &mut UserConfig,
) -> anyhow::Result<()> {
  // A window hidden the moment it was shown (see
  // `animation_manager::hides_new_windows`).
  #[cfg(target_os = "windows")]
  let hidden_on_show = {
    use wm_platform::NativeWindowWindowsExt;
    native_window.take_hidden_on_show()
  };

  let result = show_or_manage(native_window, state, config);

  // A window that's now managed is the animations' to show: it fades in
  // once in place, or gets its opacity back on its next redraw. Any other
  // is shown again as it was.
  #[cfg(target_os = "windows")]
  if let Some(hidden) = hidden_on_show {
    match state.window_from_native(native_window) {
      Some(window) => {
        state.animation_manager.mark_hidden(window.id());
        state.pending_sync.queue_container_to_redraw(window);
      }
      None => hidden.restore(native_window),
    }
  }

  result
}

/// Updates an already managed window that was shown, or manages a new one.
fn show_or_manage(
  native_window: &NativeWindow,
  state: &mut WmState,
  config: &mut UserConfig,
) -> anyhow::Result<()> {
  let found_window = state.window_from_native(native_window);

  if let Some(window) = found_window {
    info!("Window shown: {window}");

    // Update display state if window is already managed.
    if config.value.general.hide_method != HideMethod::PlaceInCorner
      && window.display_state() == DisplayState::Showing
    {
      window.set_display_state(DisplayState::Shown);
    } else {
      state.pending_sync.queue_container_to_redraw(window);
    }
  } else if !state.ignored_windows.contains(native_window) {
    // If the window is not managed and not explicitly ignored, manage it.
    manage_window(native_window.clone(), None, state, config)?;
  }

  Ok(())
}
