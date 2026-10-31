use std::{
  collections::{HashMap, HashSet},
  time::{Duration, Instant},
};

#[cfg(not(target_os = "windows"))]
use anyhow::Context;
use tokio::sync::mpsc;
use uuid::Uuid;
use wm_common::{AnimationEffect, AnimationTiming, ResolvedEffect};
#[cfg(target_os = "macos")]
use wm_platform::DispatcherExtMacOs;
#[cfg(target_os = "windows")]
use wm_platform::NativeWindowWindowsExt;
#[cfg(target_os = "windows")]
use wm_platform::{AnimatedValue, OverlayAnimation};
use wm_platform::{
  AnimationContext, AnimationCurve, AnimationWindow,
  AnimationWindowRequest, Dispatcher, Easing, NativeWindow, OpacityValue,
  Rect,
};

use crate::{
  animation_stats::AnimationStats,
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
    && wm_platform::system_animations_enabled()
    && config.value.animations.open_effect().is_some_and(|open| {
      matches!(open.style, AnimationEffect::Fade | AnimationEffect::Popin)
    })
}

/// Keeps a rect at least one pixel wide and tall: an overshooting curve
/// could otherwise shrink a window past nothing and flip it.
fn at_least_one_pixel(rect: &Rect) -> Rect {
  Rect::from_xy(
    rect.x(),
    rect.y(),
    rect.width().max(1),
    rect.height().max(1),
  )
}

/// How long a finished slide's snapshot takes to fade into the real
/// window (see `AnimationManager::end_animation`).
// LINT: Crossfades only run on Windows.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
const CROSSFADE: Duration = Duration::from_millis(70);

pub enum AnimationTrigger {
  WindowOpened,
  WindowMoved,
}

/// How a window should get to its new position.
#[derive(Clone, Debug)]
pub enum AnimationPlan {
  /// A snapshot of the window slides from its old position to the new
  /// one, while the real window waits invisible at the new position.
  Slide(ResolvedEffect),

  /// The real window is placed at the new position while invisible, then
  /// fades in.
  FadeIn(ResolvedEffect),

  /// The real window itself moves to the new position a little each
  /// frame. With `popin`, it grows there from smaller while fading in.
  Live { effect: ResolvedEffect, popin: bool },
}

/// A window to start a live animation for.
// LINT: Live animations are only started on Windows.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub struct LiveRequest {
  pub window: WindowContainer,
  pub effect: ResolvedEffect,
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
  pub effect: ResolvedEffect,
  pub target_rect: Rect,
  pub frame_rate: u32,
}

/// Timing shared by every kind of animation.
#[derive(Clone, Debug)]
struct AnimationClock {
  start_time: Instant,
  duration: Duration,
  easing: Easing,

  /// Target frame rate for the animation.
  frame_rate: u32,
}

impl AnimationClock {
  fn new(timing: &AnimationTiming, frame_rate: u32) -> Self {
    Self {
      start_time: Instant::now(),
      duration: Duration::from_millis(u64::from(timing.duration_ms)),
      easing: timing.easing.clone(),
      frame_rate,
    }
  }

  /// A clock for an opacity change: the effect's fade, or one that's
  /// over at once when fades are off.
  fn for_fade(effect: &ResolvedEffect, frame_rate: u32) -> Self {
    let instant = AnimationTiming {
      duration_ms: 0,
      easing: Easing::default(),
    };

    Self::new(effect.fade.as_ref().unwrap_or(&instant), frame_rate)
  }

  /// Returns the normalized animation progress in `[0.0, 1.0]`.
  fn progress(&self) -> f32 {
    self.progress_at(Instant::now())
  }

  /// Returns the normalized progress at `at`: 0 until the animation
  /// starts (it may start in the future, on the compositor's next frame),
  /// 1 once it's over.
  fn progress_at(&self, at: Instant) -> f32 {
    let elapsed = at.saturating_duration_since(self.start_time);

    if elapsed >= self.duration {
      1.0
    } else {
      (elapsed.as_secs_f32() / self.duration.as_secs_f32()).clamp(0.0, 1.0)
    }
  }

  /// Returns the eased animation progress: 0 at the start, 1 at the end,
  /// and with an overshooting curve, outside [0, 1] in between.
  fn eased_progress(&self) -> f32 {
    self.eased_progress_at(Instant::now())
  }

  /// Returns the eased progress at `at` (see `eased_progress`).
  fn eased_progress_at(&self, at: Instant) -> f32 {
    self.easing.apply(self.progress_at(at))
  }

  /// When the animation ends.
  fn end_time(&self) -> Instant {
    self.start_time + self.duration
  }

  /// The clock's duration and curve, for the compositor to play.
  // LINT: Only the compositor-driven platform uses it.
  #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
  fn curve(&self) -> AnimationCurve {
    AnimationCurve {
      duration: self.duration,
      easing: self.easing.clone(),
    }
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
  // LINT: Only ticked slides (off Windows) are drawn from it.
  #[cfg_attr(target_os = "windows", allow(dead_code))]
  fn current_rect(&self) -> Rect {
    self.rect_at(Instant::now())
  }

  /// Returns the interpolated rect at `at`.
  fn rect_at(&self, at: Instant) -> Rect {
    at_least_one_pixel(
      &self
        .start_rect
        .interpolate(&self.target_rect, self.clock.eased_progress_at(at)),
    )
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

  /// Timing of a pop-in's fade.
  fade_clock: AnimationClock,
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
    at_least_one_pixel(
      &self
        .start_frame
        .interpolate(&self.target_frame, self.clock.eased_progress()),
    )
  }

  /// Whether both the motion and the opacity change are over.
  fn is_complete(&self) -> bool {
    self.clock.is_complete() && self.fade_clock.is_complete()
  }

  /// Returns the window's opacity at the current progress, for a pop-in.
  fn current_opacity(&self) -> Option<OpacityValue> {
    self.fade_to.as_ref().map(|fade_to| {
      OpacityValue(0.0)
        .interpolate(fade_to, self.fade_clock.eased_progress())
    })
  }
}

/// State of a close animation: a snapshot of a closed window fading out
/// (and, for `popin`, shrinking) where the window was.
// LINT: Close animations are only started on Windows.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
#[derive(Clone, Debug)]
struct CloseState {
  /// Timing of the shrink.
  clock: AnimationClock,

  /// Timing of the fade out.
  fade_clock: AnimationClock,

  /// Where the window was when it was closed.
  rect: Rect,

  /// Scale the snapshot ends at (1.0: fade only).
  end_scale: f32,
}

// LINT: Close animations are only started on Windows.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
impl CloseState {
  /// Whether both the shrink and the fade are over.
  fn is_complete(&self) -> bool {
    self.clock.is_complete() && self.fade_clock.is_complete()
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
  #[cfg(not(target_os = "windows"))]
  tick_task: Option<tokio::task::JoinHandle<()>>,

  /// Frame rate of the running tick task, if any.
  #[cfg(not(target_os = "windows"))]
  tick_frame_rate: Option<u32>,

  /// Stop flag of the thread sending a tick per compositor frame, while
  /// it runs.
  #[cfg(target_os = "windows")]
  pacer: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,

  /// Frame timing of the running animations.
  stats: AnimationStats,

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
      #[cfg(not(target_os = "windows"))]
      tick_task: None,
      #[cfg(not(target_os = "windows"))]
      tick_frame_rate: None,
      #[cfg(target_os = "windows")]
      pacer: None,
      stats: AnimationStats::default(),
      #[cfg(target_os = "macos")]
      displays_have_separate_spaces: dispatcher
        .displays_have_separate_spaces(),
    }
  }

  /// Marks now as when the animations about to start were asked for, to
  /// measure how long they take to start. An earlier mark is kept until
  /// [`AnimationManager::clear_request`].
  pub fn mark_request(&mut self) {
    self.stats.mark_request();
  }

  /// Forgets the mark of [`AnimationManager::mark_request`].
  pub fn clear_request(&mut self) {
    self.stats.clear_request();
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
          .filter(|(_, live)| live.is_complete())
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
      self.update_ticking();
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
      self.update_ticking();
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

  /// Ends the window's animation as its window is placed for good: a
  /// finished slide's snapshot fades out over the real window (already
  /// redrawn beneath it at its place), hiding the app's own redraw at its
  /// new size. Any other animation is cancelled.
  ///
  /// # Platform-specific
  ///
  /// - **macOS**: Same as [`AnimationManager::cancel_animation`].
  // LINT: `dispatcher` is only used on Windows.
  #[cfg_attr(not(target_os = "windows"), allow(unused_variables))]
  pub fn end_animation(
    &mut self,
    window_id: &Uuid,
    dispatcher: &Dispatcher,
  ) {
    let finished_slide = self
      .slides
      .get(window_id)
      .is_some_and(|slide| slide.clock.is_complete());

    #[cfg(target_os = "windows")]
    if finished_slide {
      if let (Some(slide), Some(overlay)) = (
        self.slides.remove(window_id),
        self.windows.remove(window_id),
      ) {
        self.update_ticking();
        self.crossfade(overlay, &slide.target_rect, dispatcher);
        return;
      }
    }

    self.cancel_animation(window_id);
  }

  /// Fades a finished slide's snapshot out at `rect`, then destroys it.
  /// If it can't fade, it's destroyed as in `cancel_animation`.
  #[cfg(target_os = "windows")]
  fn crossfade(
    &self,
    overlay: AnimationWindow,
    rect: &Rect,
    dispatcher: &Dispatcher,
  ) {
    let curve = AnimationCurve {
      duration: CROSSFADE,
      easing: Easing::default(),
    };

    let animation = OverlayAnimation {
      rect: AnimatedValue {
        from: rect.clone(),
        to: rect.clone(),
        curve: curve.clone(),
      },
      opacity: Some(AnimatedValue {
        from: OpacityValue(1.0),
        to: OpacityValue(0.0),
        curve,
      }),
      scale: None,
    };

    let faded = self.context.as_ref().is_some_and(|context| {
      context
        .transaction(|| overlay.animate(&animation), dispatcher)
        .and_then(|result| result)
        .inspect_err(|err| tracing::debug!("Failed to crossfade: {err}"))
        .is_ok()
    });

    let keep_for = if faded {
      CROSSFADE + Duration::from_millis(20)
    } else {
      Duration::from_millis(20)
    };

    std::thread::spawn(move || {
      std::thread::sleep(keep_for);
      if let Err(err) = overlay.destroy() {
        tracing::warn!("Failed to destroy animation window: {err}");
      }
    });
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

  /// Updates the animations that change every frame, during a tick.
  ///
  /// On Windows, those are the ones moving or fading real windows: the
  /// compositor plays slides and closes on its own. Elsewhere, slides too,
  /// batched into a single compositor transaction.
  // LINT: `dispatcher` and the result are only used off Windows.
  #[cfg_attr(
    target_os = "windows",
    allow(unused_variables, clippy::unnecessary_wraps)
  )]
  pub fn tick_update(
    &mut self,
    dispatcher: &Dispatcher,
  ) -> anyhow::Result<()> {
    self.stats.frame();

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

      if let Some(opacity) = live.current_opacity() {
        if let Err(err) = live.window.set_transparency(&opacity) {
          tracing::debug!("Failed to update pop-in opacity: {err}");
        }
      }
    }

    #[cfg(not(target_os = "windows"))]
    if !self.slides.is_empty() {
      self
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

            anyhow::Ok(())
          },
          dispatcher,
        )??;
    }

    Ok(())
  }

  /// When the next animation the compositor plays on its own ends, if
  /// any is running: the WM wakes then to end it, instead of every frame.
  ///
  /// # Platform-specific
  ///
  /// - **macOS**: Always `None`; every animation is ticked.
  pub fn next_deadline(&self) -> Option<Instant> {
    if !cfg!(target_os = "windows") {
      return None;
    }

    // Finished ones aren't counted: a slide left at its end (while a
    // settle holds syncs back) would otherwise wake the WM nonstop.
    self
      .slides
      .values()
      .filter(|slide| !slide.clock.is_complete())
      .map(|slide| slide.clock.end_time())
      .chain(
        self
          .closes
          .iter()
          .filter(|closing| !closing.state.is_complete())
          .map(|closing| {
            closing
              .state
              .clock
              .end_time()
              .max(closing.state.fade_clock.end_time())
          }),
      )
      .min()
  }

  /// Removes close animations that have completed and destroys their
  /// overlays.
  pub fn end_completed_closes(&mut self) {
    if !self
      .closes
      .iter()
      .any(|closing| closing.state.is_complete())
    {
      return;
    }

    let (done, running) = std::mem::take(&mut self.closes)
      .into_iter()
      .partition::<Vec<_>, _>(|closing| closing.state.is_complete());

    self.closes = running;

    for closing in done {
      // On a thread of its own, as in `cancel_animation`, so the WM never
      // waits on the teardown.
      std::thread::spawn(move || {
        if let Err(err) = closing.overlay.destroy() {
          tracing::warn!(
            "Failed to destroy close animation window: {err}"
          );
        }
      });
    }

    self.update_ticking();
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
    //  - The system's own animations are turned off (accessibility).
    if !wm_platform::system_animations_enabled()
      || window.native_properties().is_minimized
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

        if distance <= move_config.trigger_threshold_px {
          return None;
        }

        &move_config.effect
      }
    };

    match effect.style {
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
            popin: effect.style == AnimationEffect::Popin
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
        clock: AnimationClock::new(
          &request.effect.timing,
          request.frame_rate,
        ),
        fade_clock: AnimationClock::for_fade(
          &request.effect,
          request.frame_rate,
        ),
        window: request.window.native().clone(),
        start_frame: request.start_frame,
        target_frame: request.target_frame,
        target_rect: request.target_rect,
        fade_to: request.fade_to,
      },
    );

    self.stats.animation_started(request.frame_rate);
    self.update_ticking();
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
    let Some(effect) = config
      .value
      .animations
      .close_effect()
      .filter(|_| wm_platform::system_animations_enabled())
    else {
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

    let overlay = match overlay {
      Some(Ok(overlay)) => overlay,
      Some(Err(err)) => {
        tracing::warn!("Failed to create close animation window: {err}");
        return;
      }
      None => return,
    };

    // Started once the snapshot is up, so its capture doesn't eat into the
    // animation.
    let start_at = self.next_frame_time(dispatcher);

    let mut state = CloseState {
      clock: AnimationClock::new(&effect.timing, frame_rate),
      fade_clock: AnimationClock::for_fade(effect, frame_rate),
      rect,
      end_scale: if effect.style == AnimationEffect::Popin {
        effect.popin_scale
      } else {
        1.0
      },
    };

    state.clock.start_time = start_at;
    state.fade_clock.start_time = start_at;

    let animation = OverlayAnimation {
      rect: AnimatedValue {
        from: state.rect.clone(),
        to: state.rect.clone(),
        curve: state.clock.curve(),
      },
      opacity: Some(AnimatedValue {
        from: OpacityValue(1.0),
        to: OpacityValue(0.0),
        curve: state.fade_clock.curve(),
      }),
      scale: Some(AnimatedValue {
        from: 1.0,
        to: state.end_scale,
        curve: state.clock.curve(),
      }),
    };

    let result = match &self.context {
      Some(context) => context
        .transaction(|| overlay.animate(&animation), dispatcher)
        .and_then(|result| result),
      None => return,
    };

    if let Err(err) = result {
      tracing::warn!("Failed to start close animation: {err}");

      std::thread::spawn(move || {
        if let Err(err) = overlay.destroy() {
          tracing::warn!(
            "Failed to destroy close animation window: {err}"
          );
        }
      });

      return;
    }

    // The snapshot is the window's exit; the system's own close transition
    // would play over it. If the app keeps the window open, it only loses
    // the system's minimize animation.
    if let Err(err) = native.set_transitions_disabled(true) {
      tracing::debug!("Failed to disable system transitions: {err}");
    }

    self.closes.push(Closing { state, overlay });
    self.stats.animation_started(frame_rate);
    self.stats.first_frame_at(start_at);
    self.update_ticking();
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

    if requests.is_empty() || !self.ensure_context(dispatcher) {
      return started;
    }

    // A slide already running turns towards its new target from where it
    // is when the change shows: on the compositor's next frame.
    let retarget_at = self.next_frame_time(dispatcher);

    let mut retargeted = Vec::new();
    let mut new_slides = Vec::new();

    for request in requests {
      let window_id = request.window.id();

      // A slide replaces a fade, continuing from the window's current
      // spot. The window stays hidden until the slide completes.
      self.fades.remove(&window_id);

      let start_rect = self.slides.get(&window_id).map_or_else(
        || request.window.native_properties().frame.clone(),
        |slide| slide.rect_at(retarget_at),
      );

      let mut slide = SlideState {
        clock: AnimationClock::new(
          &request.effect.timing,
          request.frame_rate,
        ),
        start_rect,
        target_rect: request.target_rect,
      };

      let outer_rect =
        self.outer_rect(&request.window, &slide, dispatcher);

      match self.windows.get_mut(&window_id) {
        Some(overlay) => match overlay.resize(&outer_rect) {
          Ok(()) => {
            slide.clock.start_time = retarget_at;
            self.slides.insert(window_id, slide);
            retargeted.push(window_id);
          }
          Err(err) => {
            tracing::warn!("Failed to retarget slide animation: {err}");
            self.cancel_animation(&window_id);
          }
        },
        None => new_slides.push((request.window, slide, outer_rect)),
      }
    }

    started.extend(self.draw_slides(&retargeted, retarget_at, dispatcher));

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

      // Started once the overlays are up, so captures don't eat into the
      // animation.
      let start_at = self.next_frame_time(dispatcher);
      let mut created = Vec::new();

      for ((window, mut slide, _), overlay) in
        new_slides.into_iter().zip(overlays)
      {
        match overlay {
          Ok(overlay) => {
            slide.clock.start_time = start_at;
            self.windows.insert(window.id(), overlay);
            self.slides.insert(window.id(), slide);
            created.push(window.id());
          }
          Err(err) => {
            tracing::warn!("Failed to create animation window: {err}");
          }
        }
      }

      started.extend(self.draw_slides(&created, start_at, dispatcher));
    }

    self.update_ticking();

    started
  }

  /// Draws the slides of the given windows, in one compositor
  /// transaction: on Windows, hands each slide to the compositor to play;
  /// elsewhere, draws its first frame. Slides that fail are cancelled.
  ///
  /// Returns the IDs of the slides now running.
  fn draw_slides(
    &mut self,
    window_ids: &[Uuid],
    start_at: Instant,
    dispatcher: &Dispatcher,
  ) -> Vec<Uuid> {
    if window_ids.is_empty() {
      return Vec::new();
    }

    let result = match &self.context {
      Some(context) => context
        .transaction(
          || {
            for id in window_ids {
              let (Some(overlay), Some(slide)) =
                (self.windows.get(id), self.slides.get(id))
              else {
                continue;
              };

              #[cfg(target_os = "windows")]
              overlay.animate(&OverlayAnimation {
                rect: AnimatedValue {
                  from: slide.start_rect.clone(),
                  to: slide.target_rect.clone(),
                  curve: slide.clock.curve(),
                },
                opacity: None,
                scale: None,
              })?;

              #[cfg(not(target_os = "windows"))]
              overlay.update(&slide.current_rect(), None)?;
            }

            anyhow::Ok(())
          },
          dispatcher,
        )
        .map_err(anyhow::Error::from)
        .and_then(|result| result),
      None => Err(anyhow::anyhow!("Animation context not initialized.")),
    };

    if let Err(err) = result {
      tracing::warn!("Failed to start slide animations: {err}");

      for id in window_ids {
        self.cancel_animation(id);
      }

      return Vec::new();
    }

    for id in window_ids {
      if let Some(slide) = self.slides.get(id) {
        self.stats.animation_started(slide.clock.frame_rate);
      }
    }

    self.stats.first_frame_at(start_at);

    window_ids.to_vec()
  }

  /// Estimates when the compositor shows its next frame, where an
  /// animation committed now starts.
  ///
  /// # Platform-specific
  ///
  /// - **macOS**: Now; animations are ticked from when they start.
  // LINT: `self` and `dispatcher` are only used on Windows.
  #[cfg_attr(
    not(target_os = "windows"),
    allow(unused_variables, clippy::unused_self)
  )]
  fn next_frame_time(&self, dispatcher: &Dispatcher) -> Instant {
    #[cfg(target_os = "windows")]
    if let Some(context) = &self.context {
      match context.next_frame_time(dispatcher) {
        Ok(time) => return time,
        Err(err) => {
          tracing::debug!("Failed to read the compositor's clock: {err}");
        }
      }
    }

    Instant::now()
  }

  /// Starts fading in a window that has just been hidden at its target
  /// position.
  pub fn start_fade(
    &mut self,
    window: &WindowContainer,
    effect: &ResolvedEffect,
    target_opacity: OpacityValue,
    frame_rate: u32,
  ) {
    self.fades.insert(
      window.id(),
      FadeState {
        clock: AnimationClock::for_fade(effect, frame_rate),
        window: window.native().clone(),
        target_opacity,
      },
    );

    self.stats.animation_started(frame_rate);
    self.update_ticking();
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

  /// Whether no animation is running.
  fn is_idle(&self) -> bool {
    self.slides.is_empty()
      && self.fades.is_empty()
      && self.lives.is_empty()
      && self.closes.is_empty()
  }

  /// Starts or stops the ticks for animations that change every frame,
  /// and logs the timing of a run of animations once it's over.
  ///
  /// Called on animation start and completion.
  fn update_ticking(&mut self) {
    if self.is_idle() {
      self.stats.idle();
    }

    #[cfg(target_os = "windows")]
    self.set_paced_ticks(!self.fades.is_empty() || !self.lives.is_empty());

    #[cfg(not(target_os = "windows"))]
    self.update_tick_rate();
  }

  /// Starts or stops the thread that sends a tick per compositor frame,
  /// for animations of real windows.
  ///
  /// Paced by the compositor (`DwmFlush`), so each tick lands right
  /// after a frame is shown, rather than on a timer drifting against the
  /// display.
  #[cfg(target_os = "windows")]
  fn set_paced_ticks(&mut self, on: bool) {
    use std::sync::{
      atomic::{AtomicBool, Ordering},
      Arc,
    };

    if !on {
      if let Some(stop) = self.pacer.take() {
        stop.store(true, Ordering::Relaxed);
      }

      return;
    }

    if self.pacer.is_some() {
      return;
    }

    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    let tick_tx = self.tick_tx.clone();

    let spawned = std::thread::Builder::new()
      .name("animation-pacer".to_string())
      .spawn(move || {
        while !thread_stop.load(Ordering::Relaxed) {
          if let Err(err) = wm_platform::wait_for_compositor_frame() {
            // Without a compositor clock, tick at about 60 Hz.
            tracing::debug!(
              "Failed to wait for a compositor frame: {err}"
            );
            std::thread::sleep(Duration::from_millis(16));
          }

          if thread_stop.load(Ordering::Relaxed)
            || tick_tx.send(()).is_err()
          {
            break;
          }
        }
      });

    match spawned {
      Ok(_) => self.pacer = Some(stop),
      Err(err) => tracing::warn!("Failed to start animation pacer: {err}"),
    }
  }

  /// Spawns a task for emitting ticks at the target frame rate.
  ///
  /// Cancels the existing tick task if the frame rate changes. The ticks
  /// are emitted at the highest frame rate among the animated windows.
  #[cfg(not(target_os = "windows"))]
  fn update_tick_rate(&mut self) {
    let frame_rate = self
      .slides
      .values()
      .map(|slide| slide.clock.frame_rate)
      .chain(self.fades.values().map(|fade| fade.clock.frame_rate))
      .chain(self.lives.values().map(|live| live.clock.frame_rate))
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

  use wm_common::{AnimationEffect, AnimationTiming, ResolvedEffect};
  use wm_platform::{CubicBezier, Easing, EasingPreset, Rect};

  use super::{AnimationClock, CloseState, SlideState};

  fn timing(duration_ms: u32, easing: Easing) -> AnimationTiming {
    AnimationTiming {
      duration_ms,
      easing,
    }
  }

  fn clock_with(
    elapsed_ms: u64,
    timing: &AnimationTiming,
  ) -> AnimationClock {
    let mut clock = AnimationClock::new(timing, 60);
    clock.start_time = Instant::now()
      .checked_sub(Duration::from_millis(elapsed_ms))
      .unwrap_or_else(Instant::now);

    clock
  }

  fn clock(elapsed_ms: u64, duration_ms: u32) -> AnimationClock {
    clock_with(
      elapsed_ms,
      &timing(duration_ms, Easing::Preset(EasingPreset::Linear)),
    )
  }

  #[test]
  fn clock_completes_after_duration() {
    assert!(clock(250, 200).is_complete());
    assert!(!clock(0, 200).is_complete());
  }

  #[test]
  fn instant_fade_is_over_at_once() {
    let effect = ResolvedEffect {
      style: AnimationEffect::Popin,
      timing: timing(200, Easing::default()),
      fade: None,
      popin_scale: 0.9,
    };

    assert!(AnimationClock::for_fade(&effect, 60).is_complete());
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

  #[test]
  fn slide_waits_at_its_start_until_the_compositor_starts_it() {
    let mut slide = SlideState {
      clock: clock(0, 200),
      start_rect: Rect::from_xy(0, 0, 100, 100),
      target_rect: Rect::from_xy(500, 0, 100, 100),
    };

    // Starts on a frame 16 ms from now.
    let now = Instant::now();
    slide.clock.start_time = now + Duration::from_millis(16);

    assert_eq!(slide.rect_at(now), slide.start_rect);
    assert!(!slide.clock.is_complete());
    assert_eq!(slide.clock.end_time(), now + Duration::from_millis(216));
    assert_eq!(
      slide.rect_at(now + Duration::from_millis(300)),
      slide.target_rect
    );
  }

  #[test]
  fn overshooting_slide_never_inverts_the_size() {
    // Pulls far back before starting: the size would go below zero.
    let curve =
      CubicBezier::new([0.5, -3.0, 0.9, 1.0]).expect("valid bezier");

    for elapsed in (0..200).step_by(10) {
      let slide = SlideState {
        clock: clock_with(
          elapsed,
          &timing(200, Easing::Bezier(curve.clone())),
        ),
        start_rect: Rect::from_xy(0, 0, 20, 20),
        target_rect: Rect::from_xy(0, 0, 400, 400),
      };

      let rect = slide.current_rect();
      assert!(rect.width() >= 1 && rect.height() >= 1, "{rect:?}");
    }
  }

  /// A close animation `elapsed_ms` into 160 ms, ending at `end_scale`.
  fn close(elapsed_ms: u64, end_scale: f32) -> CloseState {
    CloseState {
      clock: clock(elapsed_ms, 160),
      fade_clock: clock(elapsed_ms, 160),
      rect: Rect::from_xy(0, 0, 1000, 800),
      end_scale,
    }
  }

  #[test]
  fn close_ends_with_its_longer_clock() {
    assert!(close(500, 0.95).is_complete());
    assert!(!close(80, 0.95).is_complete());
  }

  #[test]
  fn close_waits_for_the_longer_of_shrink_and_fade() {
    let closing = CloseState {
      clock: clock(100, 80),
      fade_clock: clock(100, 300),
      rect: Rect::from_xy(0, 0, 100, 100),
      end_scale: 0.95,
    };

    assert!(!closing.is_complete());
  }
}
