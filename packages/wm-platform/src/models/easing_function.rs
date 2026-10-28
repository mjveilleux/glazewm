use serde::{Deserialize, Serialize};

/// Supported easing functions for animations.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EasingFunction {
  Linear,
  #[default]
  EaseInOut,
  EaseIn,
  EaseOut,
  EaseInOutCubic,
  EaseInCubic,
  EaseOutCubic,
  /// Strong ease-out; starts fast and settles gently. Close to the
  /// `cubic-bezier(0.23, 1, 0.32, 1)` curve that Omarchy uses for window
  /// motion in Hyprland.
  EaseOutQuint,
}

impl EasingFunction {
  /// Applies the easing function to a normalized time value (0.0 to 1.0).
  ///
  /// Returns the eased value, also in the range 0.0 to 1.0.
  #[must_use]
  pub fn apply(&self, progress: f32) -> f32 {
    match self {
      Self::Linear => progress,
      Self::EaseInOut => {
        if progress < 0.5 {
          2.0 * progress * progress
        } else {
          -1.0 + (4.0 - 2.0 * progress) * progress
        }
      }
      Self::EaseIn => progress * progress,
      Self::EaseOut => progress * (2.0 - progress),
      Self::EaseInOutCubic => {
        if progress < 0.5 {
          4.0 * progress * progress * progress
        } else {
          1.0 - (-2.0 * progress + 2.0).powi(3) / 2.0
        }
      }
      Self::EaseInCubic => progress * progress * progress,
      Self::EaseOutCubic => 1.0 - (1.0 - progress).powi(3),
      Self::EaseOutQuint => 1.0 - (1.0 - progress).powi(5),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::EasingFunction;

  #[test]
  fn easings_start_at_zero_and_end_at_one() {
    let easings = [
      EasingFunction::Linear,
      EasingFunction::EaseInOut,
      EasingFunction::EaseIn,
      EasingFunction::EaseOut,
      EasingFunction::EaseInOutCubic,
      EasingFunction::EaseInCubic,
      EasingFunction::EaseOutCubic,
      EasingFunction::EaseOutQuint,
    ];

    for easing in easings {
      assert!(easing.apply(0.0).abs() < f32::EPSILON, "{easing:?}");
      assert!(
        (easing.apply(1.0) - 1.0).abs() < f32::EPSILON,
        "{easing:?}"
      );
    }
  }

  #[test]
  fn ease_out_quint_is_mostly_done_halfway() {
    assert!(EasingFunction::EaseOutQuint.apply(0.5) > 0.95);
  }
}
