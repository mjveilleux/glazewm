use crate::platform_impl;

/// Counts of frames from the system compositor, for measuring how
/// smoothly an animation was shown.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CompositorFrameCounts {
  /// Frames the compositor has composed since it started.
  pub composed: u64,

  /// Frames the compositor has dropped since it started.
  pub dropped: u64,
}

/// Reads the system compositor's frame counts.
///
/// Compare two readings to see how many frames were composed and dropped
/// in between.
///
/// # Platform-specific
///
/// - **Windows**: From `DwmGetCompositionTimingInfo`, for the whole
///   desktop.
/// - **macOS**: Not supported; returns `None`.
#[must_use]
pub fn compositor_frame_counts() -> Option<CompositorFrameCounts> {
  platform_impl::compositor_frame_counts()
}

/// Whether the system lets apps animate, i.e. the user hasn't turned
/// animations off for accessibility.
///
/// # Platform-specific
///
/// - **Windows**: The "Animation effects" setting (Settings >
///   Accessibility > Visual effects), `SPI_GETCLIENTAREAANIMATION`.
/// - **macOS**: Always `true`.
#[must_use]
pub fn system_animations_enabled() -> bool {
  platform_impl::system_animations_enabled()
}

/// Blocks until the system compositor has shown its next frame.
///
/// For pacing work that must happen once per frame (e.g. moving a window
/// a little each frame) with the display instead of a timer.
///
/// # Platform-specific
///
/// This function is only available on Windows (`DwmFlush`).
#[cfg(target_os = "windows")]
pub fn wait_for_compositor_frame() -> crate::Result<()> {
  platform_impl::wait_for_compositor_frame()
}
