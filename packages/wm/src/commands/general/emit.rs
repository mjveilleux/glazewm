use wm_common::WmEvent;
#[cfg(target_os = "windows")]
use wm_platform::DispatcherExtWindows;

use crate::wm_state::WmState;

/// Broadcasts a custom event to IPC subscribers (the `emit` command).
///
/// Runs nothing itself: a subscriber (e.g. a shell listening for its own
/// event name) acts on it. Works while the WM is paused.
pub fn emit(name: &str, args: &[String], state: &WmState) {
  // The subscriber usually brings a window of its own to the front next
  // (e.g. a launcher opened by a key). It isn't the foreground process,
  // so it may only do that if this process passes on its right to.
  #[cfg(target_os = "windows")]
  if let Err(err) = state.dispatcher.allow_any_foreground() {
    tracing::debug!("Could not pass on the foreground right: {err}");
  }

  tracing::info!("Emitting custom event '{name}' {args:?}.");

  state.emit_event(WmEvent::Custom {
    name: name.to_string(),
    args: args.to_vec(),
  });
}
