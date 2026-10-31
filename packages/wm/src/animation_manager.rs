use std::{
  collections::{HashMap, HashSet},
  time::{Duration, Instant},
};

use anyhow::Context;
use tokio::sync::mpsc;
use uuid::Uuid;
use wm_common::{AnimationEffect, AnimationEffectConfig};
#[cfg(target_os = "macos")]
use wm_platform::DispatcherExtMacOs;
#[cfg(target_os = "windows")]
use wm_platform::NativeWindowWindowsExt;
use wm_platform::{
  AnimationContext, AnimationWindow, AnimationWindowRequest, Dispatcher,
  EasingFunction, NativeWindow, OpacityValue, Rect,
};

use crate::{
  models::{NativeMonitorProperties, WindowContainer},
  traits::{CommonGetters, WindowGetters},
  user_config::UserConfig,
};

/// Whether new windows should be hidden the moment they're shown, so they
/// never appear where their app put them before fading in at their place.
///
/// Only with cloaking as the hide method: with the other methods, the WM
/// itself shows windows it manages, which would hide them too.
#[cfg(target_os = "windows")]
pub fn hides_new_windows(config: &UserConfig) -> bool {
  config.value.general.hide_method == wm_common::HideMethod::Cloak
    && config.value.animations.open_effect().is_some_and(|open| {
      matches!(
        open.effect_type,
        AnimationEffect::Fade | AnimationEffect::Popin
      )
    })
}

/// Size a pop-in starts at, relative to the window's (Hyprland's
/// `popin 87%`).
pub const POPIN_START_SCALE: f32 = 0.87;

/// Size a closing window's pop-out ends at, relative to the window's
/// (Omarchy's `windowsOut`, `popin 95%`).
// LINT: Close animations are only started on Windows.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub const CLOSE_END_SCALE: f32 = 0.95;

pub enum AnimationTrigger {
  WindowOpened,
  WindowMoved,
}

/// How a window should get to its new position.
#[derive(Clone, Debug)]
pub enum AnimationPlan {
  /// A snapshot of the window slides from its old position to the new
  /// one, while the real window waits invisible at the new position.
  Slide(AnimationEffectConfig),

  /// The real window is placed at the new position while invisible, then
  /// fades in.
  FadeIn(AnimationEffectConfig),

  /// The real window itself moves to the new position a little each
  /// frame. With `popin`, it grows there from smaller while fading in.
  Live {
    effect: AnimationEffectConfig,
    popin: bool,
  },
}

/// A window to start a live animation for.
// LINT: Live animations are only started on Windows.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub struct LiveRequest {
  pub window: WindowContainer,
  pub effect: AnimationEffectConfig,
  /// Where the window is when it starts, as its native frame.
  pub start_frame: Rect,
  /// Where it ends, as its native frame.
  pub target_frame: Rect,
  /// Its target in the layout, to compare later targets with.
  pub target_rect: Rect,
  /// Opacity to fade in to, for a pop-in.
  pub fade_to: Option<OpacityValue>,
  pub frame_rate: u32,
}

/// A window to start a slide animation for.
pub struct SlideRequest {
  pub window: WindowContainer,
  pub effect: AnimationEffectConfig,
  pub target_rect: Rect,
  pub frame_rate: u32,
}

/// Timing shared by every kind of animation.
#[derive(Clone, Debug)]
struct AnimationClock {
  start_time: Instant,
  duration: Duration,
  easing: EasingFunction,

  /// Target frame rate for the animation.
  frame_rate: u32,
}

impl AnimationClock {
  fn new(config: &AnimationEffectConfig, frame_rate: u32) -> Self {
    Self {
      start_time: Instant::now(),
      duration: Duration::from_millis(u64::from(config.duration_ms)),
      easing: config.easing.clone(),
      frame_rate,
    }
  }

  /// Returns the normalized animation progress in `[0.0, 1.0]`.
  fn progress(&self) -> f32 {
    let elapsed = self.start_time.elapsed();

    if elapsed >= self.duration {
      1.0
    } else {
      (elapsed.as_secs_f32() / self.duration.as_secs_f32()).clamp(0.0, 1.0)
    }
  }

  /// Returns the eased animation progress in `[0.0, 1.0]`.
  fn eased_progress(&self) -> f32 {
    self.easing.apply(self.progress())
  }

  /// Whether the animation has completed.
  // LINT: Progress is clamped to [0.0, 1.0], so exact comparison is safe.
  #[allow(clippy::float_cmp)]
  fn is_complete(&self) -> bool {
    self.progress() == 1.0
  }
}

/// State of a slide animation, drawn by an `AnimationWindow` overlay.
#[derive(Clone, Debug)]
struct SlideState {
  clock: AnimationClock,

  /// Start and target positions for the animation.
  start_rect: Rect,
  target_rect: Rect,
}

impl SlideState {
  /// Returns the interpolated rect at the current animation progress.
  fn current_rect(&self) -> Rect {
    self
      .start_rect
      .interpolate(&self.target_rect, self.clock.eased_progress())
  }
}

/// State of a fade-in animation, applied to the real window's opacity.
// LINT: Fades are only started on Windows.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
#[derive(Clone, Debug)]
struct FadeState {
  clock: AnimationClock,
  window: NativeWindow,

  /// Opacity the window ends at (its transparency effect, if any).
  target_opacity: OpacityValue,
}

// LINT: Fades are only started on Windows.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
impl FadeState {
  /// Returns the interpolated opacity at the current animation progress.
  fn current_opacity(&self) -> OpacityValue {
    OpacityValue(0.0)
      .interpolate(&self.target_opacity, self.clock.eased_progress())
  }
}

/// State of a live animation: the real window's frame (and for a pop-in,
/// its opacity) set each frame.
// LINT: Live animations are only started on Windows.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
#[derive(Clone, Debug)]
struct LiveState {
  clock: AnimationClock,
  window: NativeWindow,
  start_frame: Rect,
  target_frame: Rect,
  target_rect: Rect,
  fade_to: Option<OpacityValue>,
}

// LINT: Live animations are only started on Windows.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
impl LiveState {
  /// Returns the window's frame at the current animation progress.
  fn current_frame(&self) -> Rect {
    self
      .start_frame
      .interpolate(&self.target_frame, self.clock.eased_progress())
  }
}

/// State of a close animation: a snapshot of a closed window fading out
/// (and, for `popin`, shrinking) where the window was.
// LINT: Close animations are only started on Windows.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
#[derive(Clone, Debug)]
struct CloseState {
  clock: AnimationClock,

  /// Where the window was when it was closed.
  rect: Rect,

  /// Scale the snapshot ends at (1.0: fade only).
  end_scale: f32,
}

// LINT: Close animations are only started on Windows.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
impl CloseState {
  /// Returns the snapshot's opacity and scale at the current progress.
  fn current(&self) -> (OpacityValue, f32) {
    let progress = self.clock.eased_progress();

    (
      OpacityValue(1.0 - progress),
      1.0 + (self.end_scale - 1.0) * progress,
    )
  }
}

/// A running close animation and the overlay that draws it.
struct Closing {
  state: CloseState,
  overlay: AnimationWindow,
}

/// Manages animations for all windows.
///
/// A window has at most one animation at a time: a slide, a fade or a
/// live one.
pub struct AnimationManager {
  /// Active slide animations keyed by window ID.
  slides: HashMap<Uuid, SlideState>,

  /// Active live animations keyed by window ID.
  lives: HashMap<Uuid, LiveState>,

  /// Active fade-in animations keyed by window ID.
  fades: HashMap<Uuid, FadeState>,

  /// Windows whose real opacity an animation has set to 0. They get their
  /// opacity back once no animation is running for them.
  hidden: HashSet<Uuid>,

  /// Sender for animation tick events.
  tick_tx: mpsc::UnboundedSender<()>,

  /// Receiver for animation tick events.
  pub tick_rx: mpsc::UnboundedReceiver<()>,

  /// Per-window overlay windows for slides, keyed by window ID.
  windows: HashMap<Uuid, AnimationWindow>,

  /// Close animations of windows already gone. Kept apart from the
  /// per-window maps: the window is unmanaged while its snapshot still
  /// fades, so nothing done to a window ID touches them.
  closes: Vec<Closing>,

  /// Shared GPU context for animation overlay windows. Lazily
  /// initialized on the first slide.
  context: Option<AnimationContext>,

  /// Handle to the running tick task, if any.
  tick_task: Option<tokio::task::JoinHandle<()>>,

  /// Frame rate of the running tick task, if any.
  tick_frame_rate: Option<u32>,

  /// Whether "Displays have separate Spaces" setting is enabled.
  #[cfg(target_os = "macos")]
  displays_have_separate_spaces: bool,
}

impl AnimationManager {
  pub fn new(
    // LINT: `dispatcher` is only used on macOS.
    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
    dispatcher: &Dispatcher,
  ) -> Self {
    let (tick_tx, tick_rx) = mpsc::unbounded_channel();

    Self {
      slides: HashMap::new(),
      lives: HashMap::new(),
      fades: HashMap::new(),
      hidden: HashSet::new(),
      tick_tx,
      tick_rx,
      windows: HashMap::new(),
      closes: Vec::new(),
      context: None,
      tick_task: None,
      tick_frame_rate: None,
      #[cfg(target_os = "macos")]
      displays_have_separate_spaces: dispatcher
        .displays_have_separate_spaces(),
    }
  }

  /// Whether an animation is currently active for a given window.
  pub fn is_animating(&self, window_id: &Uuid) -> bool {
    self.slides.contains_key(window_id)
      || self.fades.contains_key(window_id)
      || self.lives.contains_key(window_id)
  }

  /// Whether a live animation is moving the window. Its moves are then
  /// the WM's own.
  pub fn is_live(&self, window_id: &Uuid) -> bool {
    self.lives.contains_key(window_id)
  }

  /// Where a live animation has the window now, as its native frame.
  pub fn live_frame(&self, window_id: &Uuid) -> Option<Rect> {
    self.lives.get(window_id).map(LiveState::current_frame)
  }

  /// Whether an animation has set the window's real opacity to 0.
  pub fn is_hidden(&self, window_id: &Uuid) -> bool {
    self.hidden.contains(window_id)
  }

  /// Records that an animation has set the window's real opacity to 0.
  pub fn mark_hidden(&mut self, window_id: Uuid) {
    self.hidden.insert(window_id);
  }

  /// Clears the hidden mark of a window that no longer animates.
  ///
  /// Returns whether the window's opacity should now be restored.
  pub fn take_hidden(&mut self, window_id: &Uuid) -> bool {
    !self.is_animating(window_id) && self.hidden.remove(window_id)
  }

  /// Gets the window IDs of slides that have completed.
  ///
  /// Live animations count as slides: both end with a redraw.
  pub fn completed_slide_ids(&self) -> HashSet<Uuid> {
    self
      .slides
      .iter()
      .filter(|(_, slide)| slide.clock.is_complete())
      .map(|(id, _)| *id)
      .chain(
        self
          .lives
          .iter()
          .filter(|(_, live)| live.clock.is_complete())
          .map(|(id, _)| *id),
      )
      .collect()
  }

  /// Removes fades that have completed and returns their window IDs.
  ///
  /// The windows stay marked hidden until their opacity is restored.
  pub fn take_completed_fade_ids(&mut self) -> HashSet<Uuid> {
    let completed = self
      .fades
      .iter()
      .filter(|(_, fade)| fade.clock.is_complete())
      .map(|(id, _)| *id)
      .collect::<HashSet<_>>();

    if !completed.is_empty() {
      self.fades.retain(|id, _| !completed.contains(id));
      self.update_tick_rate();
    }

    completed
  }

  /// Stops any animation of the window and destroys its overlay.
  ///
  /// The hidden mark is kept, so the window's opacity is restored on its
  /// next redraw.
  pub fn cancel_animation(&mut self, window_id: &Uuid) {
    let had_fade = self.fades.remove(window_id).is_some();
    let had_slide = self.slides.remove(window_id).is_some();
    let had_live = self.lives.remove(window_id).is_some();

    if had_fade || had_slide || had_live {
      self.update_tick_rate();
    }

    if let Some(anim_window) = self.windows.remove(window_id) {
      std::thread::spawn(|| {
        // Briefly keep animation windows up to hide flicker during sync.
        std::thread::sleep(Duration::from_millis(20));
        if let Err(err) = anim_window.destroy() {
          tracing::warn!("Failed to destroy animation window: {err}");
        }
      });
    }
  }

  /// Stops any animation of the window and forgets it entirely.
  ///
  /// Used when the window is no longer managed. A window that is merely
  /// hidden (e.g. to the tray) may be shown again later, so it's made
  /// opaque if an animation had hidden it.
  pub fn destroy_animation(&mut self, window: &WindowContainer) {
    self.cancel_animation(&window.id());

    if self.hidden.remove(&window.id()) {
      #[cfg(target_os = "windows")]
      if let Err(err) = window
        .native()
        .set_transparency(&OpacityValue::from_alpha(u8::MAX))
      {
        tracing::debug!("Failed to restore opacity: {err}");
      }
    }
  }

  /// Updates all active animations during a single tick.
  ///
  /// Slide updates get batched into a single compositor transaction.
  pub fn tick_update(
    &mut self,
    dispatcher: &Dispatcher,
  ) -> anyhow::Result<()> {
    // Fades only change the real window's opacity, which is cheap and
    // needs no compositor transaction.
    #[cfg(target_os = "windows")]
    for fade in self.fades.values() {
      if let Err(err) =
        fade.window.set_transparency(&fade.current_opacity())
      {
        tracing::warn!("Failed to update fade animation: {err}");
      }
    }

    // Live animations move the real windows. A window that won't move
    // just stays where it is until the animation ends.
    #[cfg(target_os = "windows")]
    for live in self.lives.values() {
      if let Err(err) = live.window.set_frame(&live.current_frame()) {
        tracing::debug!("Failed to update live animation: {err}");
      }

      if let Some(fade_to) = &live.fade_to {
        let opacity = OpacityValue(0.0)
          .interpolate(fade_to, live.clock.eased_progress());

        if let Err(err) = live.window.set_transparency(&opacity) {
          tracing::debug!("Failed to update pop-in opacity: {err}");
        }
      }
    }

    if self.slides.is_empty() && self.closes.is_empty() {
      return Ok(());
    }

    let result = self
      .context
      .as_ref()
      .context("Animation context not initialized.")?
      .transaction(
        || {
          for (id, slide) in &self.slides {
            if !slide.clock.is_complete() {
              if let Some(anim_window) = self.windows.get(id) {
                anim_window.update(&slide.current_rect(), None)?;
              }
            }
          }

          #[cfg(target_os = "windows")]
          for closing in &self.closes {
            let (opacity, scale) = closing.state.current();
            closing.overlay.update_scaled(
              &closing.state.rect,
              Some(&opacity),
              scale,
            )?;
          }

          anyhow::Ok(())
        },
        dispatcher,
      )?;

    self.end_completed_closes();

    result
  }

  /// Removes close animations that have completed and destroys their
  /// overlays.
  fn end_completed_closes(&mut self) {
    if !self
      .closes
      .iter()
      .any(|closing| closing.state.clock.is_complete())
    {
      return;
    }

    let (done, running) = std::mem::take(&mut self.closes)
      .into_iter()
      .partition::<Vec<_>, _>(|closing| closing.state.clock.is_complete());

    self.closes = running;

    for closing in done {
      // On a thread of its own, as in `cancel_animation`, so the tick
      // never waits on the teardown.
      std::thread::spawn(move || {
        if let Err(err) = closing.overlay.destroy() {
          tracing::warn!(
            "Failed to destroy close animation window: {err}"
          );
        }
      });
    }

    self.update_tick_rate();
  }

  /// Returns how the window should animate to `target_rect`, or `None` if
  /// it should move without animation.
  pub fn animation_plan_for_window(
    &self,
    window: &WindowContainer,
    trigger: &AnimationTrigger,
    target_rect: &Rect,
    monitor_properties: &NativeMonitorProperties,
    config: &UserConfig,
  ) -> Option<AnimationPlan> {
    // Skip animation if:
    //  - The window is minimized.
    //  - The window is maximized (macOS only - can't override the OS's
    //    animation).
    //  - The window is hidden in the corner, but not animating. Safeguards
    //    against race condition where window finished an animation, but
    //    hasn't been moved to the real window position yet.
    if window.native_properties().is_minimized
      || (window.native_properties().is_maximized
        && cfg!(target_os = "macos"))
      || (!self.is_animating(&window.id())
        && window.is_in_corner(&monitor_properties.working_area))
    {
      return None;
    }

    let animations = &config.value.animations;

    let effect = match trigger {
      AnimationTrigger::WindowOpened => {
        let open_config = animations.open_effect()?;

        if self.is_animating(&window.id()) {
          return None;
        }

        open_config
      }
      AnimationTrigger::WindowMoved => {
        let move_config = animations.move_effect()?;

        // A fade in progress already hides the move.
        if self.fades.contains_key(&window.id()) {
          return None;
        }

        // If the window is mid-slide, compare the previous slide target
        // to the new target.
        let frame = window.native_properties().frame;
        let prev_rect = self
          .slides
          .get(&window.id())
          .map(|slide| &slide.target_rect)
          .or_else(|| {
            self.lives.get(&window.id()).map(|live| &live.target_rect)
          })
          .unwrap_or(&frame);

        let distance = (prev_rect.x() - target_rect.x()).abs()
          + (prev_rect.y() - target_rect.y()).abs()
          + (prev_rect.width() - target_rect.width()).abs()
          + (prev_rect.height() - target_rect.height()).abs();

        // Always in pixels: checked when the config is read
        // (`AnimationsConfig::validate`).
        #[allow(clippy::cast_possible_truncation)]
        let threshold_px = move_config.trigger_threshold.amount as i32;

        if distance <= threshold_px {
          return None;
        }

        &move_config.effect
      }
    };

    match effect.effect_type {
      AnimationEffect::Slide => Some(AnimationPlan::Slide(effect.clone())),
      // Fades change the real window's opacity, which is only possible on
      // Windows.
      AnimationEffect::Fade if cfg!(target_os = "windows") => {
        // A window mid-slide keeps sliding to its new target instead.
        if self.slides.contains_key(&window.id()) {
          Some(AnimationPlan::Slide(effect.clone()))
        } else {
          Some(AnimationPlan::FadeIn(effect.clone()))
        }
      }
      // Live animations move the real window, which is only done on
      // Windows. A window already moving keeps moving to its new target,
      // and one mid-slide finishes its slide.
      AnimationEffect::Move | AnimationEffect::Popin
        if cfg!(target_os = "windows") =>
      {
        if self.slides.contains_key(&window.id()) {
          Some(AnimationPlan::Slide(effect.clone()))
        } else {
          Some(AnimationPlan::Live {
            effect: effect.clone(),
            popin: effect.effect_type == AnimationEffect::Popin
              && !self.lives.contains_key(&window.id()),
          })
        }
      }
      // Off Windows, nothing animates.
      AnimationEffect::Fade
      | AnimationEffect::Move
      | AnimationEffect::Popin => None,
    }
  }

  /// Starts or retargets a live animation.
  // LINT: Live animations are only started on Windows.
  #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
  pub fn start_live(&mut self, request: LiveRequest) {
    let window_id = request.window.id();

    // A live animation replaces a fade; the window stays hidden until it
    // ends, unless it fades in itself.
    self.fades.remove(&window_id);

    // A pop-in sent elsewhere before it ended is shown in full at once,
    // rather than fading in again from nothing.
    #[cfg(target_os = "windows")]
    if request.fade_to.is_none() {
      if let Some(fade_to) = self
        .lives
        .get(&window_id)
        .and_then(|live| live.fade_to.as_ref())
      {
        let _ = request.window.native().set_transparency(fade_to);
      }
    }

    self.lives.insert(
      window_id,
      LiveState {
        clock: AnimationClock::new(&request.effect, request.frame_rate),
        window: request.window.native().clone(),
        start_frame: request.start_frame,
        target_frame: request.target_frame,
        target_rect: request.target_rect,
        fade_to: request.fade_to,
      },
    );

    self.update_tick_rate();
  }

  /// Creates the shared GPU context on first use.
  ///
  /// Returns `false` when it couldn't be made; the animation is then
  /// skipped, and the next one tries again.
  fn ensure_context(&mut self, dispatcher: &Dispatcher) -> bool {
    if self.context.is_none() {
      match AnimationContext::new(dispatcher) {
        Ok(context) => self.context = Some(context),
        Err(err) => {
          tracing::warn!("Failed to create animation context: {err}");
          return false;
        }
      }
    }

    true
  }

  /// Starts a close animation for a window the WM is about to close.
  ///
  /// Takes a snapshot of the window as it is now, under the window: once
  /// it closes, the snapshot fades out (and for `popin`, shrinks) where
  /// it was. If the app keeps the window open (e.g. it asks to save), the
  /// snapshot fades unseen beneath it.
  ///
  /// Skipped when no close animation is configured, and for windows that
  /// can't be seen or shouldn't move (minimized, maximized, fullscreen,
  /// still transparent from an animation).
  #[cfg(target_os = "windows")]
  pub fn start_close(
    &mut self,
    window: &WindowContainer,
    config: &UserConfig,
    frame_rate: u32,
    dispatcher: &Dispatcher,
  ) {
    let Some(effect) = config.value.animations.close_effect() else {
      return;
    };

    let properties = window.native_properties();

    if properties.is_minimized
      || properties.is_maximized
      || matches!(window.state(), wm_common::WindowState::Fullscreen(_))
      || self.hidden.contains(&window.id())
    {
      return;
    }

    if !self.ensure_context(dispatcher) {
      return;
    }

    // A running animation ends here, so the snapshot is of the window
    // itself rather than mid-move.
    self.cancel_animation(&window.id());

    let rect = properties.frame.clone();
    let native = window.native().clone();
    let request = AnimationWindowRequest {
      window: &native,
      inner_rect: rect.clone(),
      outer_rect: rect.clone(),
      opacity: None,
    };

    let overlay = match &self.context {
      Some(context) => {
        AnimationWindow::new_many(context, &[request], dispatcher).pop()
      }
      None => None,
    };

    match overlay {
      Some(Ok(overlay)) => {
        // The snapshot is the window's exit; the system's own close
        // transition would play over it. If the app keeps the window
        // open, it only loses the system's minimize animation.
        if let Err(err) = native.set_transitions_disabled(true) {
          tracing::debug!("Failed to disable system transitions: {err}");
        }

        self.closes.push(Closing {
          state: CloseState {
            // Started once the snapshot is up, so its capture doesn't eat
            // into the animation.
            clock: AnimationClock::new(effect, frame_rate),
            rect,
            end_scale: if effect.effect_type == AnimationEffect::Popin {
              CLOSE_END_SCALE
            } else {
              1.0
            },
          },
          overlay,
        });

        self.update_tick_rate();
      }
      Some(Err(err)) => {
        tracing::warn!("Failed to create close animation window: {err}");
      }
      None => {}
    }
  }

  /// Starts or extends slide animations for several windows at once.
  ///
  /// Overlays for windows that weren't already sliding are created
  /// together, so their screen captures run in parallel.
  ///
  /// Returns the IDs of windows whose slide is running. The caller should
  /// hide these windows at their target position.
  pub fn start_slides(
    &mut self,
    requests: Vec<SlideRequest>,
    dispatcher: &Dispatcher,
  ) -> HashSet<Uuid> {
    let mut started = HashSet::new();

    if requests.is_empty() {
      return started;
    }

    if !self.ensure_context(dispatcher) {
      return started;
    }

    let mut new_slides = Vec::new();

    for request in requests {
      let window_id = request.window.id();

      // A slide replaces a fade, continuing from the window's current
      // spot. The window stays hidden until the slide completes.
      self.fades.remove(&window_id);

      let start_rect = self.slides.get(&window_id).map_or_else(
        || request.window.native_properties().frame.clone(),
        SlideState::current_rect,
      );

      let slide = SlideState {
        clock: AnimationClock::new(&request.effect, request.frame_rate),
        start_rect,
        target_rect: request.target_rect,
      };

      let outer_rect =
        self.outer_rect(&request.window, &slide, dispatcher);

      if self.windows.contains_key(&window_id) {
        match self.retarget_slide(
          &window_id,
          &slide,
          &outer_rect,
          dispatcher,
        ) {
          Ok(()) => {
            self.slides.insert(window_id, slide);
            started.insert(window_id);
          }
          Err(err) => {
            tracing::warn!("Failed to retarget slide animation: {err}");
            self.cancel_animation(&window_id);
          }
        }
      } else {
        new_slides.push((request.window, slide, outer_rect));
      }
    }

    if !new_slides.is_empty() {
      let overlays = {
        let natives = new_slides
          .iter()
          .map(|(window, _, _)| window.native().clone())
          .collect::<Vec<_>>();

        let overlay_requests = new_slides
          .iter()
          .zip(&natives)
          .map(|((_, slide, outer_rect), native)| AnimationWindowRequest {
            window: native,
            inner_rect: slide.start_rect.clone(),
            outer_rect: outer_rect.clone(),
            opacity: None,
          })
          .collect::<Vec<_>>();

        match &self.context {
          Some(context) => AnimationWindow::new_many(
            context,
            &overlay_requests,
            dispatcher,
          ),
          None => Vec::new(),
        }
      };

      for ((window, mut slide, _), overlay) in
        new_slides.into_iter().zip(overlays)
      {
        match overlay {
          Ok(overlay) => {
            // Start the clock once the overlay is up, so captures don't
            // eat into the animation.
            slide.clock.start_time = Instant::now();
            self.windows.insert(window.id(), overlay);
            self.slides.insert(window.id(), slide);
            started.insert(window.id());
          }
          Err(err) => {
            tracing::warn!("Failed to create animation window: {err}");
          }
        }
      }
    }

    self.update_tick_rate();

    started
  }

  /// Starts fading in a window that has just been hidden at its target
  /// position.
  pub fn start_fade(
    &mut self,
    window: &WindowContainer,
    effect: &AnimationEffectConfig,
    target_opacity: OpacityValue,
    frame_rate: u32,
  ) {
    self.fades.insert(
      window.id(),
      FadeState {
        clock: AnimationClock::new(effect, frame_rate),
        window: window.native().clone(),
        target_opacity,
      },
    );

    self.update_tick_rate();
  }

  /// Gets the bounds of the overlay for a slide: the union of its start
  /// and target rects.
  // LINT: `self` is only used on macOS.
  #[cfg_attr(not(target_os = "macos"), allow(clippy::unused_self))]
  fn outer_rect(
    &self,
    // LINT: `window` and `dispatcher` are only used on macOS.
    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
    window: &WindowContainer,
    slide: &SlideState,
    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
    dispatcher: &Dispatcher,
  ) -> Rect {
    let outer_rect = slide.start_rect.union(&slide.target_rect);

    // On macOS, windows cannot span across multiple displays when
    // "Displays have separate Spaces" is enabled. Attempting to position a
    // window beyond the display bounds causes it to wrap around on the
    // same display. We therefore crop the animation to only be shown on
    // the source display.
    #[cfg(target_os = "macos")]
    if self.displays_have_separate_spaces {
      if let Ok(display_bounds) = dispatcher
        .nearest_display(&window.native())
        .and_then(|display| display.bounds())
      {
        return outer_rect.crop(&display_bounds);
      }
    }

    outer_rect
  }

  /// Resizes an existing overlay to the bounds of a new slide, keeping its
  /// screenshot and z-order.
  fn retarget_slide(
    &mut self,
    window_id: &Uuid,
    slide: &SlideState,
    outer_rect: &Rect,
    dispatcher: &Dispatcher,
  ) -> anyhow::Result<()> {
    let anim_window = self
      .windows
      .get_mut(window_id)
      .context("No animation window.")?;

    anim_window.resize(outer_rect)?;

    // Immediately redraw the animation after resizing. It's positioned
    // relative to the overlay's frame, so it would otherwise be incorrect
    // until the next tick.
    let anim_window = &*anim_window;
    self
      .context
      .as_ref()
      .context("Animation context not initialized.")?
      .transaction(
        || anim_window.update(&slide.current_rect(), None),
        dispatcher,
      )??;

    Ok(())
  }

  /// Spawns a task for emitting ticks at the target frame rate.
  ///
  /// Cancels the existing tick task if the frame rate changes. The ticks
  /// are emitted at the highest frame rate among the animated windows.
  ///
  /// Called on animation start and completion.
  fn update_tick_rate(&mut self) {
    let frame_rate = self
      .slides
      .values()
      .map(|slide| slide.clock.frame_rate)
      .chain(self.fades.values().map(|fade| fade.clock.frame_rate))
      .chain(self.lives.values().map(|live| live.clock.frame_rate))
      .chain(
        self
          .closes
          .iter()
          .map(|closing| closing.state.clock.frame_rate),
      )
      .max();

    // Keep the running task when the rate is unchanged, so ticks stay
    // evenly spaced as animations come and go.
    if frame_rate == self.tick_frame_rate && self.tick_task.is_some() {
      return;
    }

    if let Some(handle) = self.tick_task.take() {
      handle.abort();
    }

    self.tick_frame_rate = frame_rate;

    let Some(frame_rate) = frame_rate else {
      return;
    };

    let frame_time = Duration::from_secs_f64(1.0 / f64::from(frame_rate));
    let tick_tx = self.tick_tx.clone();

    self.tick_task = Some(tokio::spawn(async move {
      let mut interval = tokio::time::interval(frame_time);
      interval
        .set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

      loop {
        interval.tick().await;
        if tick_tx.send(()).is_err() {
          break;
        }
      }
    }));
  }
}

#[cfg(test)]
mod tests {
  use std::time::{Duration, Instant};

  use wm_common::AnimationEffectConfig;
  use wm_platform::{EasingFunction, Rect};

  use super::{AnimationClock, CloseState, SlideState, CLOSE_END_SCALE};

  fn clock(elapsed_ms: u64, duration_ms: u32) -> AnimationClock {
    let config = AnimationEffectConfig {
      duration_ms,
      easing: EasingFunction::Linear,
      ..AnimationEffectConfig::default()
    };

    let mut clock = AnimationClock::new(&config, 60);
    clock.start_time = Instant::now()
      .checked_sub(Duration::from_millis(elapsed_ms))
      .unwrap_or_else(Instant::now);

    clock
  }

  #[test]
  fn clock_completes_after_duration() {
    assert!(clock(250, 200).is_complete());
    assert!(!clock(0, 200).is_complete());
  }

  #[test]
  fn slide_ends_at_target() {
    let slide = SlideState {
      clock: clock(500, 200),
      start_rect: Rect::from_xy(0, 0, 100, 100),
      target_rect: Rect::from_xy(200, 50, 300, 400),
    };

    assert_eq!(slide.current_rect(), slide.target_rect);
  }

  /// A close animation `elapsed_ms` into 160 ms, ending at `end_scale`.
  fn close(elapsed_ms: u64, end_scale: f32) -> CloseState {
    CloseState {
      clock: clock(elapsed_ms, 160),
      rect: Rect::from_xy(0, 0, 1000, 800),
      end_scale,
    }
  }

  #[test]
  fn close_starts_as_the_window() {
    let (opacity, scale) = close(0, CLOSE_END_SCALE).current();

    assert!((opacity.0 - 1.0).abs() < 0.05);
    assert!((scale - 1.0).abs() < 0.01);
  }

  #[test]
  fn close_ends_invisible_and_small() {
    let (opacity, scale) = close(500, CLOSE_END_SCALE).current();

    assert!(opacity.0.abs() < f32::EPSILON);
    assert!((scale - CLOSE_END_SCALE).abs() < f32::EPSILON);
  }

  #[test]
  fn fade_close_keeps_its_size() {
    let (_, scale) = close(80, 1.0).current();

    assert!((scale - 1.0).abs() < f32::EPSILON);
  }
}
