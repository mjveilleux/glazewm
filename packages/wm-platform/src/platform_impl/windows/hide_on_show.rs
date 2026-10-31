use std::{
  collections::HashMap,
  sync::{
    atomic::{AtomicBool, Ordering},
    LazyLock, Mutex, Once,
  },
  time::{Duration, Instant},
};

use windows::{
  core::{w, PCWSTR},
  Win32::{
    Foundation::{BOOL, HANDLE, HWND, LPARAM},
    Graphics::Gdi::{
      RedrawWindow, RDW_ALLCHILDREN, RDW_ERASE, RDW_FRAME, RDW_INVALIDATE,
    },
    UI::{
      Accessibility::{SetWinEventHook, HWINEVENTHOOK},
      WindowsAndMessaging::{
        DispatchMessageW, EnumWindows, GetLayeredWindowAttributes,
        GetMessageW, GetPropW, GetWindowLongPtrW, IsWindowVisible,
        RemovePropW, SetLayeredWindowAttributes, SetPropW,
        SetWindowLongPtrW, TranslateMessage, EVENT_OBJECT_SHOW,
        GWL_EXSTYLE, LAYERED_WINDOW_ATTRIBUTES_FLAGS, LWA_ALPHA, MSG,
        OBJID_WINDOW, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
        WS_CAPTION, WS_CHILD, WS_EX_LAYERED, WS_EX_NOACTIVATE,
        WS_EX_TOOLWINDOW,
      },
    },
  },
};

use super::NativeWindow;
use crate::HiddenOnShow;

/// Window property marking a window hidden on show, holding how to
/// restore it (see `mark`). Lets the watcher find and restore such windows
/// if the WM dies before taking them over, since the WM's own record of
/// them dies with it.
const MARK: PCWSTR = w!("GlazeWM.HiddenOnShow");

/// Whether windows are hidden as soon as they're shown.
static ENABLED: AtomicBool = AtomicBool::new(false);

/// Windows hidden on show that the WM hasn't taken over yet, keyed by
/// window handle.
static HIDDEN: LazyLock<Mutex<HashMap<isize, HiddenOnShow>>> =
  LazyLock::new(|| Mutex::new(HashMap::new()));

/// Windows whose latest show event has been handled, and when: the event
/// reaches both the WM's own hook and the watcher thread's (see
/// `watch_shows`), and only the first to get it may hide the window.
/// Otherwise the second could hide a window again that the WM has already
/// taken over and shown.
static HANDLED: LazyLock<Mutex<HashMap<isize, Instant>>> =
  LazyLock::new(|| Mutex::new(HashMap::new()));

/// Serializes `hide_if_app_window` between the two hooks.
static HIDING: Mutex<()> = Mutex::new(());

/// How long a show stays handled: longer than either hook takes to get
/// the event.
const HANDLED_FOR: Duration = Duration::from_secs(1);

/// Implements [`crate::set_hide_windows_on_show`].
pub(crate) fn set_enabled(enabled: bool) {
  ENABLED.store(enabled, Ordering::Relaxed);

  if enabled {
    watch_shows();
  }
}

/// Starts, once, a thread whose only job is hiding new windows the moment
/// they're shown.
///
/// The WM's own event hook runs on its busy main thread, which also
/// captures and draws animations: there a new window could stay on screen
/// for a few frames, where its app put it, before being hidden. This
/// thread gets the show event as soon as it's sent.
fn watch_shows() {
  static STARTED: Once = Once::new();

  STARTED.call_once(|| {
    let spawned = std::thread::Builder::new()
      .name("hide-on-show".to_string())
      .spawn(|| {
        // SAFETY: Hooks show events for this thread, with a callback that
        // only looks at and hides windows; the hook lives as long as the
        // thread, which pumps its messages below.
        let hook = unsafe {
          SetWinEventHook(
            EVENT_OBJECT_SHOW,
            EVENT_OBJECT_SHOW,
            None,
            Some(on_show),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
          )
        };

        if hook.is_invalid() {
          tracing::warn!("Failed to watch for windows being shown.");
          return;
        }

        // Out-of-context hooks are called from this thread's message
        // loop.
        let mut msg = MSG::default();

        // SAFETY: `msg` is a valid, writable `MSG` for each call.
        while unsafe { GetMessageW(&raw mut msg, None, 0, 0) }.as_bool() {
          // SAFETY: `msg` was just filled in by `GetMessageW`.
          unsafe {
            let _ = TranslateMessage(&raw const msg);
            DispatchMessageW(&raw const msg);
          }
        }
      });

    if let Err(err) = spawned {
      tracing::warn!("Failed to start the hide-on-show thread: {err}");
    }
  });
}

/// Callback of the hook set by `watch_shows`.
extern "system" fn on_show(
  _hook: HWINEVENTHOOK,
  _event_type: u32,
  handle: HWND,
  id_object: i32,
  id_child: i32,
  _event_thread: u32,
  _event_time: u32,
) {
  if id_object == OBJID_WINDOW.0 && id_child == 0 && handle != HWND(0) {
    hide_if_app_window(handle.0);
  }
}

/// Marks the window's latest show as handled.
///
/// Returns `false` if it already was, by the other hook.
fn claim_show(handle: isize) -> bool {
  let Ok(mut handled) = HANDLED.lock() else {
    return false;
  };

  let now = Instant::now();
  handled.retain(|_, at| now.duration_since(*at) < HANDLED_FOR);

  if handled.contains_key(&handle) {
    return false;
  }

  handled.insert(handle, now);
  true
}

/// Hides a window that has just been shown, if it looks like one the WM
/// will manage.
///
/// Called from the hide-on-show thread (see `watch_shows`) and from the
/// WM's window event hook, before the event reaches the WM, so the window
/// is hidden before it has been on screen for long. Whichever gets the
/// show first handles it.
pub(crate) fn hide_if_app_window(handle: isize) {
  if !ENABLED.load(Ordering::Relaxed) {
    return;
  }

  // Held throughout, so a hook that finds the show claimed by the other
  // returns (and lets the event reach the WM) only once the window is
  // hidden and recorded: the WM then always finds it to take over.
  let Ok(_hiding) = HIDING.lock() else {
    return;
  };

  if !claim_show(handle) {
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
    mark(handle, state);
    hidden.insert(handle, state);
  } else if !was_layered {
    remove_layered_style(handle);
  }
}

/// Implements [`crate::NativeWindowWindowsExt::take_hidden_on_show`].
pub(crate) fn take(handle: isize) -> Option<HiddenOnShow> {
  let state = HIDDEN.lock().ok()?.remove(&handle)?;
  unmark(handle);
  Some(state)
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
    unmark(handle);
    restore(handle, state);
  }
}

/// Implements [`crate::restore_windows_hidden_by_lost_wm`].
pub(crate) fn restore_marked() {
  /// Restores one window if it's marked.
  unsafe extern "system" fn restore_if_marked(
    hwnd: HWND,
    _: LPARAM,
  ) -> BOOL {
    // SAFETY: Reading a window property of any window is allowed.
    let value = unsafe { GetPropW(hwnd, MARK) }.0;

    if value != 0 {
      unmark(hwnd.0);
      restore(hwnd.0, unpack(value));
    }

    BOOL(1)
  }

  // SAFETY: The callback only reads and removes a property, and restores
  // the window's opacity.
  if let Err(err) =
    unsafe { EnumWindows(Some(restore_if_marked), LPARAM(0)) }
  {
    tracing::warn!("Failed to look for windows hidden on show: {err}");
  }
}

/// Marks a window as hidden on show, with how to restore it.
fn mark(handle: isize, state: HiddenOnShow) {
  // SAFETY: Window properties can be set on any window; the value is a
  // plain number, not a pointer.
  if let Err(err) =
    unsafe { SetPropW(HWND(handle), MARK, HANDLE(pack(state))) }
  {
    tracing::debug!("Failed to mark window hidden on show: {err}");
  }
}

/// Removes the mark of `mark`.
fn unmark(handle: isize) {
  // SAFETY: Removing a property that's not there is harmless.
  let _ = unsafe { RemovePropW(HWND(handle), MARK) };
}

/// Packs how to restore a window into a non-zero property value: the
/// alpha, plus 256 if it was layered, plus 512 so it's never 0.
fn pack(state: HiddenOnShow) -> isize {
  isize::from(state.alpha) + if state.was_layered { 256 } else { 0 } + 512
}

/// Unpacks a property value packed by `pack`.
fn unpack(value: isize) -> HiddenOnShow {
  let value = value - 512;

  HiddenOnShow {
    was_layered: value & 256 != 0,
    // LINT: Masked to the low byte.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    alpha: (value & 0xff) as u8,
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

#[cfg(test)]
mod tests {
  use super::{pack, unpack};
  use crate::HiddenOnShow;

  #[test]
  fn mark_values_round_trip() {
    for (was_layered, alpha) in [(false, 255), (true, 0), (true, 200)] {
      let state = HiddenOnShow { was_layered, alpha };
      let value = pack(state);
      let unpacked = unpack(value);

      assert_ne!(value, 0);
      assert_eq!(unpacked.was_layered, was_layered);
      assert_eq!(unpacked.alpha, alpha);
    }
  }
}
