use windows::Win32::{
  Foundation::{BOOL, HWND},
  Graphics::Dwm::{
    DwmFlush, DwmGetCompositionTimingInfo, DWM_TIMING_INFO,
  },
  UI::WindowsAndMessaging::{
    SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
  },
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

/// Implements [`crate::system_animations_enabled`].
///
/// Animations are allowed if the setting can't be read.
pub(crate) fn system_animations_enabled() -> bool {
  let mut enabled = BOOL(1);

  // SAFETY: `SPI_GETCLIENTAREAANIMATION` writes one `BOOL` to the given
  // pointer, which points at `enabled`.
  let result = unsafe {
    SystemParametersInfoW(
      SPI_GETCLIENTAREAANIMATION,
      0,
      Some((&raw mut enabled).cast()),
      SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
    )
  };

  if let Err(err) = result {
    tracing::debug!("Failed to read the animation effects setting: {err}");
    return true;
  }

  enabled.as_bool()
}

/// Implements [`crate::wait_for_compositor_frame`].
pub(crate) fn wait_for_compositor_frame() -> crate::Result<()> {
  // SAFETY: `DwmFlush` takes no arguments and only blocks the calling
  // thread.
  unsafe { DwmFlush() }?;

  Ok(())
}
