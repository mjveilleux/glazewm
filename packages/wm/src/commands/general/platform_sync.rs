use std::collections::HashSet;

use anyhow::Context;
#[cfg(target_os = "windows")]
use wm_common::WindowEffectConfig;
use wm_common::{
  AnimationEffect, CursorJumpTrigger, DisplayState, HideCorner,
  HideMethod, ResolvedEffect, UniqueExt, WindowState, WmEvent,
};
#[cfg(target_os = "windows")]
use wm_platform::CornerStyle;
#[cfg(target_os = "windows")]
use wm_platform::NativeWindowWindowsExt;
use wm_platform::{OpacityValue, Rect, WindowZOrder};

use crate::{
  animation_manager::{
    entrance_scale, AnimationPlan, AnimationTrigger, LiveRequest,
    SlideRequest,
  },
  models::{Container, WindowContainer},
  traits::{CommonGetters, PositionGetters, WindowGetters},
  user_config::UserConfig,
  wm_state::WmState,
};

pub fn platform_sync(
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  // Any sync takes along the changes a settle was holding back.
  state.settle_deadline = None;

  // Animations this sync starts were asked for now, or when a settle
  // began holding the sync back.
  state.animation_manager.mark_request();
  let result = sync(state, config);
  state.animation_manager.clear_request();

  result
}

/// Implements [`platform_sync`].
fn sync(state: &mut WmState, config: &UserConfig) -> anyhow::Result<()> {
  let focused_container =
    state.focused_container().context("No focused container.")?;

  if state.pending_sync.needs_focus_update() {
    sync_focus(&focused_container, state)?;
  }

  if !state.pending_sync.containers_to_redraw().is_empty()
    || !state.pending_sync.workspaces_to_reorder().is_empty()
  {
    redraw_containers(&focused_container, state, config)?;
  }

  if state.pending_sync.needs_cursor_jump()
    && config.value.general.cursor_jump.enabled
  {
    jump_cursor(focused_container.clone(), state, config)?;
  }

  if state.pending_sync.needs_focused_effect_update()
    || state.pending_sync.needs_all_effects_update()
  {
    // Keep reference to the previous window that had focus effects
    // applied.
    let prev_effects_window = state.prev_effects_window.clone();

    if let Ok(window) = focused_container.as_window_container() {
      let is_animation_hidden =
        state.animation_manager.is_hidden(&window.id());
      apply_window_effects(&window, true, is_animation_hidden, config);
      state.prev_effects_window = Some(window.clone());
    } else {
      state.prev_effects_window = None;
    }

    // Get windows that should have the unfocused border applied to them.
    // For the sake of performance, we only update the border of the
    // previously focused window. If the `reset_window_effects` flag is
    // passed, the unfocused border is applied to all unfocused windows.
    let unfocused_windows =
      if state.pending_sync.needs_all_effects_update() {
        state.windows()
      } else {
        prev_effects_window.into_iter().collect()
      }
      .into_iter()
      .filter(|window| window.id() != focused_container.id());

    for window in unfocused_windows {
      let is_animation_hidden =
        state.animation_manager.is_hidden(&window.id());
      apply_window_effects(&window, false, is_animation_hidden, config);
    }
  }

  state.pending_sync.clear();

  Ok(())
}

fn sync_focus(
  focused_container: &Container,
  state: &mut WmState,
) -> anyhow::Result<()> {
  let native_window = focused_container.as_window_container().ok();

  // Sets focus to the appropriate target:
  // - If the container is a window, focuses that window.
  // - If the container is a workspace, "resets" focus by focusing the
  //   desktop window.
  //
  // In either case, a `PlatformEvent::WindowFocused` event is subsequently
  // triggered.
  let result = if let Some(window) = native_window {
    tracing::info!("Setting focus to window: {window}");
    window.native().focus()
  } else {
    tracing::info!("Setting focus to the desktop window.");
    state.dispatcher.reset_focus()
  };

  if let Err(err) = result {
    tracing::warn!("Failed to set focus: {}", err);
  }

  state.emit_event(WmEvent::FocusChanged {
    focused_container: focused_container.to_dto()?,
  });

  Ok(())
}

/// Finds windows that should be brought to the top of their workspace's
/// z-order.
///
/// Windows are brought to front if they match the focused window's state
/// (floating/tiling) and any of these conditions are met:
///  * Focus has changed to a different window.
///  * Focused window's state has changed (e.g. tiling -> floating).
///  * Focused window has moved to a different workspace.
fn windows_to_bring_to_front(
  focused_container: &Container,
  state: &WmState,
) -> anyhow::Result<Vec<WindowContainer>> {
  let focused_workspace =
    focused_container.workspace().context("No workspace.")?;

  // Add focused workspace if there's been a focus change.
  let workspaces_to_reorder = state
    .pending_sync
    .workspaces_to_reorder()
    .iter()
    .chain(
      state
        .pending_sync
        .needs_focus_update()
        .then_some(&focused_workspace),
    )
    .unique_by(|workspace| workspace.id());

  // Bring forward windows that match the focused state. Only do this for
  // tiling/floating windows.
  let windows_to_bring_to_front = workspaces_to_reorder
    .flat_map(|workspace| {
      let focused_descendant = workspace
        .descendant_focus_order()
        .next()
        .and_then(|container| container.as_window_container().ok());

      match focused_descendant {
        Some(focused_descendant) => workspace
          .descendants()
          .filter_map(|descendant| descendant.as_window_container().ok())
          .filter(|window| {
            let is_floating_or_tiling = matches!(
              window.state(),
              WindowState::Floating(_) | WindowState::Tiling
            );

            is_floating_or_tiling
              && window.state().is_same_state(&focused_descendant.state())
          })
          .collect(),
        None => vec![],
      }
    })
    .collect::<Vec<_>>();

  Ok(windows_to_bring_to_front)
}

#[allow(clippy::too_many_lines)]
fn redraw_containers(
  focused_container: &Container,
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  let windows_to_redraw = state.windows_to_redraw();
  let windows_to_bring_to_front =
    windows_to_bring_to_front(focused_container, state)?;

  let windows_to_update = {
    let mut windows = windows_to_redraw
      .iter()
      .chain(&windows_to_bring_to_front)
      .unique_by(|window| window.id())
      .collect::<Vec<_>>();

    let descendant_focus_order = state
      .root_container
      .descendant_focus_order()
      .collect::<Vec<_>>();

    // Sort the windows to update by their focus order. The most recently
    // focused window will be updated first.
    // TODO: To reduce flicker, redraw windows that will be shown first,
    // then redraw the ones to be hidden last.
    windows.sort_by_key(|window| {
      descendant_focus_order
        .iter()
        .position(|order| order.id() == window.id())
    });

    windows
  };

  // Get monitors by their optimal hide corner.
  let monitors_by_hide_corner = state.monitors_by_hide_corner();

  // A workspace switch to animate: its monitor's bounds, and the effects
  // of the windows leaving and arriving.
  let switch = workspace_switch_animation(state, config);

  let mut redraws = Vec::new();

  for window in windows_to_update.iter().rev() {
    let should_bring_to_front = windows_to_bring_to_front.contains(window);

    let workspace =
      window.workspace().context("Window has no workspace.")?;

    let monitor = window.monitor().context("No monitor.")?;
    let hide_corner = monitors_by_hide_corner
      .iter()
      .find(|(m, _)| m.id() == monitor.id())
      .map(|(_, hide_corner)| hide_corner)
      .context("Monitor not found in hide corner map.")?;

    // Whether the window should be shown above all other windows.
    let z_order = match window.state() {
      WindowState::Floating(config) if config.shown_on_top => {
        WindowZOrder::TopMost
      }
      WindowState::Fullscreen(config) if config.shown_on_top => {
        WindowZOrder::TopMost
      }
      _ if should_bring_to_front => {
        let focused_descendant = workspace
          .descendant_focus_order()
          .next()
          .and_then(|container| container.as_window_container().ok());

        if let Some(focused_descendant) = focused_descendant {
          if window.id() == focused_descendant.id() {
            WindowZOrder::Normal
          } else {
            WindowZOrder::AfterWindow(focused_descendant.native().id())
          }
        } else {
          WindowZOrder::Normal
        }
      }
      _ => WindowZOrder::Normal,
    };

    // Set the z-order of the window.
    //
    // NOTE: macOS doesn't have a robust public API for setting the z-order
    // of a window. See `NativeWindow::raise` for more details.
    #[cfg(target_os = "windows")]
    if should_bring_to_front && !windows_to_redraw.contains(window) {
      tracing::info!("Updating window z-order: {window}");

      if let Err(err) = window.native().set_z_order(&z_order) {
        tracing::warn!("Failed to set window z-order: {}", err);
      }
    }

    // Skip updating the window's position if it only required a z-order
    // change.
    if !windows_to_redraw.contains(window) {
      continue;
    }

    // Transition display state depending on whether window will be
    // shown or hidden.
    window.set_display_state(
      match (window.display_state(), workspace.is_displayed()) {
        (DisplayState::Hidden | DisplayState::Hiding, true) => {
          DisplayState::Showing
        }
        (DisplayState::Shown | DisplayState::Showing, false) => {
          DisplayState::Hiding
        }
        _ => window.display_state(),
      },
    );

    let target_rect = window.to_rect()?;

    let is_visible = matches!(
      window.display_state(),
      DisplayState::Showing | DisplayState::Shown
    );

    // A window leaving or arriving with an animated workspace switch.
    let switch_role = switch
      .as_ref()
      .filter(|switch| switch.monitor_id == monitor.id())
      .and_then(|_| match window.display_state() {
        DisplayState::Hiding => Some(SwitchRole::Leaving),
        DisplayState::Showing => Some(SwitchRole::Arriving),
        _ => None,
      });

    // Hidden windows and syncs that skip animations (e.g. workspace
    // switches, or ending finished animations) stop any running
    // animation; the window then jumps straight to its target. A visible
    // window whose slide is over gets the slide's snapshot faded into it.
    // Windows in an animated workspace switch slide with it instead.
    let animation_plan = if switch_role.is_some() {
      None
    } else if !is_visible {
      state.animation_manager.cancel_animation(&window.id());
      None
    } else if state.pending_sync.should_skip_animations() {
      state
        .animation_manager
        .end_animation(&window.id(), &state.dispatcher);
      None
    } else {
      let animation_trigger =
        if state.pending_sync.open_animation_windows().contains(window) {
          AnimationTrigger::WindowOpened
        } else {
          AnimationTrigger::WindowMoved
        };

      state.animation_manager.animation_plan_for_window(
        window,
        &animation_trigger,
        &target_rect,
        &monitor.native_properties(),
        config,
      )
    };

    redraws.push(PendingRedraw {
      window: (*window).clone(),
      z_order,
      target_rect,
      is_visible,
      animation_plan,
      frame_rate: monitor.native_properties().refresh_rate.unwrap_or(60),
      hide_corner: *hide_corner,
      switch_role,
    });
  }

  // Start all slides together, before any window moves, so that each
  // snapshot shows the window where it was and their captures run in
  // parallel.
  // Windows leaving with a workspace switch are captured now too, while
  // still shown.
  let mut slide_requests = redraws
    .iter()
    .filter_map(|redraw| match &redraw.animation_plan {
      Some(AnimationPlan::Slide(effect)) => Some(SlideRequest::new(
        redraw.window.clone(),
        effect.clone(),
        redraw.target_rect.clone(),
        redraw.frame_rate,
      )),
      _ => None,
    })
    .collect::<Vec<_>>();

  if let Some(switch) = &switch {
    slide_requests.extend(
      redraws
        .iter()
        .filter(|redraw| redraw.switch_role == Some(SwitchRole::Leaving))
        .filter_map(|redraw| switch.leaving_request(redraw)),
    );
  }

  let mut started_slides = state
    .animation_manager
    .prepare_slides(slide_requests, &state.dispatcher);

  // Without a switch, slides start before any window moves. With one,
  // they wait for the windows arriving, captured once they're shown.
  if switch.is_none() {
    started_slides = state
      .animation_manager
      .start_prepared_slides(&state.dispatcher);
  }

  // Windows captured once placed, at an opacity of 0: those arriving with
  // a switch, and new windows growing in (captured at their own size, not
  // the one their app opened them at).
  let mut placed_first = switch.as_ref().map_or_else(Vec::new, |switch| {
    redraws
      .iter()
      .filter(|redraw| redraw.switch_role == Some(SwitchRole::Arriving))
      .filter_map(|redraw| switch.arriving_request(redraw))
      .collect::<Vec<_>>()
  });

  placed_first.extend(redraws.iter().filter_map(|redraw| {
    match &redraw.animation_plan {
      Some(AnimationPlan::SnapshotIn(effect)) => Some(SlideRequest {
        start_rect: Some(redraw.target_rect.clone()),
        // Fading in (at once, if fades are off).
        opacity: effect.fade.as_ref().map(|_| (0.0, 1.0)),
        fade: effect.fade.clone(),
        scale: Some((entrance_scale(effect), (1.0, 1.0))),
        ..SlideRequest::new(
          redraw.window.clone(),
          effect.clone(),
          redraw.target_rect.clone(),
          redraw.frame_rate,
        )
      }),
      _ => None,
    }
  }));

  let placed_first_ids = placed_first
    .iter()
    .map(|request| request.window.id())
    .collect::<HashSet<_>>();

  for redraw in redraws {
    let window = &redraw.window;

    let fade_effect = match &redraw.animation_plan {
      Some(AnimationPlan::FadeIn(effect)) => Some(effect),
      _ => None,
    };

    // A live animation starts where the window is now (or, for a
    // pop-in, smaller at its target), and the window is placed there.
    let live = match &redraw.animation_plan {
      Some(AnimationPlan::Live { effect, popin }) => live_frames(
        window,
        &redraw.target_rect,
        popin.then_some(effect.popin_scale),
        state,
      )
      .map(|(start, target)| (effect, *popin, start, target)),
      _ => None,
    };

    let is_popin = live.as_ref().is_some_and(|(_, popin, _, _)| *popin);

    // Windows that animate wait at their target with an opacity of 0:
    // behind their overlay for a slide, or until they fade in. Windows
    // whose animation has ended get their opacity back.
    let opacity_change = if fade_effect.is_some()
      || is_popin
      || started_slides.contains(&window.id())
      || placed_first_ids.contains(&window.id())
    {
      OpacityChange::Hide
    } else if state.animation_manager.take_hidden(&window.id()) {
      OpacityChange::Restore(resting_opacity(
        window,
        window.id() == focused_container.id(),
        config,
      ))
    } else {
      OpacityChange::Keep
    };

    tracing::debug!("Updating window position: {window}");

    // Marked before repositioning, so the window gets its opacity back
    // even if repositioning fails partway.
    if opacity_change == OpacityChange::Hide {
      state.animation_manager.mark_hidden(window.id());
    }

    if let Err(err) = reposition_window(
      window,
      &redraw.target_rect,
      live.as_ref().map(|(_, _, start, _)| start),
      redraw.hide_corner,
      &redraw.z_order,
      &opacity_change,
      redraw.is_visible,
      config,
    ) {
      tracing::warn!("Failed to set window position: {}", err);
    }

    if let Some((effect, popin, start_frame, target_frame)) = live {
      state.animation_manager.start_live(LiveRequest {
        window: window.clone(),
        effect: effect.clone(),
        start_frame,
        target_frame,
        target_rect: redraw.target_rect.clone(),
        fade_to: popin.then(|| {
          resting_opacity(
            window,
            window.id() == focused_container.id(),
            config,
          )
        }),
        frame_rate: redraw.frame_rate,
      });
    }

    // Start the fade even if repositioning failed, since it's what makes
    // the window visible again.
    if let Some(effect) = fade_effect {
      state.animation_manager.start_fade(
        window,
        effect,
        resting_opacity(
          window,
          window.id() == focused_container.id(),
          config,
        ),
        redraw.frame_rate,
      );
    }

    // Whether the window is either transitioning to or from fullscreen.
    // TODO: This check can be improved since `prev_state` can be
    // fullscreen without it needing to be marked as not fullscreen.
    #[cfg(target_os = "windows")]
    {
      let is_transitioning_fullscreen =
        match (window.prev_state(), window.state()) {
          (Some(_), WindowState::Fullscreen(s)) if !s.maximized => true,
          (Some(WindowState::Fullscreen(_)), _) => true,
          _ => false,
        };

      if is_transitioning_fullscreen {
        if let Err(err) = window.native().mark_fullscreen(matches!(
          window.state(),
          WindowState::Fullscreen(_)
        )) {
          tracing::warn!("Failed to mark window as fullscreen: {}", err);
        }
      }
    }

    // Skip setting taskbar visibility if the window is hidden (has no
    // effect). Since cloaked windows are normally always visible in the
    // taskbar, we only need to set visibility if `show_all_in_taskbar` is
    // `false`.
    #[cfg(target_os = "windows")]
    if config.value.general.hide_method == HideMethod::Cloak
      && !config.value.general.show_all_in_taskbar
      && matches!(
        window.display_state(),
        DisplayState::Showing | DisplayState::Hiding
      )
    {
      if let Err(err) =
        window.native().set_taskbar_visibility(redraw.is_visible)
      {
        tracing::warn!("Failed to set taskbar visibility: {}", err);
      }
    }
  }

  // With a switch, the windows leaving start with those arriving.
  if switch.is_some() || !placed_first.is_empty() {
    start_placed_first(placed_first, focused_container, state, config);
  }

  Ok(())
}

/// Captures the windows to animate once placed (shown by now, at an
/// opacity of 0: windows arriving with a workspace switch, new windows
/// growing in), and starts every prepared slide on the same frame, those
/// of windows leaving with a switch included.
///
/// A window that can't animate gets its opacity back at once, so it's
/// never left invisible.
///
/// # Platform-specific
///
/// - **macOS**: Never called; workspaces switch and windows appear without
///   these animations.
// LINT: `focused_container` and `config` are only used on Windows.
#[cfg_attr(not(target_os = "windows"), allow(unused_variables))]
fn start_placed_first(
  requests: Vec<SlideRequest>,
  focused_container: &Container,
  state: &mut WmState,
  config: &UserConfig,
) {
  #[cfg(target_os = "windows")]
  let windows = requests
    .iter()
    .map(|request| request.window.clone())
    .collect::<Vec<_>>();

  state
    .animation_manager
    .prepare_slides(requests, &state.dispatcher);

  // LINT: Only read on Windows.
  #[cfg_attr(not(target_os = "windows"), allow(unused_variables))]
  let started = state
    .animation_manager
    .start_prepared_slides(&state.dispatcher);

  #[cfg(target_os = "windows")]
  for window in windows {
    if !started.contains(&window.id())
      && state.animation_manager.take_hidden(&window.id())
    {
      let opacity = resting_opacity(
        &window,
        window.id() == focused_container.id(),
        config,
      );

      if let Err(err) = window.native().set_transparency(&opacity) {
        tracing::warn!("Failed to restore window opacity: {err}");
      }
    }
  }
}

/// A window's part in an animated workspace switch.
#[derive(Clone, Copy, Debug, PartialEq)]
enum SwitchRole {
  Leaving,
  Arriving,
}

/// How a workspace switch animates, worked out once per sync.
struct SwitchAnimation {
  monitor_id: uuid::Uuid,

  /// Bounds of the monitor, which the slides are drawn within.
  monitor_rect: Rect,

  /// `1` towards a later workspace, `-1` towards an earlier one.
  direction: i32,

  leaving: Option<ResolvedEffect>,
  arriving: Option<ResolvedEffect>,
}

impl SwitchAnimation {
  /// How far, and which way, a window arriving starts from its place:
  /// the way the workspaces slide, `slide_fraction` of the monitor.
  /// Leaving windows go the opposite way.
  fn offset(&self, effect: &ResolvedEffect) -> (i32, i32) {
    // LINT: Monitor sizes are far below the `f32` precision limit.
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    let distance = |length: i32| {
      (length as f32 * effect.slide_fraction).round() as i32
        * self.direction
    };

    if effect.style == AnimationEffect::SlideVert {
      (0, distance(self.monitor_rect.height()))
    } else {
      (distance(self.monitor_rect.width()), 0)
    }
  }

  /// Opacity of a window's snapshot, from and to, if the style fades.
  fn opacity(
    effect: &ResolvedEffect,
    arriving: bool,
  ) -> Option<(f32, f32)> {
    matches!(
      effect.style,
      AnimationEffect::Fade | AnimationEffect::SlideFade
    )
    .then_some(if arriving { (0.0, 1.0) } else { (1.0, 0.0) })
  }

  /// The slide of a window leaving: from where it is, out the way the
  /// workspaces go.
  fn leaving_request(
    &self,
    redraw: &PendingRedraw,
  ) -> Option<SlideRequest> {
    let effect = self.leaving.as_ref()?;
    let (dx, dy) = self.offset(effect);

    Some(SlideRequest {
      bounds: Some(self.monitor_rect.clone()),
      opacity: Self::opacity(effect, false),
      ..SlideRequest::new(
        redraw.window.clone(),
        effect.clone(),
        offset_rect(&redraw.target_rect, -dx, -dy),
        redraw.frame_rate,
      )
    })
  }

  /// The slide of a window arriving: in from the way the workspaces come,
  /// to its place.
  fn arriving_request(
    &self,
    redraw: &PendingRedraw,
  ) -> Option<SlideRequest> {
    let effect = self.arriving.as_ref()?;
    let (dx, dy) = self.offset(effect);

    Some(SlideRequest {
      start_rect: Some(offset_rect(&redraw.target_rect, dx, dy)),
      bounds: Some(self.monitor_rect.clone()),
      opacity: Self::opacity(effect, true),
      ..SlideRequest::new(
        redraw.window.clone(),
        effect.clone(),
        redraw.target_rect.clone(),
        redraw.frame_rate,
      )
    })
  }
}

/// Moves a rect by `dx` and `dy`.
fn offset_rect(rect: &Rect, dx: i32, dy: i32) -> Rect {
  rect.translate_to_coordinates(rect.x() + dx, rect.y() + dy)
}

/// Works out how this sync's workspace switch animates, if it's one and
/// workspace animations are on.
///
/// # Platform-specific
///
/// - **macOS**: Always `None`; workspaces switch without animation.
fn workspace_switch_animation(
  state: &WmState,
  config: &UserConfig,
) -> Option<SwitchAnimation> {
  if !cfg!(target_os = "windows")
    || !wm_platform::system_animations_enabled()
  {
    return None;
  }

  let switch = state.pending_sync.workspace_switch()?;
  let resolved = config.value.animations.resolved();

  if switch.direction == 0
    || (resolved.workspaces_in.is_none()
      && resolved.workspaces_out.is_none())
  {
    return None;
  }

  let monitor = state
    .monitors()
    .into_iter()
    .find(|monitor| monitor.id() == switch.monitor_id)?;

  Some(SwitchAnimation {
    monitor_id: switch.monitor_id,
    monitor_rect: monitor.to_rect().ok()?,
    direction: switch.direction,
    leaving: resolved.workspaces_out.clone(),
    arriving: resolved.workspaces_in.clone(),
  })
}

/// A window to reposition, with what was decided for it before any
/// window moves.
struct PendingRedraw {
  window: WindowContainer,
  z_order: WindowZOrder,
  target_rect: Rect,
  is_visible: bool,
  animation_plan: Option<AnimationPlan>,
  frame_rate: u32,
  hide_corner: HideCorner,

  /// The window's part in an animated workspace switch, if any.
  switch_role: Option<SwitchRole>,
}

/// What to do with a window's opacity when repositioning it.
#[derive(Debug, PartialEq)]
enum OpacityChange {
  /// Leave the opacity as is.
  Keep,

  /// Set the opacity to 0 before moving the window, while an animation
  /// shows it instead.
  Hide,

  /// Set the opacity back after an animation has ended.
  Restore(OpacityValue),
}

/// Gets the opacity a window should have when no animation is running:
/// its transparency effect if one applies, otherwise fully opaque.
fn resting_opacity(
  window: &WindowContainer,
  is_focused: bool,
  config: &UserConfig,
) -> OpacityValue {
  let window_effects = &config.value.window_effects;

  let effect_config = if is_focused {
    &window_effects.focused_window
  } else {
    &window_effects.other_windows
  };

  if effect_config.transparency.enabled
    && should_apply_window_effects(&window.state())
  {
    effect_config.transparency.opacity
  } else {
    OpacityValue::from_alpha(u8::MAX)
  }
}

/// Gets the native frames a live animation of the window goes between:
/// where it is now (smaller at its target, for a pop-in), and its target.
fn live_frames(
  window: &WindowContainer,
  target_rect: &Rect,
  popin_scale: Option<f32>,
  state: &WmState,
) -> Option<(Rect, Rect)> {
  let target_frame =
    target_rect.apply_delta(&window.total_border_delta().ok()?, None);

  let start_frame = if let Some(frame) =
    state.animation_manager.live_frame(&window.id())
  {
    frame
  } else if let Some(popin_scale) = popin_scale {
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    let scale = |length: i32| (length as f32 * popin_scale) as i32;

    let width = scale(target_frame.width());
    let height = scale(target_frame.height());

    Rect::from_xy(
      target_frame.x() + (target_frame.width() - width) / 2,
      target_frame.y() + (target_frame.height() - height) / 2,
      width,
      height,
    )
  } else {
    window.native_properties().frame
  };

  Some((start_frame, target_frame))
}

/// Repositions the window at `rect`, or at `native_frame` when given
/// (a frame as the OS has it, borders included, such as the first frame of
/// a live animation).
#[allow(clippy::too_many_arguments)]
fn reposition_window(
  window: &WindowContainer,
  rect: &Rect,
  native_frame: Option<&Rect>,
  hide_corner: HideCorner,
  // LINT: `z_order` is only used on Windows.
  #[cfg_attr(not(target_os = "windows"), allow(unused_variables))]
  z_order: &WindowZOrder,
  opacity_change: &OpacityChange,
  is_visible: bool,
  config: &UserConfig,
) -> anyhow::Result<()> {
  let rect = match native_frame {
    Some(frame) => frame.clone(),
    None => rect.apply_delta(&window.total_border_delta()?, None),
  };

  // For animations on macOS and `HideMethod::PlaceInCorner`, we need to
  // hide windows by repositioning them in the corner of the monitor.
  if (cfg!(target_os = "macos") && *opacity_change == OpacityChange::Hide)
    || (config.value.general.hide_method == HideMethod::PlaceInCorner
      && !is_visible)
  {
    const VISIBLE_SLIVER: i32 = 1;

    let monitor_rect = window
      .monitor()
      .context("No monitor.")?
      .native_properties()
      .working_area;

    let frame = window.native_properties().frame;

    let position_y = monitor_rect.bottom - VISIBLE_SLIVER;
    let position_x = match hide_corner {
      HideCorner::BottomLeft => {
        monitor_rect.left + VISIBLE_SLIVER - frame.width()
      }
      HideCorner::BottomRight => monitor_rect.right - VISIBLE_SLIVER,
    };

    // Even though the window size is unchanged, `NativeWindow::set_frame`
    // is used instead of `NativeWindow::reposition` because the latter
    // resulted in occasional incorrect positionings on macOS.
    window.native().set_frame(&Rect::from_xy(
      position_x,
      position_y,
      frame.width(),
      frame.height(),
    ))?;

    return Ok(());
  }

  if window.active_drag().is_some() {
    window.native().resize(rect.width(), rect.height())?;
  } else {
    #[cfg(target_os = "macos")]
    window.native().set_frame(&rect)?;

    #[cfg(target_os = "windows")]
    {
      use wm_platform::{
        SWP_ASYNCWINDOWPOS, SWP_FRAMECHANGED, SWP_NOACTIVATE,
        SWP_NOCOPYBITS, SWP_NOSENDCHANGING, WS_MAXIMIZEBOX,
      };

      // Hide before moving, so the move itself is never seen.
      if *opacity_change == OpacityChange::Hide {
        window
          .native()
          .set_transparency(&OpacityValue::from_alpha(0))?;
      }

      // Restore window if it's minimized/maximized and shouldn't be. This
      // is needed to be able to move and resize it.
      let should_restore = match &window.state() {
        // Need to restore window if transitioning from maximized
        // fullscreen to non-maximized fullscreen.
        WindowState::Fullscreen(fullscreen) => {
          !fullscreen.maximized && window.native().is_maximized()?
        }
        // No need to restore window if it'll be minimized. Transitioning
        // from maximized to minimized works without having to
        // restore.
        WindowState::Minimized => false,
        _ => {
          window.native().is_minimized()?
            || window.native().is_maximized()?
        }
      };

      if should_restore {
        // Restoring to position has the same effect as `ShowWindow` with
        // `SW_RESTORE`, but doesn't cause a flicker.
        window.native().restore(Some(&rect))?;
      }

      let mut swp_flags = SWP_NOACTIVATE
        | SWP_NOCOPYBITS
        | SWP_NOSENDCHANGING
        | SWP_ASYNCWINDOWPOS;

      match &window.state() {
        WindowState::Minimized => {
          if !window.native().is_minimized()? {
            window.native().minimize()?;
          }
        }
        WindowState::Fullscreen(fullscreen)
          if fullscreen.maximized
            && window.native().has_window_style(WS_MAXIMIZEBOX) =>
        {
          if !window.native().is_maximized()? {
            window.native().maximize()?;
          }

          window.native().set_window_pos(z_order, &rect, swp_flags)?;
        }
        _ => {
          swp_flags |= SWP_FRAMECHANGED;

          window.native().set_window_pos(z_order, &rect, swp_flags)?;

          // When there's a mismatch between the DPI of the monitor and the
          // window, the window might be sized incorrectly after the first
          // move. If we set the position twice, inconsistencies after the
          // first move are resolved.
          if window.has_pending_dpi_adjustment() {
            window.native().set_window_pos(z_order, &rect, swp_flags)?;
          }
        }
      }

      if let OpacityChange::Restore(opacity) = opacity_change {
        window.native().set_transparency(opacity)?;
      }

      // Set visibility based on the hide method.
      if config.value.general.hide_method == HideMethod::Cloak {
        window.native().set_cloaked(!is_visible)?;
      } else if is_visible {
        window.native().show()?;
      } else {
        window.native().hide()?;
      }
    }
  }

  Ok(())
}

fn jump_cursor(
  focused_container: Container,
  state: &WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  let cursor_jump = &config.value.general.cursor_jump;

  let jump_target = match cursor_jump.trigger {
    CursorJumpTrigger::WindowFocus => Some(focused_container),
    CursorJumpTrigger::MonitorFocus => {
      let target_monitor =
        focused_container.monitor().context("No monitor.")?;

      let cursor_monitor = state
        .dispatcher
        .cursor_position()
        .ok()
        .and_then(|pos| state.monitor_at_point(&pos));

      // Jump to the target monitor if the cursor is not already on it.
      cursor_monitor
        .filter(|monitor| monitor.id() != target_monitor.id())
        .map(|_| target_monitor.into())
    }
  };

  if let Some(jump_target) = jump_target {
    let center = jump_target.to_rect()?.center_point();

    if let Err(err) = state.dispatcher.set_cursor_position(&center) {
      tracing::warn!("Failed to set cursor position: {}", err);
    }
  }

  Ok(())
}

fn apply_window_effects(
  // LINT: `window` is only used on Windows.
  #[cfg_attr(not(target_os = "windows"), allow(unused_variables))]
  window: &WindowContainer,
  is_focused: bool,
  // LINT: `is_animation_hidden` is only used on Windows.
  #[cfg_attr(not(target_os = "windows"), allow(unused_variables))]
  is_animation_hidden: bool,
  config: &UserConfig,
) {
  let window_effects = &config.value.window_effects;

  // An animation owns the opacity of windows it has hidden, and restores
  // it (transparency effect included) once it ends.
  #[cfg(target_os = "windows")]
  let transparency_enabled = !is_animation_hidden
    && (window_effects.focused_window.transparency.enabled
      || window_effects.other_windows.transparency.enabled);

  // LINT: `effect_config` is only used on Windows.
  #[cfg_attr(not(target_os = "windows"), allow(unused_variables))]
  let effect_config = if is_focused {
    &window_effects.focused_window
  } else {
    &window_effects.other_windows
  };

  // Fullscreen windows cover the entire monitor, so DWM decorations
  // (border color, rounded corners, transparency) would be drawn on top
  // of them. Windows suppresses these for maximized windows, but not for
  // windows that an app made fullscreen itself (e.g. F11 in a browser,
  // fullscreen video). Any attributes previously set by GlazeWM would
  // therefore remain visible and make the window not look fullscreen, so
  // reset them here. `hide_title_bar` is still applied so title bars
  // stay hidden.
  #[cfg(target_os = "windows")]
  if !should_apply_window_effects(&window.state()) {
    if window_effects.focused_window.border.enabled
      || window_effects.other_windows.border.enabled
    {
      _ = window.native().set_border_color(None);
    }

    if window_effects.focused_window.corner_style.enabled
      || window_effects.other_windows.corner_style.enabled
    {
      _ = window.native().set_corner_style(&CornerStyle::Default);
    }

    if transparency_enabled {
      _ = window
        .native()
        .set_transparency(&OpacityValue::from_alpha(u8::MAX));
    }

    if window_effects.focused_window.hide_title_bar.enabled
      || window_effects.other_windows.hide_title_bar.enabled
    {
      apply_hide_title_bar_effect(window, effect_config);
    }

    return;
  }

  // Skip if both focused + non-focused window effects are disabled.
  #[cfg(target_os = "windows")]
  if window_effects.focused_window.border.enabled
    || window_effects.other_windows.border.enabled
  {
    apply_border_effect(window, effect_config);
  }

  #[cfg(target_os = "windows")]
  if window_effects.focused_window.hide_title_bar.enabled
    || window_effects.other_windows.hide_title_bar.enabled
  {
    apply_hide_title_bar_effect(window, effect_config);
  }

  #[cfg(target_os = "windows")]
  if window_effects.focused_window.corner_style.enabled
    || window_effects.other_windows.corner_style.enabled
  {
    apply_corner_effect(window, effect_config);
  }

  #[cfg(target_os = "windows")]
  if transparency_enabled {
    apply_transparency_effect(window, effect_config);
  }
}

/// Whether DWM window effects (border color, corner style, transparency)
/// should be applied to a window in the given state.
///
/// Effects are skipped for fullscreen windows, since they cover the entire
/// monitor and should not show any border or rounded corners.
fn should_apply_window_effects(state: &WindowState) -> bool {
  !matches!(state, WindowState::Fullscreen(_))
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
  use wm_common::{
    FloatingStateConfig, FullscreenStateConfig, WindowState,
  };

  use super::should_apply_window_effects;

  #[test]
  fn skips_effects_for_fullscreen_windows() {
    let fullscreen = WindowState::Fullscreen(FullscreenStateConfig {
      maximized: false,
      shown_on_top: false,
    });
    assert!(!should_apply_window_effects(&fullscreen));

    let maximized_fullscreen =
      WindowState::Fullscreen(FullscreenStateConfig {
        maximized: true,
        shown_on_top: false,
      });
    assert!(!should_apply_window_effects(&maximized_fullscreen));
  }

  #[test]
  fn applies_effects_for_non_fullscreen_windows() {
    assert!(should_apply_window_effects(&WindowState::Tiling));

    let floating = WindowState::Floating(FloatingStateConfig {
      centered: true,
      shown_on_top: false,
    });
    assert!(should_apply_window_effects(&floating));

    assert!(should_apply_window_effects(&WindowState::Minimized));
  }
}

#[cfg(target_os = "windows")]
fn apply_border_effect(
  window: &WindowContainer,
  effect_config: &WindowEffectConfig,
) {
  let border_color = if effect_config.border.enabled {
    Some(&effect_config.border.color)
  } else {
    None
  };

  _ = window.native().set_border_color(border_color);

  let native = window.native().clone();
  let border_color = border_color.cloned();

  // Re-apply border color after a short delay to better handle
  // windows that change it themselves.
  tokio::task::spawn(async move {
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    _ = native.set_border_color(border_color.as_ref());
  });
}

#[cfg(target_os = "windows")]
fn apply_hide_title_bar_effect(
  window: &WindowContainer,
  effect_config: &WindowEffectConfig,
) {
  _ = window
    .native()
    .set_title_bar_visibility(!effect_config.hide_title_bar.enabled);
}

#[cfg(target_os = "windows")]
fn apply_corner_effect(
  window: &WindowContainer,
  effect_config: &WindowEffectConfig,
) {
  let corner_style = if effect_config.corner_style.enabled {
    &effect_config.corner_style.style
  } else {
    &CornerStyle::Default
  };

  _ = window.native().set_corner_style(corner_style);
}

#[cfg(target_os = "windows")]
fn apply_transparency_effect(
  window: &WindowContainer,
  effect_config: &WindowEffectConfig,
) {
  let transparency = if effect_config.transparency.enabled {
    &effect_config.transparency.opacity
  } else {
    // Reset the transparency to default.
    &OpacityValue::from_alpha(u8::MAX)
  };

  _ = window.native().set_transparency(transparency);
}
