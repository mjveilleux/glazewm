use crate::CompositorFrameCounts;

/// Implements [`crate::compositor_frame_counts`].
///
/// macOS has no public counter of composed frames.
pub(crate) fn compositor_frame_counts() -> Option<CompositorFrameCounts> {
  None
}

/// Implements [`crate::system_animations_enabled`].
pub(crate) fn system_animations_enabled() -> bool {
  true
}
