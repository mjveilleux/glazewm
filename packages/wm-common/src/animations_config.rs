use std::collections::BTreeMap;

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use wm_platform::{
  CubicBezier, Easing, EasingPreset, LengthUnit, LengthValue,
};

/// Animation settings, as a tree of nodes after Hyprland's: a child takes
/// every setting it leaves out from its parent.
///
/// ```text
/// windows ─┬─ windows_in      fade ─┬─ fade_in      workspaces ─┬─ workspaces_in
///          ├─ windows_out           └─ fade_out                 └─ workspaces_out
///          └─ windows_move    border
/// ```
///
/// A node animates once it, or its parent, is in the config (and neither
/// says `enabled: false`). Read with [`AnimationsConfig::resolve`], which
/// checks it and works out every node's settings once.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all(serialize = "camelCase"))]
pub struct AnimationsConfig {
  /// Whether any animation runs. Off keeps the settings below for when
  /// it's turned on again.
  pub enabled: bool,

  /// Named cubic bezier curves, `[x1, y1, x2, y2]`, usable as `easing`
  /// next to the built-in presets.
  pub beziers: BTreeMap<String, [f32; 4]>,

  pub windows: Option<AnimationNode>,
  /// New windows appearing.
  pub windows_in: Option<AnimationNode>,
  /// Windows closed with the `close` command leaving.
  pub windows_out: Option<AnimationNode>,
  /// Windows moving or resizing.
  pub windows_move: Option<AnimationNode>,

  /// Opacity of windows appearing and leaving. Without it, opacity
  /// follows the window's own animation.
  pub fade: Option<AnimationNode>,
  pub fade_in: Option<AnimationNode>,
  pub fade_out: Option<AnimationNode>,

  /// The focused window's border color changing.
  pub border: Option<AnimationNode>,

  /// Switching workspaces.
  pub workspaces: Option<AnimationNode>,
  pub workspaces_in: Option<AnimationNode>,
  pub workspaces_out: Option<AnimationNode>,

  /// Deprecated: `windows_move`.
  pub window_move: Option<WindowMoveAnimationConfig>,
  /// Deprecated: `windows_in`.
  pub window_open: Option<AnimationEffectConfig>,
  /// Deprecated: `windows_out`.
  pub window_close: Option<AnimationEffectConfig>,

  /// Every node's settings, worked out by [`AnimationsConfig::resolve`].
  #[serde(skip)]
  resolved: ResolvedAnimations,
}

impl Default for AnimationsConfig {
  fn default() -> Self {
    AnimationsConfig {
      enabled: true,
      beziers: BTreeMap::new(),
      windows: None,
      windows_in: None,
      windows_out: None,
      windows_move: None,
      fade: None,
      fade_in: None,
      fade_out: None,
      border: None,
      workspaces: None,
      workspaces_in: None,
      workspaces_out: None,
      window_move: None,
      window_open: None,
      window_close: None,
      resolved: ResolvedAnimations::default(),
    }
  }
}

/// One node of the animation tree. Every setting is optional: one left
/// out comes from the node's parent, or else its default.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields, rename_all(serialize = "camelCase"))]
pub struct AnimationNode {
  pub enabled: Option<bool>,
  pub duration_ms: Option<u32>,

  /// A preset (e.g. `ease_out_quint`) or a curve from `beziers`.
  pub easing: Option<String>,

  pub style: Option<AnimationEffect>,

  /// For `popin`: the size, in percent of the window's, that a window
  /// appearing grows from or a window leaving shrinks to.
  pub popin_percent: Option<f32>,

  /// For `windows_move`: the smallest move that animates, in pixels.
  pub trigger_threshold: Option<LengthValue>,
}

/// Duration and curve of an animation.
#[derive(Clone, Debug, PartialEq)]
pub struct AnimationTiming {
  pub duration_ms: u32,
  pub easing: Easing,
}

/// An animation's settings, with every inherited value filled in.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedEffect {
  pub style: AnimationEffect,

  /// Timing of the motion: position and size.
  pub timing: AnimationTiming,

  /// Timing of the opacity change, for windows appearing or leaving.
  /// `None` when fades are turned off: opacity changes at once.
  pub fade: Option<AnimationTiming>,

  /// For `popin`: the size a window appearing starts at, or a window
  /// leaving ends at, relative to its own (`1.0`).
  pub popin_scale: f32,
}

/// The move animation's settings, with every inherited value filled in.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedMove {
  pub effect: ResolvedEffect,

  /// The smallest move that animates, in pixels.
  pub trigger_threshold_px: i32,
}

/// Every animation's settings, worked out once when the config is read.
/// `None` for an animation that doesn't run.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResolvedAnimations {
  pub windows_in: Option<ResolvedEffect>,
  pub windows_out: Option<ResolvedEffect>,
  pub windows_move: Option<ResolvedMove>,
  pub border: Option<AnimationTiming>,
  pub workspaces_in: Option<ResolvedEffect>,
  pub workspaces_out: Option<ResolvedEffect>,

  /// Deprecated keys found in the config, to warn about.
  pub deprecated_keys: Vec<&'static str>,
}

/// Defaults for a node whose chain sets nothing.
const DEFAULT_DURATION_MS: u32 = 200;
const DEFAULT_THRESHOLD_PX: f32 = 10.0;

/// Size a window appearing with `popin` starts at, in percent
/// (Hyprland's `popin 87%`).
const DEFAULT_POPIN_IN_PERCENT: f32 = 87.0;

/// Size a window leaving with `popin` ends at, in percent (Omarchy's
/// `windowsOut`, `popin 95%`).
const DEFAULT_POPIN_OUT_PERCENT: f32 = 95.0;

impl AnimationsConfig {
  /// Every animation's settings, as worked out by
  /// [`AnimationsConfig::resolve`]. All off while `enabled` is false.
  #[must_use]
  pub fn resolved(&self) -> &ResolvedAnimations {
    &self.resolved
  }

  /// The open animation, when it runs.
  #[must_use]
  pub fn open_effect(&self) -> Option<&ResolvedEffect> {
    self.resolved.windows_in.as_ref()
  }

  /// The move animation, when it runs.
  #[must_use]
  pub fn move_effect(&self) -> Option<&ResolvedMove> {
    self.resolved.windows_move.as_ref()
  }

  /// The close animation, when it runs.
  #[must_use]
  pub fn close_effect(&self) -> Option<&ResolvedEffect> {
    self.resolved.windows_out.as_ref()
  }

  /// Checks the settings serde can't, and works out every node's
  /// settings (see [`AnimationsConfig::resolved`]).
  ///
  /// Checked whether or not animations are enabled, so turning them on
  /// never surfaces an old mistake.
  ///
  /// Errors name the key at fault (e.g. `animations.windows_in.easing`).
  pub fn resolve(&mut self) -> anyhow::Result<()> {
    let beziers = self.parse_beziers()?;
    let tree = self.tree_with_legacy_keys()?;

    let resolver = Resolver {
      beziers: &beziers,
      tree: &tree,
    };

    let windows_in = resolver.effect(
      "windows_in",
      Some("windows"),
      Some("fade_in"),
      AnimationEffect::Popin,
      DEFAULT_POPIN_IN_PERCENT,
    )?;

    let windows_out = resolver.effect(
      "windows_out",
      Some("windows"),
      Some("fade_out"),
      AnimationEffect::Popin,
      DEFAULT_POPIN_OUT_PERCENT,
    )?;

    if let Some(out) = &windows_out {
      if !matches!(
        out.style,
        AnimationEffect::Fade | AnimationEffect::Popin
      ) {
        bail!("`animations.windows_out.style` must be 'fade' or 'popin'.");
      }
    }

    let windows_move = resolver.movement()?;

    let border = resolver
      .timing("border", None)?
      .filter(|_| resolver.is_enabled("border", None));

    let workspaces_in = resolver.effect(
      "workspaces_in",
      Some("workspaces"),
      None,
      AnimationEffect::Slide,
      100.0,
    )?;

    let workspaces_out = resolver.effect(
      "workspaces_out",
      Some("workspaces"),
      None,
      AnimationEffect::Slide,
      100.0,
    )?;

    let mut deprecated_keys = Vec::new();

    for (key, used) in [
      ("window_open", self.window_open.is_some()),
      ("window_close", self.window_close.is_some()),
      ("window_move", self.window_move.is_some()),
    ] {
      if used {
        deprecated_keys.push(key);
      }
    }

    self.resolved = if self.enabled {
      ResolvedAnimations {
        windows_in,
        windows_out,
        windows_move,
        border,
        workspaces_in,
        workspaces_out,
        deprecated_keys,
      }
    } else {
      ResolvedAnimations {
        deprecated_keys,
        ..ResolvedAnimations::default()
      }
    };

    Ok(())
  }

  /// Checks and bakes the named bezier curves.
  fn parse_beziers(&self) -> anyhow::Result<BTreeMap<String, Easing>> {
    self
      .beziers
      .iter()
      .map(|(name, points)| {
        if EasingPreset::from_name(name).is_some() {
          bail!(
            "`animations.beziers.{name}`: '{name}' is a built-in easing; \
             pick another name."
          );
        }

        let curve = CubicBezier::new(*points)
          .with_context(|| format!("`animations.beziers.{name}`"))?;

        Ok((name.clone(), Easing::Bezier(curve)))
      })
      .collect()
  }

  /// The tree's nodes by key, with the deprecated keys mapped in where
  /// their node isn't set.
  fn tree_with_legacy_keys(
    &self,
  ) -> anyhow::Result<BTreeMap<&'static str, AnimationNode>> {
    let mut tree = BTreeMap::new();

    for (key, node) in [
      ("windows", &self.windows),
      ("windows_in", &self.windows_in),
      ("windows_out", &self.windows_out),
      ("windows_move", &self.windows_move),
      ("fade", &self.fade),
      ("fade_in", &self.fade_in),
      ("fade_out", &self.fade_out),
      ("border", &self.border),
      ("workspaces", &self.workspaces),
      ("workspaces_in", &self.workspaces_in),
      ("workspaces_out", &self.workspaces_out),
    ] {
      if let Some(node) = node {
        if node.popin_percent.is_some_and(|percent| {
          !percent.is_finite() || percent <= 0.0 || percent > 100.0
        }) {
          bail!(
            "`animations.{key}.popin_percent` must be above 0 and at most \
             100."
          );
        }

        tree.insert(key, node.clone());
      }
    }

    for (key, legacy, new_key) in [
      ("window_open", self.window_open.as_ref(), "windows_in"),
      ("window_close", self.window_close.as_ref(), "windows_out"),
    ] {
      if let Some(legacy) = legacy {
        if tree.contains_key(new_key) {
          bail!(
            "`animations.{key}` and `animations.{new_key}` are both set; \
             keep `{new_key}` only."
          );
        }

        tree.insert(new_key, legacy.to_node(None));
      }
    }

    if let Some(legacy) = &self.window_move {
      if tree.contains_key("windows_move") {
        bail!(
          "`animations.window_move` and `animations.windows_move` are both \
           set; keep `windows_move` only."
        );
      }

      tree.insert(
        "windows_move",
        legacy
          .effect
          .to_node(Some(legacy.trigger_threshold.clone())),
      );
    }

    Ok(tree)
  }
}

/// Works out nodes' settings from the tree.
struct Resolver<'a> {
  beziers: &'a BTreeMap<String, Easing>,
  tree: &'a BTreeMap<&'static str, AnimationNode>,
}

impl Resolver<'_> {
  /// Gets a setting from the node, or else from its parent.
  fn setting<T>(
    &self,
    key: &str,
    parent: Option<&str>,
    get: impl Fn(&AnimationNode) -> Option<T>,
  ) -> Option<T> {
    self.tree.get(key).and_then(&get).or_else(|| {
      parent
        .and_then(|parent| self.tree.get(parent))
        .and_then(&get)
    })
  }

  /// Whether the node animates: it or its parent is in the config, and
  /// neither turns it off (the node's own `enabled` wins).
  fn is_enabled(&self, key: &str, parent: Option<&str>) -> bool {
    let present = self.tree.contains_key(key)
      || parent.is_some_and(|parent| self.tree.contains_key(parent));

    present
      && self
        .setting(key, parent, |node| node.enabled)
        .unwrap_or(true)
  }

  /// Gets the node's timing, or `None` if neither it nor its parent is in
  /// the config.
  fn timing(
    &self,
    key: &str,
    parent: Option<&str>,
  ) -> anyhow::Result<Option<AnimationTiming>> {
    let present = self.tree.contains_key(key)
      || parent.is_some_and(|parent| self.tree.contains_key(parent));

    if !present {
      return Ok(None);
    }

    let duration_ms = self
      .setting(key, parent, |node| node.duration_ms)
      .unwrap_or(DEFAULT_DURATION_MS);

    // Named on the node that set it, for the error.
    let easing =
      match self.tree.get(key).and_then(|node| node.easing.clone()) {
        Some(name) => self.easing(key, &name)?,
        None => match parent.and_then(|parent| {
          self
            .tree
            .get(parent)
            .and_then(|node| node.easing.clone())
            .map(|name| (parent, name))
        }) {
          Some((parent, name)) => self.easing(parent, &name)?,
          None => Easing::default(),
        },
      };

    Ok(Some(AnimationTiming {
      duration_ms,
      easing,
    }))
  }

  /// Looks up an easing by name: a bezier from the config, or a preset.
  fn easing(&self, key: &str, name: &str) -> anyhow::Result<Easing> {
    if let Some(easing) = self.beziers.get(name) {
      return Ok(easing.clone());
    }

    if let Some(preset) = EasingPreset::from_name(name) {
      return Ok(Easing::Preset(preset));
    }

    let presets = EasingPreset::ALL
      .iter()
      .map(|preset| preset.name())
      .collect::<Vec<_>>()
      .join("', '");

    bail!(
      "`animations.{key}.easing`: unknown curve '{name}'. Use a curve from \
       `animations.beziers` or one of '{presets}'."
    )
  }

  /// Works out the `windows_move` node's settings, or `None` if moves
  /// don't animate.
  fn movement(&self) -> anyhow::Result<Option<ResolvedMove>> {
    let Some(effect) = self.effect(
      "windows_move",
      Some("windows"),
      Some("fade_in"),
      AnimationEffect::Slide,
      DEFAULT_POPIN_IN_PERCENT,
    )?
    else {
      return Ok(None);
    };

    let threshold = self
      .setting("windows_move", Some("windows"), |node| {
        node.trigger_threshold.clone()
      })
      .unwrap_or(LengthValue {
        amount: DEFAULT_THRESHOLD_PX,
        unit: LengthUnit::Pixel,
      });

    if threshold.unit != LengthUnit::Pixel {
      bail!(
        "`animations.windows_move.trigger_threshold` must be in pixels \
         (e.g. '10px')."
      );
    }

    // LINT: A threshold in pixels is far below `i32::MAX`.
    #[allow(clippy::cast_possible_truncation)]
    Ok(Some(ResolvedMove {
      effect,
      trigger_threshold_px: threshold.amount.round() as i32,
    }))
  }

  /// Works out an in, out or move node's settings, or `None` if it
  /// doesn't animate.
  fn effect(
    &self,
    key: &str,
    parent: Option<&str>,
    fade_key: Option<&str>,
    default_style: AnimationEffect,
    default_popin_percent: f32,
  ) -> anyhow::Result<Option<ResolvedEffect>> {
    let Some(timing) = self.timing(key, parent)? else {
      return Ok(None);
    };

    if !self.is_enabled(key, parent) {
      return Ok(None);
    }

    // Opacity follows the fade node when there is one, else the motion.
    let fade = match fade_key {
      Some(fade_key)
        if self.tree.contains_key(fade_key)
          || self.tree.contains_key("fade") =>
      {
        if self.is_enabled(fade_key, Some("fade")) {
          self.timing(fade_key, Some("fade"))?
        } else {
          None
        }
      }
      _ => Some(timing.clone()),
    };

    let popin_percent = self
      .setting(key, parent, |node| node.popin_percent)
      .unwrap_or(default_popin_percent);

    Ok(Some(ResolvedEffect {
      style: self
        .setting(key, parent, |node| node.style.clone())
        .unwrap_or(default_style),
      timing,
      fade,
      popin_scale: popin_percent / 100.0,
    }))
  }
}

/// Deprecated: settings of `window_move`. Use `windows_move`.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all(serialize = "camelCase"))]
pub struct WindowMoveAnimationConfig {
  /// Minimum distance to trigger movement animations.
  pub trigger_threshold: LengthValue,

  #[serde(flatten)]
  pub effect: AnimationEffectConfig,
}

impl Default for WindowMoveAnimationConfig {
  fn default() -> Self {
    WindowMoveAnimationConfig {
      trigger_threshold: LengthValue::from_px(10),
      effect: AnimationEffectConfig::default(),
    }
  }
}

/// Deprecated: settings of `window_open` and `window_close`. Use
/// `windows_in` and `windows_out`.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all(serialize = "camelCase"))]
pub struct AnimationEffectConfig {
  pub effect_type: AnimationEffect,
  pub duration_ms: u32,
  pub easing: EasingPreset,
}

impl AnimationEffectConfig {
  /// The node of the animation tree with the same settings.
  fn to_node(
    &self,
    trigger_threshold: Option<LengthValue>,
  ) -> AnimationNode {
    AnimationNode {
      enabled: None,
      duration_ms: Some(self.duration_ms),
      easing: Some(self.easing.name().to_string()),
      style: Some(self.effect_type.clone()),
      popin_percent: None,
      trigger_threshold,
    }
  }
}

impl Default for AnimationEffectConfig {
  fn default() -> Self {
    AnimationEffectConfig {
      effect_type: AnimationEffect::default(),
      duration_ms: DEFAULT_DURATION_MS,
      easing: EasingPreset::default(),
    }
  }
}

/// Style of an animation.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnimationEffect {
  /// A snapshot of the window moves and scales from its old position to
  /// its new one.
  #[default]
  Slide,

  /// The window is placed at its new position while invisible, then
  /// fades in.
  ///
  /// # Platform-specific
  ///
  /// - **macOS**: Not supported; the window moves without animation.
  Fade,

  /// The window itself, live, moves and resizes a little each frame
  /// until it's in place. Smoother content, choppier motion than `slide`.
  ///
  /// # Platform-specific
  ///
  /// - **macOS**: Not supported; the window moves without animation.
  Move,

  /// A window appearing grows to its size from `popin_percent` while
  /// fading in; one leaving shrinks to it while fading out.
  ///
  /// # Platform-specific
  ///
  /// - **macOS**: Not supported; the window moves without animation.
  Popin,
}

#[cfg(test)]
mod tests {
  use wm_platform::{Easing, EasingPreset};

  use super::{AnimationEffect, AnimationsConfig};

  /// Parses and resolves an `animations` section from JSON (YAML maps the
  /// same way).
  fn resolve(json: &str) -> anyhow::Result<AnimationsConfig> {
    let mut config: AnimationsConfig =
      serde_json::from_str(json).map_err(anyhow::Error::from)?;
    config.resolve()?;
    Ok(config)
  }

  fn ok(json: &str) -> AnimationsConfig {
    resolve(json).expect("config should resolve")
  }

  fn error(json: &str) -> String {
    resolve(json).expect_err("config should fail").to_string()
  }

  #[test]
  fn nothing_animates_by_default() {
    let config = ok("{}");

    assert!(config.enabled);
    assert!(config.open_effect().is_none());
    assert!(config.move_effect().is_none());
    assert!(config.close_effect().is_none());
    assert!(config.resolved().border.is_none());
  }

  #[test]
  fn children_inherit_from_their_parent() {
    let config = ok(
      r#"{
        "windows": { "duration_ms": 300, "easing": "linear" },
        "windows_in": { "duration_ms": 120 }
      }"#,
    );

    let open = config.open_effect().expect("windows_in");
    assert_eq!(open.timing.duration_ms, 120);
    assert_eq!(open.timing.easing, Easing::Preset(EasingPreset::Linear));
    assert_eq!(open.style, AnimationEffect::Popin);

    let close = config.close_effect().expect("windows_out");
    assert_eq!(close.timing.duration_ms, 300);
    assert!((close.popin_scale - 0.95).abs() < f32::EPSILON);

    let movement = config.move_effect().expect("windows_move");
    assert_eq!(movement.effect.style, AnimationEffect::Slide);
    assert_eq!(movement.trigger_threshold_px, 10);
  }

  #[test]
  fn a_child_can_turn_itself_off_or_back_on() {
    let config = ok(
      r#"{ "windows": { "enabled": false }, "windows_in": { "enabled": true } }"#,
    );

    assert!(config.open_effect().is_some());
    assert!(config.close_effect().is_none());
    assert!(config.move_effect().is_none());
  }

  #[test]
  fn named_beziers_are_usable_and_may_overshoot() {
    let config = ok(
      r#"{
        "beziers": { "snappy": [0.05, 0.9, 0.1, 1.05] },
        "windows_move": { "easing": "snappy" }
      }"#,
    );

    let easing = &config.move_effect().expect("move").effect.timing.easing;
    assert!(matches!(easing, Easing::Bezier(_)));
    assert!((0..100).any(|step| easing.apply(step as f32 / 100.0) > 1.0));
  }

  #[test]
  fn unknown_curve_names_its_key() {
    let message = error(r#"{ "windows": { "easing": "snapy" } }"#);
    assert!(message.contains("animations.windows.easing"), "{message}");
    assert!(message.contains("snapy"), "{message}");
  }

  #[test]
  fn bad_beziers_are_errors() {
    let message = error(r#"{ "beziers": { "bad": [1.5, 0, 0.5, 1] } }"#);
    assert!(message.contains("animations.beziers.bad"), "{message}");

    let message = error(r#"{ "beziers": { "linear": [0, 0, 1, 1] } }"#);
    assert!(message.contains("built-in"), "{message}");
  }

  #[test]
  fn opacity_follows_the_fade_node() {
    let own = ok(r#"{ "windows_in": { "duration_ms": 250 } }"#);
    let fade = own.open_effect().and_then(|e| e.fade.clone());
    assert_eq!(fade.map(|t| t.duration_ms), Some(250));

    let config = ok(
      r#"{ "windows_in": {}, "windows_out": {}, "fade": { "duration_ms": 90 }, "fade_out": { "enabled": false } }"#,
    );
    let fade_in = config.open_effect().and_then(|e| e.fade.clone());
    assert_eq!(fade_in.map(|t| t.duration_ms), Some(90));
    assert!(config.close_effect().expect("out").fade.is_none());
  }

  #[test]
  fn old_keys_still_load() {
    let config = ok(
      r#"{
        "window_open": { "effect_type": "fade", "duration_ms": 150, "easing": "ease_out_cubic" },
        "window_close": { "effect_type": "popin", "duration_ms": 160 },
        "window_move": { "effect_type": "slide", "duration_ms": 180, "trigger_threshold": "20px" }
      }"#,
    );

    let open = config.open_effect().expect("open");
    assert_eq!(open.style, AnimationEffect::Fade);
    assert_eq!(open.timing.duration_ms, 150);
    assert_eq!(
      open.timing.easing,
      Easing::Preset(EasingPreset::EaseOutCubic)
    );

    assert_eq!(
      config.close_effect().expect("close").timing.duration_ms,
      160
    );
    assert_eq!(
      config.move_effect().expect("move").trigger_threshold_px,
      20
    );
    assert_eq!(
      config.resolved().deprecated_keys,
      vec!["window_open", "window_close", "window_move"]
    );
  }

  #[test]
  fn old_and_new_key_together_is_an_error() {
    let message = error(r#"{ "window_open": {}, "windows_in": {} }"#);
    assert!(message.contains("windows_in"), "{message}");
  }

  #[test]
  fn disabled_turns_everything_off_but_keeps_settings() {
    let config =
      ok(r#"{ "enabled": false, "windows": {}, "window_open": {} }"#);

    assert!(config.open_effect().is_none());
    assert!(config.move_effect().is_none());
    assert!(config.windows.is_some());
    assert!(config.window_open.is_some());
  }

  #[test]
  fn mistakes_are_caught_even_while_disabled() {
    let message = error(
      r#"{ "enabled": false, "windows_move": { "easing": "nope" } }"#,
    );
    assert!(message.contains("windows_move"), "{message}");
  }

  #[test]
  fn threshold_must_be_in_pixels() {
    ok(r#"{ "windows_move": { "trigger_threshold": "10px" } }"#);
    error(r#"{ "windows_move": { "trigger_threshold": "10%" } }"#);
    error(r#"{ "window_move": { "trigger_threshold": "10%" } }"#);
  }

  #[test]
  fn close_only_fades_or_pops() {
    ok(r#"{ "windows_out": { "style": "fade" } }"#);
    ok(r#"{ "window_close": { "effect_type": "popin" } }"#);
    error(r#"{ "windows_out": { "style": "slide" } }"#);
  }

  #[test]
  fn popin_percent_is_checked() {
    let config = ok(r#"{ "windows_in": { "popin_percent": 80 } }"#);
    let open = config.open_effect().expect("open");
    assert!((open.popin_scale - 0.8).abs() < f32::EPSILON);

    error(r#"{ "windows_in": { "popin_percent": 0 } }"#);
    error(r#"{ "windows_in": { "popin_percent": 120 } }"#);
  }

  #[test]
  fn unknown_node_settings_are_errors() {
    error(r#"{ "windows": { "durration_ms": 100 } }"#);
  }
}
