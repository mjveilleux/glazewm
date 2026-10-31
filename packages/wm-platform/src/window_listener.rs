use tokio::sync::mpsc;

use crate::{platform_impl, Dispatcher, WindowEvent};

/// A listener for system-wide window events.
pub struct WindowListener {
  event_rx: mpsc::UnboundedReceiver<WindowEvent>,

  /// Inner platform-specific window listener.
  inner: platform_impl::WindowListener,
}

impl WindowListener {
  /// Creates a new window listener.
  pub fn new(dispatcher: &Dispatcher) -> crate::Result<Self> {
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let inner = platform_impl::WindowListener::new(event_tx, dispatcher)?;

    Ok(Self { event_rx, inner })
  }

  /// Returns the next window event from the listener.
  ///
  /// This will block until a window event is available.
  pub async fn next_event(&mut self) -> Option<WindowEvent> {
    self.event_rx.recv().await
  }

  /// Terminates the window listener.
  pub fn terminate(&mut self) {
    self.inner.terminate();
  }
}

/// How a window looked before it was hidden on show.
///
/// See [`set_hide_windows_on_show`].
///
/// # Platform-specific
///
/// This type is only available on Windows.
#[cfg(target_os = "windows")]
#[derive(Clone, Copy, Debug)]
pub struct HiddenOnShow {
  /// Whether the window was already layered (`WS_EX_LAYERED`).
  pub(crate) was_layered: bool,

  /// The window's opacity, if it was layered.
  pub(crate) alpha: u8,
}

#[cfg(target_os = "windows")]
impl HiddenOnShow {
  /// Gives the window back the opacity it had before it was hidden.
  pub fn restore(self, window: &crate::NativeWindow) {
    use crate::NativeWindowWindowsExt;

    platform_impl::hide_on_show::restore(window.hwnd().0, self);
  }
}

/// Whether to hide windows that look like new app windows the moment
/// they're shown, before the WM has handled them.
///
/// The WM then takes each one over with
/// [`crate::NativeWindowWindowsExt::take_hidden_on_show`], and either
/// makes it visible itself (e.g. fading it in once it's in place) or
/// restores it with [`HiddenOnShow::restore`]. Without this, a new window
/// shows at the position its app gave it until the WM has moved it.
///
/// # Platform-specific
///
/// This function is only available on Windows.
#[cfg(target_os = "windows")]
pub fn set_hide_windows_on_show(enabled: bool) {
  platform_impl::hide_on_show::set_enabled(enabled);
}

/// Restores every window hidden on show that the WM hasn't taken over.
///
/// For when the WM exits.
///
/// # Platform-specific
///
/// This function is only available on Windows.
#[cfg(target_os = "windows")]
pub fn restore_windows_hidden_on_show() {
  platform_impl::hide_on_show::restore_all();
}
