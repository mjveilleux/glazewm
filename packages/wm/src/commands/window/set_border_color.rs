use wm_common::{BorderColorOverride, SetBorderColorCommand};
#[cfg(target_os = "windows")]
use wm_platform::NativeWindowWindowsExt;

use crate::{
  models::WindowContainer, traits::WindowGetters, user_config::UserConfig,
  wm_state::WmState,
};

/// Sets or clears (with `--reset`) a window's own border colors, which
/// take precedence over the `window_effects` border colors.
///
/// Does nothing if the window already has the given colors, since window
/// rules can run this on every title change.
pub fn set_border_color(
  window: &WindowContainer,
  args: &SetBorderColorCommand,
  state: &mut WmState,
  // LINT: `config` is only used on Windows.
  #[cfg_attr(not(target_os = "windows"), allow(unused_variables))]
  config: &UserConfig,
) {
  let new_override = if args.reset {
    None
  } else {
    args
      .color
      .clone()
      .map(|color| BorderColorOverride::new(color, args.unfocused.clone()))
  };

  if window.border_color_override() == new_override {
    return;
  }

  window.set_border_color_override(new_override.clone());

  // With the override gone and border effects disabled in the config,
  // nothing else resets the border, so return it to the system default.
  #[cfg(target_os = "windows")]
  {
    let window_effects = &config.value.window_effects;

    if new_override.is_none()
      && !window_effects.focused_window.border.enabled
      && !window_effects.other_windows.border.enabled
    {
      _ = window.native().set_border_color(None);
    }
  }

  state.pending_sync.queue_window_effects_update(window);
}
