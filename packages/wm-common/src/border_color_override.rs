use wm_platform::Color;

use crate::WindowEffectConfig;

/// Border colors set on one window with the `set-border-color` command,
/// used in place of the `window_effects` border colors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BorderColorOverride {
  /// Border color while the window has focus.
  pub focused: Color,

  /// Border color while the window doesn't have focus.
  pub unfocused: Color,
}

impl BorderColorOverride {
  /// Creates an override from the command's colors.
  ///
  /// Without an unfocused color, the window keeps the same border color
  /// whether or not it has focus.
  #[must_use]
  pub fn new(focused: Color, unfocused: Option<Color>) -> Self {
    Self {
      unfocused: unfocused.unwrap_or_else(|| focused.clone()),
      focused,
    }
  }
}

/// Resolves the border color to apply to a window.
///
/// A window's override wins over the `window_effects` config, and applies
/// even when the config's border effect is disabled. `effect_config` is
/// the config for the window's focus state (`focused_window` or
/// `other_windows`).
///
/// Returns `None` when the border should be left at the system default.
#[must_use]
pub fn resolve_border_color<'a>(
  border_override: Option<&'a BorderColorOverride>,
  effect_config: &'a WindowEffectConfig,
  is_focused: bool,
) -> Option<&'a Color> {
  match border_override {
    Some(border_override) if is_focused => Some(&border_override.focused),
    Some(border_override) => Some(&border_override.unfocused),
    None if effect_config.border.enabled => {
      Some(&effect_config.border.color)
    }
    None => None,
  }
}

#[cfg(test)]
mod tests {
  use wm_platform::Color;

  use super::{resolve_border_color, BorderColorOverride};
  use crate::WindowEffectConfig;

  /// Builds an opaque color from its RGB components.
  fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color { r, g, b, a: 255 }
  }

  /// Builds a window effect config with the border effect set.
  fn effect_config(enabled: bool, color: Color) -> WindowEffectConfig {
    let mut config = WindowEffectConfig::default();
    config.border.enabled = enabled;
    config.border.color = color;
    config
  }

  #[test]
  fn override_wins_over_the_config() {
    let config = effect_config(true, rgb(1, 1, 1));
    let border_override =
      BorderColorOverride::new(rgb(2, 2, 2), Some(rgb(3, 3, 3)));

    assert_eq!(
      resolve_border_color(Some(&border_override), &config, true),
      Some(&rgb(2, 2, 2))
    );
    assert_eq!(
      resolve_border_color(Some(&border_override), &config, false),
      Some(&rgb(3, 3, 3))
    );
  }

  #[test]
  fn override_applies_when_the_config_border_is_disabled() {
    let config = effect_config(false, rgb(1, 1, 1));
    let border_override = BorderColorOverride::new(rgb(2, 2, 2), None);

    assert_eq!(
      resolve_border_color(Some(&border_override), &config, false),
      Some(&rgb(2, 2, 2))
    );
  }

  #[test]
  fn without_an_override_the_config_decides() {
    let color = rgb(1, 1, 1);

    assert_eq!(
      resolve_border_color(
        None,
        &effect_config(true, color.clone()),
        true
      ),
      Some(&color)
    );
    assert_eq!(
      resolve_border_color(None, &effect_config(false, color), true),
      None
    );
  }

  #[test]
  fn unfocused_color_defaults_to_the_focused_one() {
    let border_override = BorderColorOverride::new(rgb(2, 2, 2), None);

    assert_eq!(border_override.unfocused, rgb(2, 2, 2));
  }
}
