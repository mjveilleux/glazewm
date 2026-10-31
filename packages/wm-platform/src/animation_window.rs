use std::time::Duration;
#[cfg(target_os = "windows")]
use std::time::Instant;

use crate::{
  platform_impl, Dispatcher, Easing, NativeWindow, OpacityValue, Rect,
};

/// Shared context used by [`AnimationWindow`] instances. Holds GPU
/// resources that can be shared between animations.
///
/// Exposes a [`AnimationContext::transaction`] method for batching updates
/// across animation windows.
pub struct AnimationContext {
  inner: platform_impl::AnimationContext,
}

impl AnimationContext {
  /// Creates a new [`AnimationContext`].
  pub fn new(dispatcher: &Dispatcher) -> crate::Result<Self> {
    let inner = platform_impl::AnimationContext::new(dispatcher)?;
    Ok(Self { inner })
  }

  /// Whether the GPU device behind the context is gone (e.g. the driver
  /// was updated or reset): nothing drawn with it shows anymore, and a new
  /// context is needed.
  ///
  /// # Platform-specific
  ///
  /// - **macOS**: Always `false`.
  #[must_use]
  pub fn is_lost(&self) -> bool {
    self.inner.is_lost()
  }

  /// Executes `update_fn` inside a compositor transaction.
  ///
  /// Used with [`AnimationWindow::update`] to commit all updates together
  /// when `update_fn` returns.
  pub fn transaction<F, R>(
    &self,
    update_fn: F,
    dispatcher: &Dispatcher,
  ) -> crate::Result<R>
  where
    F: FnOnce() -> R + Send,
    R: Send,
  {
    self.inner.transaction(update_fn, dispatcher)
  }

  /// Estimates when the compositor shows its next frame: when animations
  /// committed now start playing.
  ///
  /// # Platform-specific
  ///
  /// This method is only available on Windows.
  #[cfg(target_os = "windows")]
  pub fn next_frame_time(
    &self,
    dispatcher: &Dispatcher,
  ) -> crate::Result<Instant> {
    self.inner.next_frame_time(dispatcher)
  }
}

/// A screenshot of a [`NativeWindow`] that can be animated performantly.
///
/// # Example usage
///
///   1. Swap in the `AnimationWindow` with the `NativeWindow`,
///   2. Perform animation.
///   3. Swap out the `AnimationWindow`.
///
/// ```no_run,compile_fail
/// let frame = real_window.frame()?;
/// let anim_window = AnimationWindow::new(context, real_window, frame, /* .. */)?;
///
/// # Hide the real window at the animation end position.
/// real_window.set_frame(frame.translate_in_direction(Direction::Left, 100))?;
/// real_window.set_transparency(&OpacityValue::from_alpha(0))?;
///
/// for i in 1..100 {
///   context.transaction(|| {
///     anim_window.update(frame.translate_in_direction(Direction::Left, i), None))
///   })??;
/// }
///
/// real_window.set_transparency(&OpacityValue::from_alpha(u8::MAX));
/// anim_window.destroy()?;
/// ```
pub struct AnimationWindow {
  inner: platform_impl::AnimationWindow,
}

impl AnimationWindow {
  /// Creates a new [`AnimationWindow`].
  ///
  /// The `outer_rect` should span the bounds of the start and end
  /// rects of the animation.
  pub fn new(
    context: &AnimationContext,
    window: &NativeWindow,
    inner_rect: &Rect,
    outer_rect: &Rect,
    opacity: Option<OpacityValue>,
    dispatcher: &Dispatcher,
  ) -> crate::Result<Self> {
    let inner = platform_impl::AnimationWindow::new(
      &context.inner,
      window,
      inner_rect,
      outer_rect,
      opacity,
      dispatcher,
    )?;

    Ok(Self { inner })
  }

  /// Creates several [`AnimationWindow`]s at once.
  ///
  /// Prefer this over repeated calls to [`AnimationWindow::new`] when
  /// several windows start animating together: on Windows, their screen
  /// captures run in parallel.
  ///
  /// Returns one result per request, in the same order.
  #[must_use]
  pub fn new_many(
    context: &AnimationContext,
    requests: &[AnimationWindowRequest],
    dispatcher: &Dispatcher,
  ) -> Vec<crate::Result<Self>> {
    #[cfg(target_os = "windows")]
    let results = platform_impl::AnimationWindow::new_many(
      &context.inner,
      requests,
      dispatcher,
    );

    #[cfg(not(target_os = "windows"))]
    let results = requests
      .iter()
      .map(|request| {
        platform_impl::AnimationWindow::new(
          &context.inner,
          request.window,
          &request.inner_rect,
          &request.outer_rect,
          request.opacity,
          dispatcher,
        )
      })
      .collect::<Vec<_>>();

    results
      .into_iter()
      .map(|result| result.map(|inner| Self { inner }))
      .collect()
  }

  /// Resizes the window.
  ///
  /// Called when an animation's target rect changes mid-flight.
  pub fn resize(&mut self, outer_rect: &Rect) -> crate::Result<()> {
    self.inner.resize(outer_rect)
  }

  /// Updates the layer position and opacity within the window.
  ///
  /// Does not commit; should be called within
  /// `AnimationContext::transaction` for the change to take effect.
  pub fn update(
    &self,
    inner_rect: &Rect,
    opacity: Option<&OpacityValue>,
  ) -> crate::Result<()> {
    self.inner.update(inner_rect, opacity)
  }

  /// Plays an animation of the snapshot on the compositor's own clock, so
  /// every frame is drawn on time without the caller's help.
  ///
  /// Replaces any animation already playing. Does not commit; should be
  /// called within [`AnimationContext::transaction`], and the animation
  /// starts with the frame that shows the commit (see
  /// [`AnimationContext::next_frame_time`]). Each value holds at its end
  /// once its animation is over.
  ///
  /// # Platform-specific
  ///
  /// This method is only available on Windows.
  #[cfg(target_os = "windows")]
  pub fn animate(
    &self,
    animation: &OverlayAnimation,
  ) -> crate::Result<()> {
    self.inner.animate(animation)
  }

  /// Destroys the window and releases GPU resources.
  pub fn destroy(self) -> crate::Result<()> {
    self.inner.destroy()
  }
}

/// Arguments for one window in [`AnimationWindow::new_many`].
pub struct AnimationWindowRequest<'a> {
  /// Window to capture.
  pub window: &'a NativeWindow,

  /// Where the capture is first drawn.
  pub inner_rect: Rect,

  /// Bounds of the overlay; should span the animation's start and end
  /// rects.
  pub outer_rect: Rect,

  /// Initial opacity of the capture, if animated.
  pub opacity: Option<OpacityValue>,
}

/// Duration and curve of one part of an [`OverlayAnimation`].
#[derive(Clone, Debug)]
pub struct AnimationCurve {
  pub duration: Duration,
  pub easing: Easing,
}

/// A value animated from one end to another along a curve.
#[derive(Clone, Debug)]
pub struct AnimatedValue<T> {
  pub from: T,
  pub to: T,
  pub curve: AnimationCurve,
}

/// An animation of an [`AnimationWindow`]'s snapshot, for
/// [`AnimationWindow::animate`].
#[derive(Clone, Debug)]
pub struct OverlayAnimation {
  /// Where the snapshot is drawn (in screen coordinates), cut to the
  /// rect's size.
  pub rect: AnimatedValue<Rect>,

  /// The snapshot's opacity; left as it is if `None`.
  pub opacity: Option<AnimatedValue<OpacityValue>>,

  /// Scale of the whole snapshot about the rect's center, across and
  /// down (`1.0` is its own size); `1.0` if `None`.
  pub scale: Option<AnimatedValue<(f32, f32)>>,
}
