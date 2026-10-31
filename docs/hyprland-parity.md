# Plan: make GlazeWM (omawin fork) feel like Hyprland

Branch: `claude/hyprland-parity`, off `claude/omawin-roadmap-devex-j1tmak` (which holds `omawin`, upstream `main` and upstream `animations-test`). Follow `CLAUDE.md`: no `.unwrap()`, `crate::Result` in `wm-platform`, `anyhow` elsewhere, `tracing` for logs, every fn documented, `SAFETY:` on every `unsafe`.

## 1. Where things stand

**Hyprland** owns the compositor: a moving window is its last buffer drawn at an animated position and size; the client gets its new size once. Animations are `CAnimatedVariable`s driven by named cubic-bezier curves, ticked on the monitor's frame clock, configured as a tree (`windows` → `windowsIn` / `windowsOut` / `windowsMove`, `fade*`, `border`, `workspaces` → `workspacesIn` / `workspacesOut`, …) whose children inherit from their parents.

**This fork** (`packages/wm/src/animation_manager.rs`, `packages/wm-platform/src/platform_impl/windows/animation_window.rs`):

- A click-through overlay per animation with DirectComposition visuals, holding a one-shot Windows.Graphics.Capture snapshot.
- Effects: `slide` (snapshot), `fade` (real window's opacity), `move` and `popin` (real window moved with `SetWindowPos` every frame), and a close animation for the `close` command.
- Easing is a fixed enum (`easing_function.rs`); no custom curves.
- Ticks come from a `tokio::time::interval` at the monitor's refresh rate: not vsync-aligned, and every frame goes through the WM loop and a `Commit()`. The main source of the remaining jitter.
- No workspace transitions, animated border color or dim-inactive.
- CI (`animation-check.yaml`) records a real Windows desktop and publishes contact sheets, videos and `checks.txt` to `ci/animation-check`.

Correction carried forward: Hyprland animates the texture, not the live client. Snapshot `slide` is the default; live `move` / `popin` stay opt-in.

## 2. Goals and non-goals

Goals: vsync-smooth window moves, opens and closes; workspace transitions; Hyprland-style curves and animation tree; animated focus borders; robust interruption.

Non-goals: blur or shaders on other apps' content; injecting into `dwm.exe`; undocumented `dwmapi` ordinals; macOS parity (macOS keeps compiling; differences documented under `# Platform-specific`).

## 3. Phases

Every phase passes `cargo fmt --check`, `cargo clippy --all-targets`, `cargo test` and the `animation-check` workflow before it's called done.

### Phase 0: measurement

1. Telemetry on `tracing` target `glazewm::anim`: command to first frame, per-frame interval (mean, p95, max), dropped frames, snapshot capture latency per window.
2. `scripts/animation-check.ps1` writes a timing table to `checks.txt`.
3. CI gates only what's deterministic: 0 leftover overlays, every window opaque, final rects exact. Timings are reported, not gated: GitHub's Windows runners have no GPU or real vsync, so frame-time thresholds would fail at random. A start-latency threshold, if any, comes from the measured baseline, not a guess (a first WGC frame alone is usually 1–3 refreshes).
4. After Phase 2 there are no WM ticks to count for snapshot animations, so dropped frames come from DWM (`DwmGetCompositionTimingInfo`).

### Phase 1: curves and the animation tree

1. `Easing { Preset(..), Bezier(CubicBezier) }`. `CubicBezier::y_at(x)`: Newton's method with a bisection fallback, baked into a lookup table per curve. y may leave [0, 1] (overshoot); x control points outside [0, 1] are a config error.
2. Config:
   ```yaml
   animations:
     enabled: true
     beziers:
       snappy: [0.05, 0.9, 0.1, 1.05]
     windows:       { duration_ms: 200, easing: snappy }
     windows_in:    { style: popin, popin_percent: 80 }
     windows_out:   { style: popin, popin_percent: 80 }
     windows_move:  { style: slide, trigger_threshold: 10px }
     fade:          { duration_ms: 150, easing: ease_out_cubic }
     fade_in:       {}
     fade_out:      {}
     border:        { duration_ms: 300 }
     workspaces:    { style: slide, duration_ms: 250 }
     workspaces_in: {}
     workspaces_out: {}
   ```
   Nodes: `windows` → `windows_in` / `windows_out` / `windows_move`; `fade` → `fade_in` / `fade_out`; `border`; `workspaces` → `workspaces_in` / `workspaces_out`. Every node also takes `enabled`. Children inherit unset fields from their parent; resolved once at load into a flat `ResolvedAnimations`. An unknown curve name is a config error naming the key. A popin or slide in/out uses its own node's curve for scale and position, and the `fade_in` / `fade_out` node for opacity.
3. Old keys (`window_move` / `window_open` / `window_close` with `effect_type`) keep loading, mapped into the tree, with one deprecation warning.
4. Intermediate rects are clamped to ≥ 1 px when an overshoot curve would invert a size.
5. Animations are off while Windows' own "Animation effects" setting is off (`SPI_GETCLIENTAREAANIMATION`).

### Phase 2: compositor-driven timing

1. Snapshot animations become `IDCompositionAnimation`s built by the overlay: each bezier approximated with piecewise cubics (`AddCubic`, 8–16 segments, max error < 0.5 px over the move), bound to offset X/Y, the scale transform and opacity. One `Commit()` per start or retarget.
2. The animation's real start is the compositor frame its commit lands on: take it from `IDCompositionDevice::GetFrameStatistics` (`nextEstimatedFrameTime`). Retargeting evaluates the same curve (shared Rust math) at that clock.
3. Every animation ends on a constant segment holding its final frame, so the overlay is removed only after the real window is in place, never a frame early.
4. Opacity on a visual needs `IDCompositionVisual3`, i.e. a device from `DCompositionCreateDevice3`; the overlay moves to it.
5. The WM keeps one completion timer per animation; the tokio interval goes.
6. Live `move` / `popin` still tick per frame, on a pacing thread (`DCompositionWaitForCompositorClock`, falling back to `DwmFlush`) that sends ticks to the main loop; moves are batched with `BeginDeferWindowPos` / `EndDeferWindowPos`. The tick rate stays the monitor's refresh rate (there's no frame-rate setting).

### Phase 3: workspace transitions

1. Spike first, results in the PR before building: can WGC or `DwmRegisterThumbnail` show cloaked windows? Can the outgoing windows be captured just before they're hidden, and the incoming ones live while uncloaked at alpha 0? Does layered alpha work on UWP, flip-model DirectX and Chromium windows? A per-window texture cache (LRU, ~256 MB) is built only if those fail. Stop and ask if the result changes the design.
2. Styles `slide` (direction from workspace order and monitor), `slidevert`, `fade`, `slidefade N%`, as two DComp container visuals.
3. Outgoing real windows hide at once; incoming ones wait at their final rects at alpha 0 and are revealed in one batch.
4. Rapid switches retarget the group visuals instead of queueing.

### Phase 4: window in / out / move polish

1. Snapshot `popin N%` and `gnomed` for open and close; snapshot popin the default, live popin opt-in.
2. The 100 ms open settle ends early on the first non-blank capture frame.
3. Close beyond the `close` command where a snapshot exists (Phase 3 cache or the last animation); otherwise no animation.
4. At a slide's end, crossfade the snapshot into the live window (~60–80 ms) to hide the reflow pop (see `feat/animations-test-cross-fade`).
5. Fullscreen, maximized, elevated and uncapturable windows move instantly (logged at `debug`). A window is never left hidden.

### Phase 5: decorations

1. Animated focus border color: tween `DWMWA_BORDER_COLOR` with the `border` curve, on Phase 2's pacing thread, using the existing `window_effects` border colors.
2. `dim_inactive`, opt-in: one overlay window can't sit between other windows in z-order, so it needs a helper window per real window. Spike it; drop it (with the reason here) if it flickers on focus change or stacks wrong with always-on-top windows.
3. Gradient / rotating borders: out of scope.

### Phase 6: robustness

- Interruption tests (unit, with mocks, and CI): move during open, close during move, workspace switch during slide, config reload mid-animation, monitor unplug.
- Multi-monitor and mixed DPI: overlay offsets correct per monitor.
- Exit, crash and watcher: every hidden or transparent window restored; `wm-watcher` restores layered alpha on abnormal exit.
- `DXGI_ERROR_DEVICE_REMOVED` recreates the device or falls back to no animation, never a crash.

### Phase 7: docs

README animation section and sample config with a Hyprland-like preset. Platform-agnostic logic in `wm`, Windows code in `wm-platform`, so phases 1, 2 and 4 can be offered upstream.

## 4. Order

| Phase | Size | Depends on |
|---|---|---|
| 0 Measurement | S | none |
| 1 Curves and tree | M | 0 |
| 2 Compositor timing | L | 1 |
| 3 Workspaces | L | 2 + spike |
| 4 Polish | M | 2, 3 |
| 5 Decorations | M | 1, 2 |
| 6 Robustness | M | ongoing |
| 7 Docs | S | all |

## 5. Rules

- Spikes (3.1, 5.2) before the work that depends on them; findings recorded here.
- One concern per commit. Nothing merges with `animation-check` red. No PR or merge without the owner's say.
- `vendor/` untouched unless required.

## 6. Progress

### Timing, before and after

From the `animation-check` workflow's slowed run (600 ms animations) on GitHub's Windows Server 2025 runner, which has no GPU: its compositor manages 25–30 frames a second at best, so no code reaches a 16.7 ms frame there. "Before" is the code this branch started from, with Phase 0's measurements added (`claude/hyprland-parity-baseline`); both are recorded the same way (ffmpeg only).

| | Before | After (all phases) |
|---|---|---|
| compositor frame interval during animations, mean | 32–41 ms | 33–49 ms |
| compositor frame interval, p95 | 48–62 ms | 47–65 ms |
| WM ticks during a slide or close | 37–51, at 64 Hz, one 285 ms hitch | 0 when nothing else changes: the compositor plays them; else one per compositor frame (border colors, fades) |
| WM ticks during fades and live moves | 64 Hz timer | one per compositor frame |
| window capture | 76–231 ms | 65–208 ms |
| start, asked for to first frame | 159–430 ms | 2–673 ms (median 260; the long ones are workspace switches capturing four windows on a CPU-only runner) |

The after column is the last run (`animation-check` run 41175835713), which also runs every interruption step and passes every check.

What the runner can't show: on a machine with a GPU, the compositor runs at the display's rate and plays snapshot animations with no work from the WM, so their frames are as even as the display's. The p95 target (≈ one refresh period) can only be checked there; CI checks what's the same on any machine.

### Phases

| Phase | Status | Notes |
|---|---|---|
| 0 | done | `glazewm::anim` logs start and capture latency, WM tick intervals, and (a probe thread on `DwmFlush`) the compositor's own frame intervals. `checks.txt` has the table; the check fails only on leftover overlays, transparent windows, or windows not exactly in place. The runner's DWM frame counter barely moves, so the probe is what's reported. |
| 1 | done | Beziers (baked, overshoot allowed, x outside [0, 1] refused), the node tree with inheritance and errors naming the key, fade nodes for opacity, deprecated keys mapped in with one warning, ≥ 1 px clamp, Windows' Animation effects setting followed. |
| 2 | done (accepted by the owner; p95 checked on a PC with a GPU, section 8) | Slides and closes are DirectComposition animations fitted to the curve (8–64 cubic segments, < 0.5 px), started on the frame the compositor reports, ending on a held last frame; the WM wakes once per animation. Fades and live moves tick on `DwmFlush`. `Visual3::SetOpacity` works on the desktop device, so no `DCompositionCreateDevice3`. Live moves stay one async `SetWindowPos` per window rather than `DeferWindowPos`, which is synchronous: one hung app would stall the WM. |
| 3 | done | Workspace transitions (`slide`, `slidevert`, `fade`, `slidefade`), captured live per the spike; rapid switches turn running slides around. CI switches 1→2→1→3→1 faster than a switch takes: direction right, no overlay left, every window opaque and in place. |
| 4 | done | New windows glide into place from wherever their app opened them (snapshot, or the live window with `move`): hiding them until placed loses a race with the app's first frame (section 7, the new-window flash), so they aren't hidden. `popin` and `gnomed` close windows; a window closed by its app mid-slide plays the close animation from its snapshot; finished slides crossfade (70 ms) into the window; maximized, fullscreen and uncapturable windows move at once. |
| 5 | done; dim dropped | The focus border's color animates between the `window_effects` colors on the pacer thread. `dim_inactive` dropped after the spike (section 7). Gradient borders out of scope. |
| 6 | done, but for monitors | CI interrupts animations: a window moved while opening, killed mid-slide, the config reloaded mid-slide, workspaces switched mid-switch. Unit tests cover curves, config, clocks and switch geometry. The watcher restores windows hidden on show if the WM dies; a lost GPU device is replaced. Multi-monitor and mixed DPI aren't covered in CI: the runner has one display, and adding virtual ones needs a driver installed. |
| 7 | done | README section and sample config; platform-agnostic logic in `wm` and the curve math in `wm-platform`'s models, Windows code behind `platform_impl`. |

## 7. Spike findings

### Phase 3.1: capturing hidden and transparent windows

Run on GitHub's Windows Server 2025 runner (`scripts/capture-spike.ps1`, results on `ci/capture-spike--claude-hyprland-parity`), with Notepad, Edge (Chromium, `Chrome_WidgetWin_1`) and two Windows Terminal windows (flip-model swap chain, `CASCADIA_HOSTING_WINDOW_CLASS`):

| State | Windows.Graphics.Capture |
|---|---|
| visible | a frame in 95–180 ms (no GPU), 96–100% of pixels drawn |
| cloaked (another workspace shown) | refused for every window: `CreateForWindow` fails with `E_INVALIDARG` |
| layered alpha 0 (uncloaked) | a full frame in 94–103 ms, 96–100% drawn; the screenshot shows all four windows invisible |

So no texture cache: outgoing windows are captured just before they're hidden, and incoming ones are uncloaked at alpha 0, captured, then revealed. The capture costs one capture's latency at the start of a switch (1–3 frames with a GPU). This is the design section 3 already chose; nothing changes. `DwmRegisterThumbnail` wasn't needed. UWP apps weren't testable (none on Windows Server); for them the transition falls back to no animation if a capture fails (as every animation does).

### Phase 5.2: dimming inactive windows

Run on the same runner (`scripts/dim-spike.ps1`, results on `ci/dim-spike--claude-hyprland-parity`): a dark, 40% opaque, click-through helper window placed exactly over a Notepad and ordered just above it, while focus moves between that Notepad, a second one and a floating third.

| Step | Z-order, top to bottom | Right? |
|---|---|---|
| dim placed over A, B focused | B > dim > A > C | yes |
| A focused by the OS (a click) | A > B > dim > C | yes: A rises above its dim at once |
| dim put back above A by the WM | B > dim > A > C | no: ordering the dim relative to A moved A itself below B |
| C focused | C > B > dim > A | yes |

What it showed:

- A helper window can dim a window from just above it, and windows raised above it (focused, floating) stay undimmed.
- Keeping it there isn't robust. Ordering the helper relative to its window has to go through the window above the target (`GW_HWNDPREV`), which changes under the WM's feet. Any z-order change an app makes on its own (raising itself, always-on-top toggles, owned popups) comes as a coarse `EVENT_OBJECT_REORDER` with no window named, so the dims are wrong for at least a frame each time, until every one is re-placed.
- Each dimmed window costs a full-size layered window in the compositor, doubling the layers on screen.

Dropped, as section 3 planned for this outcome. `window_effects.other_windows.transparency` already gives inactive windows a muted look without any of this.

## 8. Checking smoothness on a real PC

CI's runner has no GPU, so Phase 2's target (p95 frame interval about one refresh period) is checked by hand on a PC with one:

1. Build the branch (`cargo build --release -p wm -p wm-cli`) and start it with `glazewm start --verbose`, so debug logs (and `glazewm::anim`) are printed. To keep only the timing lines, filter for `glazewm::anim`.
2. Use the animation tree with snapshot styles: `windows_move: { style: slide }`, `windows_in: { style: popin }`, `workspaces: { style: slide }` (the sample config's block, uncommented). Live `move` / `popin` with `live: true` tick per frame and aren't what's measured.
3. Move windows around, open and close a few, switch workspaces.
4. Read the `Compositor frames.` lines: one per run of animations, with `mean_ms`, `p95_ms`, `max_ms` and `late` (intervals over 1.5 frames), against `period_ms` (the monitor's refresh period). On a 60 Hz display, p95 should be about 16.7 ms and `late` near 0. `Animation frames.` lines with `frames=0` are runs the compositor played alone; `First animation frame.` gives `start_ms`, from the command to the first frame.

If p95 stays well above the period, the logs and a short screen recording are what's needed to look into it.

### The new-window flash

A new window could show for a frame where its app put it before its open animation. `scripts/flash-spike.ps1` (results on `ci/flash-spike--claude-hyprland-parity`) records a test window at 60 fps, hidden different ways:

| Hidden | Frames shown before the reveal |
|---|---|
| not at all | every one: its title bar on the first frame, then all of it |
| by itself, before it's shown: layered alpha 0, the same with DWM's open transition off, `DWMWA_CLOAK`, or off screen | none |
| by another process, cloaked (`DWMWA_CLOAK`) | refused: `E_ACCESSDENIED`, it only works on a process's own windows |
| by another process when it's created, shown right away as apps do (alpha 0), 20 times | 13 of 20 flashed, 16 of 20 with the open transition off |

The hook got each creation 15–32 ms late. Transparency hides a window completely, title bar included, when it's applied before DWM first draws the window; the flash is the race lost. `SetWinEventHook` out of context, the only hook that runs in GlazeWM's own process, hears of a window only after the fact, while an app shows its window right after creating it, so DWM often draws a frame first. Nothing outside the app's process can act during the creation itself, short of loading code into the app.

What was done about it: new windows aren't hidden at all. A new window stays where its app opened it, and its snapshot (or, with `move`, the window itself) glides from there into its place, so it never appears and then vanishes or jumps. Hiding on show and on creation, and the settle that held other windows back while a new one was hidden, were removed. CI frames (run 41181748307) show each new Notepad appear once and glide, with no frame where it disappears.
