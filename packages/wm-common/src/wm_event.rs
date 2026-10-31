use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
  dtos::ContainerDto,
  parsed_config::{BindingModeConfig, ParsedConfig},
  TilingDirection,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
  tag = "eventType",
  rename_all = "snake_case",
  rename_all_fields = "camelCase"
)]
pub enum WmEvent {
  ApplicationExiting,
  BindingModesChanged {
    new_binding_modes: Vec<BindingModeConfig>,
  },
  FocusChanged {
    focused_container: ContainerDto,
  },
  FocusedContainerMoved {
    focused_container: ContainerDto,
  },
  MonitorAdded {
    added_monitor: ContainerDto,
  },
  MonitorRemoved {
    removed_id: Uuid,
    removed_device_name: String,
  },
  MonitorUpdated {
    updated_monitor: ContainerDto,
  },
  TilingDirectionChanged {
    direction_container: ContainerDto,
    new_tiling_direction: TilingDirection,
  },
  UserConfigChanged {
    config_path: String,
    config_string: String,
    parsed_config: ParsedConfig,
  },
  WindowManaged {
    managed_window: ContainerDto,
  },
  WindowUnmanaged {
    unmanaged_id: Uuid,
    unmanaged_handle: isize,
  },
  WorkspaceActivated {
    activated_workspace: ContainerDto,
  },
  WorkspaceDeactivated {
    deactivated_id: Uuid,
    deactivated_name: String,
  },
  WorkspaceUpdated {
    updated_workspace: ContainerDto,
  },
  PauseChanged {
    is_paused: bool,
  },
  /// A custom event from the `emit` command.
  Custom {
    /// Name of the event; subscribers filter on it.
    name: String,
    /// Words passed along with the event.
    args: Vec<String>,
  },
}

#[cfg(test)]
mod tests {
  use super::WmEvent;

  #[test]
  fn custom_event_serializes_for_subscribers() {
    let event = WmEvent::Custom {
      name: "omawin".into(),
      args: vec!["launcher".into(), "clipboard".into()],
    };

    assert_eq!(
      serde_json::to_value(&event).expect("event should serialize"),
      serde_json::json!({
        "eventType": "custom",
        "name": "omawin",
        "args": ["launcher", "clipboard"],
      })
    );
  }
}
