use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use crate::{
  models::{Container, WindowContainer, Workspace},
  traits::CommonGetters,
};

#[derive(Debug, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct PendingSync {
  /// Containers (and their descendants) that have a pending redraw.
  containers_to_redraw: HashMap<Uuid, Container>,

  /// Workspaces where z-order should be updated. Windows that match the
  /// focused window's state should be brought to the front.
  workspaces_to_reorder: Vec<Workspace>,

  /// Newly managed windows that should have an opening animation.
  open_animation_windows: Vec<WindowContainer>,

  /// Whether native focus should be reassigned to the WM's focused
  /// container.
  needs_focus_update: bool,

  /// Whether window effect for the focused window should be updated.
  needs_focused_effect_update: bool,

  /// Whether window effects for all windows should be updated.
  needs_all_effects_update: bool,

  /// IDs of windows whose own effect settings changed (e.g. through the
  /// `set-border-color` command), and whose effects should be updated
  /// whether or not they have focus.
  windows_to_update_effects: HashSet<Uuid>,

  /// Whether to jump the cursor to the focused container (if enabled in
  /// user config).
  needs_cursor_jump: bool,

  /// Whether to skip animations for the current sync.
  skip_animations: bool,
}

impl PendingSync {
  pub fn has_changes(&self) -> bool {
    !self.containers_to_redraw.is_empty()
      || !self.workspaces_to_reorder.is_empty()
      || !self.open_animation_windows.is_empty()
      || self.needs_focus_update
      || self.needs_focused_effect_update
      || self.needs_all_effects_update
      || !self.windows_to_update_effects.is_empty()
      || self.needs_cursor_jump
  }

  pub fn clear(&mut self) -> &mut Self {
    self.containers_to_redraw.clear();
    self.workspaces_to_reorder.clear();
    self.open_animation_windows.clear();
    self.needs_focus_update = false;
    self.needs_focused_effect_update = false;
    self.needs_all_effects_update = false;
    self.windows_to_update_effects.clear();
    self.needs_cursor_jump = false;
    self.skip_animations = false;
    self
  }

  pub fn queue_container_to_redraw<T>(&mut self, container: T) -> &mut Self
  where
    T: Into<Container>,
  {
    let container: Container = container.into();
    self.containers_to_redraw.insert(container.id(), container);
    self
  }

  pub fn queue_containers_to_redraw<I, T>(
    &mut self,
    containers: I,
  ) -> &mut Self
  where
    I: IntoIterator<Item = T>,
    T: Into<Container>,
  {
    for container in containers {
      let container: Container = container.into();
      self.containers_to_redraw.insert(container.id(), container);
    }

    self
  }

  pub fn dequeue_container_from_redraw<T>(
    &mut self,
    container: T,
  ) -> &mut Self
  where
    T: Into<Container>,
  {
    self.containers_to_redraw.remove(&container.into().id());
    self
  }

  pub fn queue_workspace_to_reorder(
    &mut self,
    workspace: Workspace,
  ) -> &mut Self {
    self.workspaces_to_reorder.push(workspace);
    self
  }

  pub fn queue_open_animation_window(
    &mut self,
    window: WindowContainer,
  ) -> &mut Self {
    self.open_animation_windows.push(window);
    self
  }

  pub fn queue_focus_change(&mut self) -> &mut Self {
    self.needs_focus_update = true;
    self
  }

  pub fn queue_focused_effect_update(&mut self) -> &mut Self {
    self.needs_focused_effect_update = true;
    self
  }

  pub fn queue_all_effects_update(&mut self) -> &mut Self {
    self.needs_all_effects_update = true;
    self
  }

  /// Queues an effects update for one window, focused or not.
  ///
  /// The window is looked up by ID at sync time, so that it is still
  /// found if it changes state (e.g. tiling to floating) in between.
  pub fn queue_window_effects_update(
    &mut self,
    window: &WindowContainer,
  ) -> &mut Self {
    self.windows_to_update_effects.insert(window.id());
    self
  }

  pub fn queue_cursor_jump(&mut self) -> &mut Self {
    self.needs_cursor_jump = true;
    self
  }

  pub fn set_skip_animations(&mut self, skip: bool) -> &mut Self {
    self.skip_animations = skip;
    self
  }

  pub fn should_skip_animations(&self) -> bool {
    self.skip_animations
  }

  pub fn needs_focus_update(&self) -> bool {
    self.needs_focus_update
  }

  pub fn needs_focused_effect_update(&self) -> bool {
    self.needs_focused_effect_update
  }

  pub fn needs_all_effects_update(&self) -> bool {
    self.needs_all_effects_update
  }

  pub fn needs_cursor_jump(&self) -> bool {
    self.needs_cursor_jump
  }

  /// IDs of windows queued with `queue_window_effects_update`.
  pub fn windows_to_update_effects(&self) -> &HashSet<Uuid> {
    &self.windows_to_update_effects
  }

  pub fn containers_to_redraw(&self) -> &HashMap<Uuid, Container> {
    &self.containers_to_redraw
  }

  pub fn workspaces_to_reorder(&self) -> &Vec<Workspace> {
    &self.workspaces_to_reorder
  }

  pub fn open_animation_windows(&self) -> &Vec<WindowContainer> {
    &self.open_animation_windows
  }
}
