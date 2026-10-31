use std::{fmt, sync::Arc};

use serde::{Deserialize, Serialize};

/// Built-in easing curves, usable by name in the config.
#[derive(
  Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum EasingPreset {
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

impl EasingPreset {
  /// Every preset, for listing them (e.g. in config errors).
  pub const ALL: [EasingPreset; 8] = [
    Self::Linear,
    Self::EaseInOut,
    Self::EaseIn,
    Self::EaseOut,
    Self::EaseInOutCubic,
    Self::EaseInCubic,
    Self::EaseOutCubic,
    Self::EaseOutQuint,
  ];

  /// Applies the curve to a normalized time value (0.0 to 1.0).
  ///
  /// Returns the eased value, also in the range 0.0 to 1.0.
  #[must_use]
  pub fn apply(self, progress: f32) -> f32 {
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

  /// The preset's name in the config (e.g. `ease_out_quint`).
  #[must_use]
  pub fn name(self) -> &'static str {
    match self {
      Self::Linear => "linear",
      Self::EaseInOut => "ease_in_out",
      Self::EaseIn => "ease_in",
      Self::EaseOut => "ease_out",
      Self::EaseInOutCubic => "ease_in_out_cubic",
      Self::EaseInCubic => "ease_in_cubic",
      Self::EaseOutCubic => "ease_out_cubic",
      Self::EaseOutQuint => "ease_out_quint",
    }
  }

  /// Gets a preset by its config name.
  #[must_use]
  pub fn from_name(name: &str) -> Option<Self> {
    Self::ALL.into_iter().find(|preset| preset.name() == name)
  }
}

/// Number of samples in a [`CubicBezier`]'s baked curve.
const BAKED_SAMPLES: usize = 256;

/// A CSS-style cubic bezier easing curve, from `(0, 0)` to `(1, 1)` with
/// control points `(x1, y1)` and `(x2, y2)`, as in Hyprland's `bezier`.
///
/// The y values may leave [0, 1], for curves that overshoot their target
/// or pull back before starting. The x values must be within [0, 1], so
/// the curve is a function of time.
///
/// The curve is baked into a table of samples when created, so
/// evaluating it per frame is a lookup.
#[derive(Clone)]
pub struct CubicBezier {
  points: [f32; 4],
  baked: Arc<[f32; BAKED_SAMPLES]>,
}

impl CubicBezier {
  /// Creates a curve from its control points `[x1, y1, x2, y2]`.
  ///
  /// Fails if a value isn't finite or an x value is outside [0, 1].
  pub fn new(points: [f32; 4]) -> Result<Self, crate::ParseError> {
    let [x1, _, x2, _] = points;

    if points.iter().any(|value| !value.is_finite())
      || !(0.0..=1.0).contains(&x1)
      || !(0.0..=1.0).contains(&x2)
    {
      return Err(crate::ParseError::Bezier(format!("{points:?}")));
    }

    let curve = Polynomials::new(points);
    let mut baked = [0.0; BAKED_SAMPLES];

    for (index, sample) in baked.iter_mut().enumerate() {
      // LINT: Indices are tiny, exact as `f64`.
      #[allow(clippy::cast_precision_loss)]
      let x = index as f64 / (BAKED_SAMPLES - 1) as f64;

      // LINT: Curve values are small; `f32` precision is plenty.
      #[allow(clippy::cast_possible_truncation)]
      {
        *sample = curve.y_at(x) as f32;
      }
    }

    Ok(Self {
      points,
      baked: Arc::new(baked),
    })
  }

  /// The control points `[x1, y1, x2, y2]`.
  #[must_use]
  pub fn points(&self) -> [f32; 4] {
    self.points
  }

  /// Gets the curve's y value at time `x`, from the baked samples.
  ///
  /// `x` is clamped to [0, 1]. The ends are exact: 0 at 0, 1 at 1.
  #[must_use]
  pub fn y_at(&self, x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);

    // LINT: The position is within the table, a non-negative number far
    // below `usize::MAX`.
    #[allow(
      clippy::cast_possible_truncation,
      clippy::cast_sign_loss,
      clippy::cast_precision_loss
    )]
    let (index, fraction) = {
      let position = x * (BAKED_SAMPLES - 1) as f32;
      let index = (position.floor() as usize).min(BAKED_SAMPLES - 2);
      (index, position - index as f32)
    };

    let start = self.baked[index];
    let end = self.baked[index + 1];

    start + (end - start) * fraction
  }

  /// Gets the curve's y value at time `x` by solving the curve directly,
  /// without the baked samples.
  ///
  /// Slower than [`CubicBezier::y_at`]; for when exactness matters more
  /// than speed (e.g. fitting other curves to this one).
  #[must_use]
  pub fn y_at_exact(&self, x: f64) -> f64 {
    Polynomials::new(self.points).y_at(x.clamp(0.0, 1.0))
  }
}

impl fmt::Debug for CubicBezier {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "CubicBezier({:?})", self.points)
  }
}

impl PartialEq for CubicBezier {
  fn eq(&self, other: &Self) -> bool {
    self.points == other.points
  }
}

/// A cubic bezier's x and y as polynomials of the curve parameter `t`,
/// in `f64` for solving.
struct Polynomials {
  /// Coefficients `(a, b, c)` of `x(t) = ((a t + b) t + c) t`.
  x: (f64, f64, f64),

  /// Coefficients of `y(t)`, as for `x`.
  y: (f64, f64, f64),
}

impl Polynomials {
  /// Converts control points `[x1, y1, x2, y2]` to polynomial form.
  fn new(points: [f32; 4]) -> Self {
    let coefficients = |p1: f32, p2: f32| {
      let c = 3.0 * f64::from(p1);
      let b = 3.0 * (f64::from(p2) - f64::from(p1)) - c;
      (1.0 - c - b, b, c)
    };

    Self {
      x: coefficients(points[0], points[2]),
      y: coefficients(points[1], points[3]),
    }
  }

  /// Evaluates a polynomial `((a t + b) t + c) t`.
  fn eval((a, b, c): (f64, f64, f64), t: f64) -> f64 {
    ((a * t + b) * t + c) * t
  }

  /// Evaluates a polynomial's derivative.
  fn slope((a, b, c): (f64, f64, f64), t: f64) -> f64 {
    (3.0 * a * t + 2.0 * b) * t + c
  }

  /// Finds the curve parameter `t` at which the curve's x is `x`.
  ///
  /// Newton's method converges in a few steps for most curves; where the
  /// slope is too flat for it, bisection always finds `t`, since x rises
  /// steadily from 0 to 1 when its control points are in [0, 1].
  fn t_for_x(&self, x: f64) -> f64 {
    const EPSILON: f64 = 1e-9;

    let mut t = x;

    for _ in 0..8 {
      let error = Self::eval(self.x, t) - x;

      if error.abs() < EPSILON {
        return t;
      }

      let slope = Self::slope(self.x, t);

      if slope.abs() < 1e-6 {
        break;
      }

      t -= error / slope;
    }

    let (mut low, mut high) = (0.0, 1.0);
    t = x;

    for _ in 0..64 {
      let value = Self::eval(self.x, t);

      if (value - x).abs() < EPSILON {
        break;
      }

      if value < x {
        low = t;
      } else {
        high = t;
      }

      t = f64::midpoint(low, high);
    }

    t
  }

  /// Gets the curve's y at time `x` in [0, 1].
  fn y_at(&self, x: f64) -> f64 {
    Self::eval(self.y, self.t_for_x(x))
  }
}

/// An easing curve: a built-in preset or a custom cubic bezier.
#[derive(Clone, Debug, PartialEq)]
pub enum Easing {
  Preset(EasingPreset),
  Bezier(CubicBezier),
}

impl Easing {
  /// Applies the curve to a normalized time value (0.0 to 1.0).
  ///
  /// Starts at 0 and ends at 1; a bezier may leave [0, 1] in between.
  #[must_use]
  pub fn apply(&self, progress: f32) -> f32 {
    match self {
      Self::Preset(preset) => preset.apply(progress),
      Self::Bezier(bezier) => bezier.y_at(progress),
    }
  }
}

impl Default for Easing {
  fn default() -> Self {
    Self::Preset(EasingPreset::default())
  }
}

impl From<EasingPreset> for Easing {
  fn from(preset: EasingPreset) -> Self {
    Self::Preset(preset)
  }
}

#[cfg(test)]
mod tests {
  use super::{CubicBezier, Easing, EasingPreset};

  #[test]
  fn presets_start_at_zero_and_end_at_one() {
    for preset in EasingPreset::ALL {
      assert!(preset.apply(0.0).abs() < f32::EPSILON, "{preset:?}");
      assert!(
        (preset.apply(1.0) - 1.0).abs() < f32::EPSILON,
        "{preset:?}"
      );
    }
  }

  #[test]
  fn preset_names_round_trip() {
    for preset in EasingPreset::ALL {
      assert_eq!(EasingPreset::from_name(preset.name()), Some(preset));
    }
  }

  #[test]
  fn ease_out_quint_is_mostly_done_halfway() {
    assert!(EasingPreset::EaseOutQuint.apply(0.5) > 0.95);
  }

  fn bezier(points: [f32; 4]) -> CubicBezier {
    CubicBezier::new(points).expect("valid bezier")
  }

  #[test]
  fn bezier_ends_are_exact() {
    for points in [
      [0.0, 0.0, 1.0, 1.0],
      [0.25, 0.1, 0.25, 1.0],
      [0.05, 0.9, 0.1, 1.05],
      [0.36, 0.0, 0.66, -0.56],
    ] {
      let curve = bezier(points);
      assert!(curve.y_at(0.0).abs() < 1e-6, "{points:?}");
      assert!((curve.y_at(1.0) - 1.0).abs() < 1e-6, "{points:?}");
    }
  }

  #[test]
  fn linear_bezier_is_linear() {
    let curve = bezier([0.0, 0.0, 1.0, 1.0]);

    for step in 0..=20 {
      let x = step as f32 / 20.0;
      assert!((curve.y_at(x) - x).abs() < 1e-4, "at {x}");
    }
  }

  #[test]
  fn css_ease_matches_known_points() {
    // CSS `ease`, `cubic-bezier(0.25, 0.1, 0.25, 1)`, as browsers compute
    // it.
    let curve = bezier([0.25, 0.1, 0.25, 1.0]);

    for (x, y) in [(0.25, 0.4094), (0.5, 0.8024), (0.75, 0.9604)] {
      assert!((curve.y_at_exact(x) - y).abs() < 1e-3, "exact at {x}");
      // LINT: Test values, exact enough as `f32`.
      #[allow(clippy::cast_possible_truncation)]
      let baked = curve.y_at(x as f32);
      assert!((f64::from(baked) - y).abs() < 1e-3, "baked at {x}");
    }
  }

  #[test]
  fn baked_matches_exact() {
    let curve = bezier([0.05, 0.9, 0.1, 1.05]);

    for step in 0..=1000 {
      let x = f64::from(step) / 1000.0;
      // LINT: Test values, exact enough as `f32`.
      #[allow(clippy::cast_possible_truncation)]
      let baked = f64::from(curve.y_at(x as f32));
      assert!((baked - curve.y_at_exact(x)).abs() < 2e-3, "at {x}");
    }
  }

  #[test]
  fn overshoot_leaves_the_unit_range() {
    let overshoot = bezier([0.05, 0.9, 0.1, 1.05]);
    assert!((0..100).any(|step| overshoot.y_at(step as f32 / 100.0) > 1.0));

    let pull_back = bezier([0.36, 0.0, 0.66, -0.56]);
    assert!((0..100).any(|step| pull_back.y_at(step as f32 / 100.0) < 0.0));
  }

  #[test]
  fn steep_curves_still_solve() {
    // Flat x slope at the start, where Newton's method alone stalls.
    let curve = bezier([1.0, 0.0, 1.0, 1.0]);
    let y = curve.y_at_exact(0.5);
    assert!((0.0..=1.0).contains(&y));
    assert!(curve.y_at(0.5) <= 1.0);
  }

  #[test]
  fn invalid_beziers_are_rejected() {
    assert!(CubicBezier::new([1.5, 0.0, 0.5, 1.0]).is_err());
    assert!(CubicBezier::new([0.5, 0.0, -0.1, 1.0]).is_err());
    assert!(CubicBezier::new([0.5, f32::NAN, 0.5, 1.0]).is_err());
  }

  #[test]
  fn easing_applies_either_kind() {
    let preset = Easing::Preset(EasingPreset::Linear);
    let curve = Easing::Bezier(bezier([0.0, 0.0, 1.0, 1.0]));
    assert!((preset.apply(0.3) - curve.apply(0.3)).abs() < 1e-4);
  }
}
