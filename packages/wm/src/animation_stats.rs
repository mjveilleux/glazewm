use std::time::{Duration, Instant};

use wm_platform::{compositor_frame_counts, CompositorFrameCounts};

/// Log target for animation timing, for filtering and for the CI check
/// that reads it (`scripts/animation-check.ps1`).
pub const LOG_TARGET: &str = "glazewm::anim";

/// A frame interval longer than this many frame periods counts as late.
const LATE_FACTOR: f64 = 1.5;

/// Measures how smoothly animations run, and logs it on [`LOG_TARGET`].
///
/// Animations that overlap are measured together, as one run: from the
/// first starting to the last ending.
#[derive(Debug, Default)]
pub struct AnimationStats {
  /// When the next animation to start was asked for.
  requested_at: Option<Instant>,

  /// When the running animations were asked for, until their first frame
  /// is drawn.
  awaiting_first_frame: Option<Instant>,

  /// The animations running now, if any.
  run: Option<Run>,
}

/// Timing of one run of animations.
#[derive(Debug)]
struct Run {
  started_at: Instant,

  /// Shortest frame period among the run's animations.
  frame_period: Duration,

  last_frame: Option<Instant>,

  /// Time between consecutive frames.
  intervals: Vec<Duration>,

  /// Compositor's frame counts as the run started.
  compositor_start: Option<CompositorFrameCounts>,

  /// Stop flag of the thread timing the compositor's frames meanwhile.
  #[cfg(target_os = "windows")]
  frame_probe: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

/// Summary of the time between an animation run's frames.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameSummary {
  pub frames: usize,
  pub mean_ms: f64,
  pub p95_ms: f64,
  pub max_ms: f64,

  /// Intervals longer than [`LATE_FACTOR`] frame periods.
  pub late: usize,
}

impl AnimationStats {
  /// Marks now as when the next animation to start was asked for, e.g.
  /// as a sync that may start animations begins.
  ///
  /// An earlier mark not yet used or cleared is kept.
  pub fn mark_request(&mut self) {
    self.requested_at.get_or_insert_with(Instant::now);
  }

  /// Forgets the mark of [`AnimationStats::mark_request`], e.g. once a
  /// sync is over.
  pub fn clear_request(&mut self) {
    self.requested_at = None;
  }

  /// Records that an animation has started, at `frame_rate` frames per
  /// second.
  pub fn animation_started(&mut self, frame_rate: u32) {
    let now = Instant::now();
    let requested_at = self.requested_at.unwrap_or(now);
    let frame_period =
      Duration::from_secs_f64(1.0 / f64::from(frame_rate.max(1)));

    if self.awaiting_first_frame.is_none() {
      self.awaiting_first_frame = Some(requested_at);
    }

    match &mut self.run {
      Some(run) => run.frame_period = run.frame_period.min(frame_period),
      None => {
        self.run = Some(Run {
          started_at: requested_at,
          frame_period,
          last_frame: None,
          intervals: Vec::new(),
          compositor_start: compositor_frame_counts(),
          #[cfg(target_os = "windows")]
          frame_probe: start_frame_probe(frame_period),
        });
      }
    }
  }

  /// Records that the running animations' first frame is shown at `at`,
  /// e.g. the compositor frame an animation it plays starts on.
  pub fn first_frame_at(&mut self, at: Instant) {
    if let Some(requested_at) = self.awaiting_first_frame.take() {
      tracing::debug!(
        target: LOG_TARGET,
        start_ms = %format_ms(at.saturating_duration_since(requested_at)),
        "First animation frame."
      );
    }
  }

  /// Records that a frame of the running animations was drawn by a tick.
  pub fn frame(&mut self) {
    let now = Instant::now();

    self.first_frame_at(now);

    if let Some(run) = &mut self.run {
      if let Some(last_frame) = run.last_frame {
        run.intervals.push(now - last_frame);
      }

      run.last_frame = Some(now);
    }
  }

  /// Records that no animation is running anymore, and logs the run's
  /// timing.
  pub fn idle(&mut self) {
    self.awaiting_first_frame = None;

    let Some(run) = self.run.take() else {
      return;
    };

    #[cfg(target_os = "windows")]
    if let Some(stop) = &run.frame_probe {
      stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    let duration = run.started_at.elapsed();

    // What the compositor did meanwhile. `expected` is how many frames a
    // display at the run's refresh rate shows in that time.
    let compositor = run
      .compositor_start
      .zip(compositor_frame_counts())
      .map(|(start, end)| {
        (
          end.composed.saturating_sub(start.composed),
          end.dropped.saturating_sub(start.dropped),
        )
      });

    // LINT: A frame count far below `u64::MAX`.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let expected = (duration.as_secs_f64()
      / run.frame_period.as_secs_f64())
    .round() as u64;

    let dwm_composed = compositor.map(|(composed, _)| composed);
    let dwm_dropped = compositor.map(|(_, dropped)| dropped);

    // Runs the compositor played on its own have no ticks to time.
    let Some(summary) = summarize(&run.intervals, run.frame_period) else {
      tracing::debug!(
        target: LOG_TARGET,
        frames = 0,
        period_ms = %format_ms(run.frame_period),
        duration_ms = %format_ms(duration),
        dwm_composed,
        dwm_dropped,
        dwm_expected = expected,
        "Animation frames."
      );
      return;
    };

    tracing::debug!(
      target: LOG_TARGET,
      frames = summary.frames,
      mean_ms = %format!("{:.1}", summary.mean_ms),
      p95_ms = %format!("{:.1}", summary.p95_ms),
      max_ms = %format!("{:.1}", summary.max_ms),
      late = summary.late,
      period_ms = %format_ms(run.frame_period),
      duration_ms = %format_ms(duration),
      dwm_composed,
      dwm_dropped,
      dwm_expected = expected,
      "Animation frames."
    );
  }
}

/// Starts a thread that times every frame the compositor shows until
/// stopped, then logs how evenly they came: what the eye sees of
/// animations the compositor plays on its own, which the WM doesn't tick.
///
/// Returns its stop flag, or `None` if it couldn't start.
#[cfg(target_os = "windows")]
fn start_frame_probe(
  frame_period: Duration,
) -> Option<std::sync::Arc<std::sync::atomic::AtomicBool>> {
  use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
  };

  let stop = Arc::new(AtomicBool::new(false));
  let thread_stop = stop.clone();

  let spawned = std::thread::Builder::new()
    .name("animation-frame-probe".to_string())
    .spawn(move || {
      let mut last = Instant::now();
      let mut intervals = Vec::new();

      while !thread_stop.load(Ordering::Relaxed) {
        if wm_platform::wait_for_compositor_frame().is_err() {
          return;
        }

        let now = Instant::now();
        intervals.push(now - last);
        last = now;
      }

      // The first wait started mid-frame; it's not a whole interval.
      if !intervals.is_empty() {
        intervals.remove(0);
      }

      if let Some(summary) = summarize(&intervals, frame_period) {
        tracing::debug!(
          target: LOG_TARGET,
          frames = summary.frames,
          mean_ms = %format!("{:.1}", summary.mean_ms),
          p95_ms = %format!("{:.1}", summary.p95_ms),
          max_ms = %format!("{:.1}", summary.max_ms),
          late = summary.late,
          period_ms = %format_ms(frame_period),
          "Compositor frames."
        );
      }
    });

  match spawned {
    Ok(_) => Some(stop),
    Err(err) => {
      tracing::debug!("Failed to start the frame probe: {err}");
      None
    }
  }
}

/// Formats a duration in milliseconds, to a tenth.
fn format_ms(duration: Duration) -> String {
  format!("{:.1}", duration.as_secs_f64() * 1000.0)
}

/// Summarizes the intervals between frames.
///
/// Returns `None` if there are none.
#[must_use]
pub fn summarize(
  intervals: &[Duration],
  frame_period: Duration,
) -> Option<FrameSummary> {
  if intervals.is_empty() {
    return None;
  }

  let mut ms = intervals
    .iter()
    .map(|interval| interval.as_secs_f64() * 1000.0)
    .collect::<Vec<_>>();

  ms.sort_by(f64::total_cmp);

  // LINT: Frame counts are small enough to be exact as `f64`.
  #[allow(clippy::cast_precision_loss)]
  let count = ms.len() as f64;

  // Nearest rank: the smallest value at or above 95% of the intervals.
  // LINT: The rank is between 1 and `ms.len()`.
  #[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
  )]
  let p95_index = ((count * 0.95).ceil() as usize).clamp(1, ms.len()) - 1;

  let late_ms = frame_period.as_secs_f64() * 1000.0 * LATE_FACTOR;

  Some(FrameSummary {
    // The first frame has no interval before it.
    frames: ms.len() + 1,
    mean_ms: ms.iter().sum::<f64>() / count,
    p95_ms: ms[p95_index],
    max_ms: ms[ms.len() - 1],
    late: ms.iter().filter(|&&interval| interval > late_ms).count(),
  })
}

#[cfg(test)]
mod tests {
  use std::time::Duration;

  use super::summarize;

  fn ms(values: &[u64]) -> Vec<Duration> {
    values.iter().map(|&v| Duration::from_millis(v)).collect()
  }

  #[test]
  fn no_intervals_no_summary() {
    assert!(summarize(&[], Duration::from_millis(16)).is_none());
  }

  #[test]
  fn even_frames() {
    let summary = summarize(&ms(&[16; 20]), Duration::from_millis(16))
      .expect("summary");

    assert_eq!(summary.frames, 21);
    assert!((summary.mean_ms - 16.0).abs() < 1e-9);
    assert!((summary.p95_ms - 16.0).abs() < 1e-9);
    assert_eq!(summary.late, 0);
  }

  #[test]
  fn late_frames_counted_and_in_the_tail() {
    let mut intervals = vec![16; 18];
    intervals.extend([40, 50]);

    let summary = summarize(&ms(&intervals), Duration::from_millis(16))
      .expect("summary");

    assert_eq!(summary.late, 2);
    assert!((summary.max_ms - 50.0).abs() < 1e-9);
    // 95% of 20 is 19: the 19th smallest.
    assert!((summary.p95_ms - 40.0).abs() < 1e-9);
  }
}
