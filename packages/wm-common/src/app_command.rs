use std::{iter, path::PathBuf};

use clap::{error::KindFormatter, Args, Parser, ValueEnum};
use serde::{Deserialize, Deserializer, Serialize};
use tracing::Level;
use uuid::Uuid;
use wm_platform::{Color, Delta, Direction, LengthValue, OpacityValue};

use crate::TilingDirection;

const VERSION: &str = env!("VERSION_NUMBER");

#[derive(Clone, Debug, Parser)]
#[clap(name = "glazewm", author, version = VERSION, about, long_about = None)]
pub enum AppCommand {
  /// Starts the window manager.
  Start {
    /// Custom path to user config file.
    ///
    /// The default path is `%userprofile%/.glzr/glazewm/config.yaml`
    #[clap(short = 'c', long = "config", value_hint = clap::ValueHint::FilePath)]
    config_path: Option<PathBuf>,

    #[clap(flatten)]
    verbosity: Verbosity,
  },

  /// Retrieves and outputs a specific part of the window manager's state.
  ///
  /// Requires an already running instance of the window manager.
  #[clap(alias = "q")]
  Query {
    #[clap(subcommand)]
    command: QueryCommand,
  },

  /// Invokes a window manager command.
  ///
  /// Requires an already running instance of the window manager.
  #[clap(alias = "c")]
  Command {
    #[clap(long = "id")]
    subject_container_id: Option<Uuid>,

    #[clap(subcommand)]
    command: InvokeCommand,
  },

  /// Subscribes to one or more WM events (e.g. `window_close`), and
  /// continuously outputs the incoming events.
  ///
  /// Requires an already running instance of the window manager.
  Sub {
    /// WM event(s) to subscribe to.
    #[clap(short = 'e', long, value_enum, num_args = 1..)]
    events: Vec<SubscribableEvent>,
  },

  /// Unsubscribes from a prior event subscription.
  ///
  /// Requires an already running instance of the window manager.
  Unsub {
    /// Subscription ID to unsubscribe from.
    #[clap(long = "id")]
    subscription_id: Uuid,
  },
}

impl AppCommand {
  /// Parses `AppCommand` from command line arguments.
  ///
  /// Defaults to `AppCommand::Start` if no arguments are provided.
  #[must_use]
  pub fn parse_with_default(args: &Vec<String>) -> Self {
    if args.len() == 1 {
      AppCommand::Start {
        config_path: None,
        verbosity: Verbosity {
          verbose: false,
          quiet: false,
        },
      }
    } else {
      AppCommand::parse_from(args)
    }
  }
}

/// Verbosity flags to be used with `#[command(flatten)]`.
#[derive(Args, Clone, Debug)]
#[clap(about = None, long_about = None)]
pub struct Verbosity {
  /// Enables verbose logging.
  #[clap(short = 'v', long, action)]
  verbose: bool,

  /// Disables logging.
  #[clap(short = 'q', long, action, conflicts_with = "verbose")]
  quiet: bool,
}

impl Verbosity {
  /// Gets the log level based on the verbosity flags.
  #[must_use]
  pub fn level(&self) -> Level {
    match (self.verbose, self.quiet) {
      (true, _) => Level::DEBUG,
      (_, true) => Level::ERROR,
      _ => Level::INFO,
    }
  }
}

#[derive(Clone, Debug, Parser)]
pub enum QueryCommand {
  /// Outputs metadata about the application (e.g. version number).
  AppMetadata,
  /// Outputs the active binding modes.
  BindingModes,
  /// Outputs the focused container (either a window or an empty
  /// workspace).
  Focused,
  /// Outputs the tiling direction of the focused container.
  TilingDirection,
  /// Outputs all monitors.
  Monitors,
  /// Outputs all windows.
  Windows,
  /// Outputs all active workspaces.
  Workspaces,
  /// Outputs whether the window manager is paused.
  Paused,
}

#[derive(Clone, Debug, PartialEq, ValueEnum)]
#[clap(rename_all = "snake_case")]
pub enum SubscribableEvent {
  All,
  ApplicationExiting,
  BindingModesChanged,
  FocusChanged,
  FocusedContainerMoved,
  MonitorAdded,
  MonitorUpdated,
  MonitorRemoved,
  TilingDirectionChanged,
  UserConfigChanged,
  WindowManaged,
  WindowUnmanaged,
  WorkspaceActivated,
  WorkspaceDeactivated,
  WorkspaceUpdated,
  PauseChanged,
  /// Custom events from the `emit` command.
  Custom,
}

#[derive(Clone, Debug, Parser, PartialEq, Serialize)]
pub enum InvokeCommand {
  AdjustBorders(InvokeAdjustBordersCommand),
  Close,
  /// Broadcasts a custom event to IPC subscribers, without running
  /// anything (e.g. `emit omawin launcher` asks omawin's shell to open
  /// its launcher).
  Emit {
    /// Name of the event; subscribers filter on it.
    #[clap(required = true)]
    name: String,

    /// Words passed along with the event, flags included.
    #[clap(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
  },
  Focus(InvokeFocusCommand),
  Ignore,
  Move(InvokeMoveCommand),
  MoveWorkspace {
    #[clap(long)]
    direction: Direction,
  },
  Position(InvokePositionCommand),
  Resize(InvokeResizeCommand),
  UpdateWorkspaceConfig {
    #[clap(long, allow_hyphen_values = true)]
    workspace: Option<String>,
    #[clap(flatten)]
    new_config: InvokeUpdateWorkspaceConfig,
  },
  /// Sets the border color of the subject window, in place of the
  /// `window_effects` border colors (e.g. `set-border-color #a6e3a1` in a
  /// window rule).
  SetBorderColor(SetBorderColorCommand),
  SetFloating {
    #[clap(long, default_missing_value = "true", require_equals = true, num_args = 0..=1)]
    shown_on_top: Option<bool>,

    #[clap(long, default_missing_value = "true", require_equals = true, num_args = 0..=1)]
    centered: Option<bool>,

    #[clap(long, allow_hyphen_values = true)]
    x_pos: Option<i32>,

    #[clap(long, allow_hyphen_values = true)]
    y_pos: Option<i32>,

    #[clap(long, allow_hyphen_values = true)]
    width: Option<LengthValue>,

    #[clap(long, allow_hyphen_values = true)]
    height: Option<LengthValue>,
  },
  SetFullscreen {
    #[clap(long, default_missing_value = "true", require_equals = true, num_args = 0..=1)]
    shown_on_top: Option<bool>,

    #[clap(long, default_missing_value = "true", require_equals = true, num_args = 0..=1)]
    maximized: Option<bool>,
  },
  SetMinimized,
  SetTiling,
  SetTitleBarVisibility {
    #[clap(required = true, value_enum)]
    visibility: TitleBarVisibility,
  },
  SetTransparency(SetTransparencyCommand),
  ShellExec {
    #[clap(long, action)]
    hide_window: bool,

    #[clap(required = true, trailing_var_arg = true)]
    command: Vec<String>,
  },
  // Reuse `InvokeResizeCommand` struct.
  Size(InvokeResizeCommand),
  ToggleFloating {
    #[clap(long, default_missing_value = "true", require_equals = true, num_args = 0..=1)]
    shown_on_top: Option<bool>,

    #[clap(long, default_missing_value = "true", require_equals = true, num_args = 0..=1)]
    centered: Option<bool>,
  },
  ToggleFullscreen {
    #[clap(long, default_missing_value = "true", require_equals = true, num_args = 0..=1)]
    shown_on_top: Option<bool>,

    #[clap(long, default_missing_value = "true", require_equals = true, num_args = 0..=1)]
    maximized: Option<bool>,
  },
  ToggleMinimized,
  ToggleTiling,
  ToggleTilingDirection,
  SetTilingDirection {
    #[clap(required = true)]
    tiling_direction: TilingDirection,
  },
  WmCycleFocus {
    #[clap(long, default_value_t = false)]
    omit_floating: bool,

    #[clap(long, default_value_t = false)]
    omit_fullscreen: bool,

    #[clap(long, default_value_t = true)]
    omit_minimized: bool,

    #[clap(long, default_value_t = false)]
    omit_tiling: bool,
  },
  WmDisableBindingMode {
    #[clap(long)]
    name: String,
  },
  WmEnableBindingMode {
    #[clap(long)]
    name: String,
  },
  WmExit,
  WmRedraw,
  WmReloadConfig,
  WmTogglePause,
}

impl InvokeCommand {
  /// Whether the command runs while the WM is paused.
  ///
  /// Unpausing must stay possible, and `emit` only notifies IPC
  /// subscribers (it moves no window), so both pass.
  #[must_use]
  pub fn runs_while_paused(&self) -> bool {
    matches!(
      self,
      InvokeCommand::WmTogglePause | InvokeCommand::Emit { .. }
    )
  }
}

impl<'de> Deserialize<'de> for InvokeCommand {
  fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
  where
    D: Deserializer<'de>,
  {
    // Clap expects an array of string slices where the first argument is
    // the binary name/path. When deserializing commands from the user
    // config, we therefore have to prepend an additional empty argument.
    let unparsed = String::deserialize(deserializer)?;
    let unparsed_split = iter::once("").chain(unparsed.split_whitespace());

    InvokeCommand::try_parse_from(unparsed_split).map_err(|err| {
      // Format the error message and remove the "error: " prefix.
      let err_msg = err.apply::<KindFormatter>().to_string();
      serde::de::Error::custom(err_msg.trim_start_matches("error: "))
    })
  }
}

#[derive(Clone, Debug, PartialEq, Serialize, ValueEnum)]
#[clap(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum TitleBarVisibility {
  Shown,
  Hidden,
}

#[derive(Args, Clone, Debug, PartialEq, Serialize)]
#[group(required = true, multiple = true)]
pub struct InvokeAdjustBordersCommand {
  #[clap(long, allow_hyphen_values = true)]
  pub top: Option<LengthValue>,

  #[clap(long, allow_hyphen_values = true)]
  pub right: Option<LengthValue>,

  #[clap(long, allow_hyphen_values = true)]
  pub bottom: Option<LengthValue>,

  #[clap(long, allow_hyphen_values = true)]
  pub left: Option<LengthValue>,
}

#[derive(Args, Clone, Debug, PartialEq, Serialize)]
#[group(required = true, multiple = false)]
#[allow(clippy::struct_excessive_bools)]
pub struct InvokeFocusCommand {
  #[clap(long)]
  pub direction: Option<Direction>,

  #[clap(long)]
  pub container_id: Option<Uuid>,

  #[clap(long)]
  pub workspace_in_direction: Option<Direction>,

  #[clap(long)]
  pub workspace: Option<String>,

  #[clap(long)]
  pub monitor: Option<usize>,

  #[clap(long)]
  pub next_active_workspace: bool,

  #[clap(long)]
  pub prev_active_workspace: bool,

  #[clap(long)]
  pub next_workspace: bool,

  #[clap(long)]
  pub prev_workspace: bool,

  #[clap(long)]
  pub next_active_workspace_on_monitor: bool,

  #[clap(long)]
  pub prev_active_workspace_on_monitor: bool,

  #[clap(long)]
  pub recent_workspace: bool,
}

#[derive(Args, Clone, Debug, PartialEq, Serialize)]
#[group(required = true, multiple = false)]
#[allow(clippy::struct_excessive_bools)]
pub struct InvokeMoveCommand {
  /// Direction to move the window.
  #[clap(long)]
  pub direction: Option<Direction>,

  /// Move window to workspace in specified direction.
  #[clap(long)]
  pub workspace_in_direction: Option<Direction>,

  /// Name of workspace to move the window.
  #[clap(long)]
  pub workspace: Option<String>,

  #[clap(long)]
  pub next_active_workspace: bool,

  #[clap(long)]
  pub prev_active_workspace: bool,

  #[clap(long)]
  pub next_workspace: bool,

  #[clap(long)]
  pub prev_workspace: bool,

  #[clap(long)]
  pub next_active_workspace_on_monitor: bool,

  #[clap(long)]
  pub prev_active_workspace_on_monitor: bool,

  #[clap(long)]
  pub recent_workspace: bool,
}

#[derive(Args, Clone, Debug, PartialEq, Serialize)]
#[group(required = true, multiple = true)]
pub struct InvokeResizeCommand {
  #[clap(long, allow_hyphen_values = true)]
  pub width: Option<LengthValue>,

  #[clap(long, allow_hyphen_values = true)]
  pub height: Option<LengthValue>,
}

#[derive(Args, Clone, Debug, PartialEq, Serialize)]
#[group(required = true, multiple = true)]
pub struct SetTransparencyCommand {
  #[clap(long)]
  pub opacity: Option<OpacityValue>,

  #[clap(long, allow_hyphen_values = true)]
  pub opacity_delta: Option<Delta<OpacityValue>>,
}

#[derive(Args, Clone, Debug, PartialEq, Serialize)]
pub struct SetBorderColorCommand {
  /// Border color while the window has focus, as `#RRGGBB` or
  /// `#RRGGBBAA`. Also used while unfocused, unless `--unfocused` is
  /// given.
  #[clap(required_unless_present = "reset")]
  pub color: Option<Color>,

  /// Border color while the window doesn't have focus.
  #[clap(long, requires = "color")]
  pub unfocused: Option<Color>,

  /// Clears the window's border color, so that the `window_effects`
  /// border colors apply again.
  #[clap(long, action, conflicts_with_all = ["color", "unfocused"])]
  pub reset: bool,
}

#[derive(Args, Clone, Debug, PartialEq, Serialize)]
#[group(required = true, multiple = true)]
pub struct InvokePositionCommand {
  #[clap(long, action)]
  pub centered: bool,

  #[clap(long, allow_hyphen_values = true)]
  pub x_pos: Option<i32>,

  #[clap(long, allow_hyphen_values = true)]
  pub y_pos: Option<i32>,
}

#[derive(Args, Clone, Debug, PartialEq, Serialize)]
#[group(required = true, multiple = true)]
pub struct InvokeUpdateWorkspaceConfig {
  #[clap(long, allow_hyphen_values = true)]
  pub name: Option<String>,

  #[clap(long, allow_hyphen_values = true)]
  pub display_name: Option<String>,

  #[clap(long)]
  pub bind_to_monitor: Option<u32>,

  #[clap(long)]
  pub keep_alive: Option<bool>,
}

#[cfg(test)]
mod tests {
  use std::iter;

  use clap::Parser;
  use wm_platform::Color;

  use super::{
    AppCommand, InvokeCommand, SetBorderColorCommand, SubscribableEvent,
  };

  /// Parses a command the way the user config does (see `Deserialize`).
  fn parse(command: &str) -> InvokeCommand {
    InvokeCommand::try_parse_from(
      iter::once("").chain(command.split_whitespace()),
    )
    .expect("command should parse")
  }

  #[test]
  fn emit_takes_name_and_words() {
    assert_eq!(
      parse("emit omawin launcher clipboard"),
      InvokeCommand::Emit {
        name: "omawin".into(),
        args: vec!["launcher".into(), "clipboard".into()],
      }
    );
  }

  #[test]
  fn emit_takes_a_name_alone() {
    assert_eq!(
      parse("emit omawin"),
      InvokeCommand::Emit {
        name: "omawin".into(),
        args: vec![],
      }
    );
  }

  #[test]
  fn emit_passes_flags_through() {
    assert_eq!(
      parse("emit omawin --open install -x"),
      InvokeCommand::Emit {
        name: "omawin".into(),
        args: vec!["--open".into(), "install".into(), "-x".into()],
      }
    );
  }

  #[test]
  fn emit_needs_a_name() {
    assert!(InvokeCommand::try_parse_from(["", "emit"]).is_err());
  }

  #[test]
  fn emit_parses_from_the_config() {
    let command: InvokeCommand =
      serde_json::from_str("\"emit omawin launcher audio\"")
        .expect("config command should parse");

    assert_eq!(command, parse("emit omawin launcher audio"));
  }

  #[test]
  fn custom_events_can_be_subscribed_to() {
    let command =
      AppCommand::try_parse_from(["glazewm", "sub", "--events", "custom"])
        .expect("sub should parse");

    assert!(matches!(
      command,
      AppCommand::Sub { events } if events == vec![SubscribableEvent::Custom]
    ));
  }

  #[test]
  fn only_pause_and_emit_run_while_paused() {
    assert!(parse("wm-toggle-pause").runs_while_paused());
    assert!(parse("emit omawin launcher").runs_while_paused());
    assert!(!parse("close").runs_while_paused());
    assert!(!parse("shell-exec omawin.exe launcher").runs_while_paused());
  }

  /// Builds an opaque color from its RGB components.
  fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color { r, g, b, a: 255 }
  }

  #[test]
  fn set_border_color_takes_a_color() {
    assert_eq!(
      parse("set-border-color #a6e3a1"),
      InvokeCommand::SetBorderColor(SetBorderColorCommand {
        color: Some(rgb(0xa6, 0xe3, 0xa1)),
        unfocused: None,
        reset: false,
      })
    );
  }

  #[test]
  fn set_border_color_takes_an_unfocused_color() {
    assert_eq!(
      parse("set-border-color #a6e3a1 --unfocused #40a02b80"),
      InvokeCommand::SetBorderColor(SetBorderColorCommand {
        color: Some(rgb(0xa6, 0xe3, 0xa1)),
        unfocused: Some(Color {
          r: 0x40,
          g: 0xa0,
          b: 0x2b,
          a: 0x80,
        }),
        reset: false,
      })
    );
  }

  #[test]
  fn set_border_color_resets() {
    assert_eq!(
      parse("set-border-color --reset"),
      InvokeCommand::SetBorderColor(SetBorderColorCommand {
        color: None,
        unfocused: None,
        reset: true,
      })
    );
  }

  #[test]
  fn set_border_color_rejects_bad_input() {
    for command in [
      "set-border-color",
      "set-border-color a6e3a1",
      "set-border-color #a6e3a",
      "set-border-color #zzzzzz",
      "set-border-color --unfocused #a6e3a1",
      "set-border-color #a6e3a1 --unfocused",
      "set-border-color #a6e3a1 --reset",
      "set-border-color --reset --unfocused #a6e3a1",
    ] {
      assert!(
        InvokeCommand::try_parse_from(
          iter::once("").chain(command.split_whitespace())
        )
        .is_err(),
        "{command} should fail"
      );
    }
  }

  #[test]
  fn set_border_color_parses_from_the_config() {
    let command: InvokeCommand =
      serde_json::from_str("\"set-border-color #a6e3a1\"")
        .expect("config command should parse");

    assert_eq!(command, parse("set-border-color #a6e3a1"));
  }
}
