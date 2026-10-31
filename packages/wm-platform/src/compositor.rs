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
