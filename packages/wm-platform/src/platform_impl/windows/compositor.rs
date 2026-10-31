use windows::Win32::{
  Foundation::HWND,
  Graphics::Dwm::{DwmGetCompositionTimingInfo, DWM_TIMING_INFO},
};

use crate::CompositorFrameCounts;

/// Implements [`crate::compositor_frame_counts`].
pub(crate) fn compositor_frame_counts() -> Option<CompositorFrameCounts> {
  let mut info = DWM_TIMING_INFO {
    // LINT: The struct's size always fits in a `u32`.
    #[allow(clippy::cast_possible_truncation)]
    cbSize: std::mem::size_of::<DWM_TIMING_INFO>() as u32,
    ..Default::default()
  };

  // SAFETY: `info` is a valid, writable `DWM_TIMING_INFO` with `cbSize`
  // set, as the function requires. A null window asks for the desktop's
  // timing, the only kind available since Windows 8.1.
  match unsafe { DwmGetCompositionTimingInfo(HWND(0), &raw mut info) } {
    Ok(()) => Some(CompositorFrameCounts {
      composed: info.cFrame,
      dropped: info.cFramesDropped,
    }),
    Err(err) => {
      tracing::debug!("Failed to read compositor timing: {err}");
      None
    }
  }
}
