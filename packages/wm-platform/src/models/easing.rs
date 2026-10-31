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
    // LINT: Eased values are in [0, 1]; `f32` keeps plenty of precision.
    #[allow(clippy::cast_possible_truncation)]
    let eased = self.apply_f64(f64::from(progress)) as f32;
    eased
  }

  /// Like [`EasingPreset::apply`], in `f64`.
  #[must_use]
  pub fn apply_f64(self, progress: f64) -> f64 {
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

/// One piece of an animated value, as a cubic polynomial of time: from
/// `begin` seconds into the animation, the value is
/// `constant + linear s + quadratic s² + cubic s³`, `s` being the seconds
/// since `begin`. The form compositors take (DirectComposition's
/// `IDCompositionAnimation::AddCubic`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CubicSegment {
  pub begin: f64,
  pub constant: f32,
  pub linear: f32,
  pub quadratic: f32,
  pub cubic: f32,
}

impl CubicSegment {
  /// Gets the segment's value `seconds` into the animation.
  #[must_use]
  pub fn value_at(&self, seconds: f64) -> f64 {
    let s = seconds - self.begin;

    f64::from(self.constant)
      + s
        * (f64::from(self.linear)
          + s * (f64::from(self.quadratic) + s * f64::from(self.cubic)))
  }
}

impl Easing {
  /// Like [`Easing::apply`], in `f64` and without the baked samples: for
  /// when exactness matters more than speed.
  #[must_use]
  pub fn apply_exact(&self, progress: f64) -> f64 {
    match self {
      Self::Preset(preset) => preset.apply_f64(progress.clamp(0.0, 1.0)),
      Self::Bezier(bezier) => bezier.y_at_exact(progress),
    }
  }

  /// Approximates a value going from `from` to `to` along this curve
  /// over `duration_s` seconds, as cubic segments a compositor can play
  /// on its own clock.
  ///
  /// Uses as few segments as keep every point within `max_error` (in the
  /// value's own units, e.g. half a pixel for a position) of the curve,
  /// between 8 and 64. The value holds at `to` after the last segment.
  ///
  /// Returns no segments for a zero duration: the value is `to` at once.
  #[must_use]
  pub fn cubic_segments(
    &self,
    from: f32,
    to: f32,
    duration_s: f64,
    max_error: f32,
  ) -> Vec<CubicSegment> {
    const MIN_SEGMENTS: usize = 8;
    const MAX_SEGMENTS: usize = 64;

    if duration_s <= 0.0 || !duration_s.is_finite() {
      return Vec::new();
    }

    let (from, to) = (f64::from(from), f64::from(to));
    let value =
      |progress: f64| from + (to - from) * self.apply_exact(progress);

    // Slope in value per second, from a central difference (one-sided at
    // the ends).
    let slope = |progress: f64| {
      const STEP: f64 = 1e-5;
      let low = (progress - STEP).max(0.0);
      let high = (progress + STEP).min(1.0);
      (value(high) - value(low)) / ((high - low) * duration_s)
    };

    let mut count = MIN_SEGMENTS;

    loop {
      let segments = (0..count)
        .map(|index| {
          // LINT: Segment counts are tiny, exact as `f64`.
          #[allow(clippy::cast_precision_loss)]
          let (start, end) = (
            index as f64 / count as f64,
            (index + 1) as f64 / count as f64,
          );

          hermite_segment(
            start * duration_s,
            (end - start) * duration_s,
            (value(start), slope(start)),
            (value(end), slope(end)),
          )
        })
        .collect::<Vec<_>>();

      let fits = segments.iter().enumerate().all(|(index, segment)| {
        (1..8).all(|step| {
          // LINT: Small counts, exact as `f64`.
          #[allow(clippy::cast_precision_loss)]
          let progress =
            (index as f64 + f64::from(step) / 8.0) / count as f64;

          (segment.value_at(progress * duration_s) - value(progress)).abs()
            <= f64::from(max_error)
        })
      });

      if fits || count >= MAX_SEGMENTS {
        return segments;
      }

      count *= 2;
    }
  }
}

/// Builds the cubic through two points with given slopes (a Hermite
/// spline), over `length` seconds from `begin`.
fn hermite_segment(
  begin: f64,
  length: f64,
  (start, start_slope): (f64, f64),
  (end, end_slope): (f64, f64),
) -> CubicSegment {
  let quadratic = (3.0 * (end - start)
    - (2.0 * start_slope + end_slope) * length)
    / (length * length);
  let cubic = (2.0 * (start - end) + (start_slope + end_slope) * length)
    / (length * length * length);

  // LINT: Coefficients of on-screen values; `f32` is what compositors
  // take.
  #[allow(clippy::cast_possible_truncation)]
  CubicSegment {
    begin,
    constant: start as f32,
    linear: start_slope as f32,
    quadratic: quadratic as f32,
    cubic: cubic as f32,
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

    for step in 0..=20_u8 {
      let x = f32::from(step) / 20.0;
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
    assert!((0..100_u8)
      .any(|step| overshoot.y_at(f32::from(step) / 100.0) > 1.0));

    let pull_back = bezier([0.36, 0.0, 0.66, -0.56]);
    assert!((0..100_u8)
      .any(|step| pull_back.y_at(f32::from(step) / 100.0) < 0.0));
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

  /// Checks that segments follow the curve within `max_error`, start at
  /// 0 s and are in order.
  fn assert_segments_fit(
    easing: &Easing,
    from: f32,
    to: f32,
    max_error: f32,
  ) {
    let duration = 0.25;
    let segments = easing.cubic_segments(from, to, duration, max_error);

    assert!(segments.len() >= 8 && segments.len() <= 64);
    assert!(segments[0].begin.abs() < f64::EPSILON);
    assert!(segments
      .windows(2)
      .all(|pair| pair[0].begin < pair[1].begin));

    for step in 0..=500 {
      let seconds = duration * f64::from(step) / 500.0;
      let segment = segments
        .iter()
        .rev()
        .find(|segment| segment.begin <= seconds)
        .expect("a segment");

      let want = f64::from(from)
        + f64::from(to - from) * easing.apply_exact(seconds / duration);
      let got = segment.value_at(seconds);

      // f32 coefficients cost a little precision over the fit.
      assert!(
        (got - want).abs() <= f64::from(max_error) * 1.5,
        "{easing:?} at {seconds}: {got} vs {want}"
      );
    }
  }

  #[test]
  fn segments_fit_presets_and_beziers() {
    for preset in EasingPreset::ALL {
      assert_segments_fit(&Easing::Preset(preset), 100.0, 1500.0, 0.5);
    }

    for points in [[0.05, 0.9, 0.1, 1.05], [0.36, 0.0, 0.66, -0.56]] {
      assert_segments_fit(
        &Easing::Bezier(bezier(points)),
        -300.0,
        900.0,
        0.5,
      );
    }

    // Opacity, in its own units.
    assert_segments_fit(&Easing::default(), 0.0, 1.0, 0.002);
  }

  #[test]
  fn zero_duration_has_no_segments() {
    assert_eq!(
      Easing::default().cubic_segments(0.0, 1.0, 0.0, 0.5),
      vec![]
    );
  }

  #[test]
  fn easing_applies_either_kind() {
    let preset = Easing::Preset(EasingPreset::Linear);
    let curve = Easing::Bezier(bezier([0.0, 0.0, 1.0, 1.0]));
    assert!((preset.apply(0.3) - curve.apply(0.3)).abs() < 1e-4);
  }
}
