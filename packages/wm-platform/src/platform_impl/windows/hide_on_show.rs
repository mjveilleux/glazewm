use std::{
  collections::HashMap,
  sync::{
    atomic::{AtomicBool, Ordering},
    LazyLock, Mutex,
  },
};

use windows::Win32::{
  Foundation::HWND,
  Graphics::Gdi::{
    RedrawWindow, RDW_ALLCHILDREN, RDW_ERASE, RDW_FRAME, RDW_INVALIDATE,
  },
  UI::WindowsAndMessaging::{
    GetLayeredWindowAttributes, GetWindowLongPtrW, IsWindowVisible,
    SetLayeredWindowAttributes, SetWindowLongPtrW, GWL_EXSTYLE,
    LAYERED_WINDOW_ATTRIBUTES_FLAGS, LWA_ALPHA, WS_CAPTION, WS_CHILD,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
  },
};

use super::NativeWindow;
use crate::HiddenOnShow;

/// Whether windows are hidden as soon as they're shown.
static ENABLED: AtomicBool = AtomicBool::new(false);

/// Windows hidden on show that the WM hasn't taken over yet, keyed by
/// window handle.
static HIDDEN: LazyLock<Mutex<HashMap<isize, HiddenOnShow>>> =
  LazyLock::new(|| Mutex::new(HashMap::new()));

/// Implements [`crate::set_hide_windows_on_show`].
pub(crate) fn set_enabled(enabled: bool) {
  ENABLED.store(enabled, Ordering::Relaxed);
}

/// Hides a window that has just been shown, if it looks like one the WM
/// will manage.
///
/// Called from the window event hook, before the event reaches the WM, so
/// the window is hidden before it has been on screen for long.
pub(crate) fn hide_if_app_window(handle: isize) {
  if !ENABLED.load(Ordering::Relaxed) {
    return;
  }

  let window = NativeWindow::new(handle);

  // The same checks as the WM's own for whether to manage a window, as far
  // as they can be made this early.
  let is_app_window = unsafe { IsWindowVisible(HWND(handle)) }.as_bool()
    && !window.has_window_style(WS_CHILD)
    && !window.has_window_style_ex(WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW)
    && (!window.has_owner_window() || window.has_window_style(WS_CAPTION))
    && window.is_cloaked().is_ok_and(|cloaked| !cloaked);

  if !is_app_window {
    return;
  }

  let Ok(mut hidden) = HIDDEN.lock() else {
    return;
  };

  if hidden.contains_key(&handle) {
    return;
  }

  let was_layered = window.has_window_style_ex(WS_EX_LAYERED);
  let mut alpha = u8::MAX;

  if was_layered {
    let mut flags = LAYERED_WINDOW_ATTRIBUTES_FLAGS::default();

    // Leave alone windows that draw their own transparency (per-pixel
    // alpha or a color key): their opacity can't be put back as it was.
    let has_plain_alpha = unsafe {
      GetLayeredWindowAttributes(
        HWND(handle),
        None,
        Some(&raw mut alpha),
        Some(&raw mut flags),
      )
    }
    .is_ok()
      && (flags.0 & !LWA_ALPHA.0) == 0;

    if !has_plain_alpha {
      return;
    }
  }

  let state = HiddenOnShow { was_layered, alpha };

  if window
    .set_transparency(&crate::OpacityValue::from_alpha(0))
    .is_ok()
  {
    hidden.insert(handle, state);
  } else if !was_layered {
    remove_layered_style(handle);
  }
}

/// Implements [`crate::NativeWindowWindowsExt::take_hidden_on_show`].
pub(crate) fn take(handle: isize) -> Option<HiddenOnShow> {
  HIDDEN.lock().ok()?.remove(&handle)
}

/// Implements [`HiddenOnShow::restore`].
pub(crate) fn restore(handle: isize, state: HiddenOnShow) {
  if state.was_layered {
    let _ = unsafe {
      SetLayeredWindowAttributes(
        HWND(handle),
        None,
        state.alpha,
        LWA_ALPHA,
      )
    };
  } else {
    remove_layered_style(handle);
  }
}

/// Implements [`crate::restore_windows_hidden_on_show`].
pub(crate) fn restore_all() {
  let drained = match HIDDEN.lock() {
    Ok(mut hidden) => hidden.drain().collect::<Vec<_>>(),
    Err(_) => return,
  };

  for (handle, state) in drained {
    restore(handle, state);
  }
}

/// Removes `WS_EX_LAYERED` from a window and has it repaint, which makes
/// it fully opaque again.
fn remove_layered_style(handle: isize) {
  let hwnd = HWND(handle);
  let style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };

  #[allow(clippy::cast_possible_wrap)]
  let layered = WS_EX_LAYERED.0 as isize;

  if style & layered != 0 {
    unsafe {
      SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style & !layered);
      let _ = RedrawWindow(
        hwnd,
        None,
        None,
        RDW_ERASE | RDW_INVALIDATE | RDW_FRAME | RDW_ALLCHILDREN,
      );
    }
  }
}
