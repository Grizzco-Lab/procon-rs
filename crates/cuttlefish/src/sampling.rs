//! Which frames of a video the reviewer sees, at what size, and what they
//! cost.
//!
//! - **A moment** ([`moment_times`]): dense near `t` ([`DENSE_FPS`] within
//!   ±[`DENSE_S`]), sparse further out (one a second from [`MOMENT_S`]`.0`
//!   before to [`MOMENT_S`]`.1` after), [`MOMENT_HEIGHT`] lines high.
//! - **A range** of at most [`MAX_RANGE_S`]: frames at the chosen rate and
//!   height (defaults [`RANGE_FPS`], [`RANGE_HEIGHT`]), placed by
//!   [`weighted_times`] so that more fall where the picture changes (a
//!   cheap frame difference, [`change_weights`]) or the HUD's wave changes,
//!   fewer in calm stretches. A range longer than [`TWO_PASS_S`] takes two
//!   calls: an overview at [`SCOUT_FPS`] and [`SCOUT_HEIGHT`] in which the
//!   model picks up to [`KEY_MOMENTS`] key moments, then frames around
//!   those only ([`key_times`]) at the chosen height, with the answer.
//!
//! Times sit on a grid of [`GRID_FPS`], so identical frames have identical
//! times (the lab caches them on disk by time and height). Frames are
//! never upscaled ([`scaled_size`]); an image costs about width × height /
//! 750 input tokens ([`image_tokens`]).

use alloc::vec::Vec;

/// Frame times are multiples of 1 / this, seconds
pub const GRID_FPS: f64 = 10.0;
/// Seconds before and after `t` that a question about a moment covers
pub const MOMENT_S: (f64, f64) = (4.0, 2.0);
/// Half width of the dense stretch around a moment, seconds
pub const DENSE_S: f64 = 1.0;
/// Frames per second within [`DENSE_S`] of a moment
pub const DENSE_FPS: f64 = 5.0;
/// Frames per second in the rest of a moment
pub const SPARSE_FPS: f64 = 1.0;
/// Height of a moment's frames, before the no-upscale rule
pub const MOMENT_HEIGHT: u32 = 720;
/// Longest range a message may ask about, seconds
pub const MAX_RANGE_S: f64 = 100.0;
/// Default frames per second of a range
pub const RANGE_FPS: f64 = 1.0;
/// Default height of a range's frames
pub const RANGE_HEIGHT: u32 = 480;
/// Highest frame rate and height a message may ask for
pub const MAX_FPS: f64 = 4.0;
pub const MAX_HEIGHT: u32 = 720;
/// Lowest height a message may ask for
pub const MIN_HEIGHT: u32 = 144;
/// Ranges longer than this take two passes, seconds
pub const TWO_PASS_S: f64 = 20.0;
/// Frames per second and height of the first pass's overview
pub const SCOUT_FPS: f64 = 0.5;
pub const SCOUT_HEIGHT: u32 = 360;
/// Key moments the first pass picks at most
pub const KEY_MOMENTS: usize = 5;
/// Frames around each key moment in the second pass, seconds from it
pub const KEY_OFFSETS: [f64; 5] = [-1.0, -0.4, 0.0, 0.4, 1.0];
/// Frames sent in one call at most
pub const MAX_FRAMES: usize = 60;
/// Frames per second of the frame-difference signal of a range
pub const SIGNAL_FPS: f64 = 2.0;
/// Width and height of the grey thumbnails the signal compares
pub const SIGNAL_SIZE: (u32, u32) = (32, 18);
/// Seconds around a HUD event (a wave starting or ending) that weigh more
pub const EVENT_S: f64 = 1.0;
/// Weight of a signal slot: [`CALM_WEIGHT`] plus its change over the
/// range's mean change, at most [`CHANGE_CAP`]; [`EVENT_WEIGHT`] more near
/// a HUD event
pub const CALM_WEIGHT: f64 = 0.5;
pub const CHANGE_CAP: f64 = 4.0;
pub const EVENT_WEIGHT: f64 = 3.0;

/// A time put on the [`GRID_FPS`] grid
pub fn on_grid(t_s: f64) -> f64 {
    (t_s * GRID_FPS).round() / GRID_FPS
}

/// The grid slot of a time: tenths of a second, for cache keys
pub fn grid_index(t_s: f64) -> u64 {
    (t_s.max(0.0) * GRID_FPS).round() as u64
}

/// Sorted, on the grid, within `0..=duration_s` (when known), without
/// repeats
fn tidy(times: impl IntoIterator<Item = f64>, duration_s: Option<f64>) -> Vec<f64> {
    let end = duration_s.unwrap_or(f64::INFINITY);
    let mut slots: Vec<u64> = times
        .into_iter()
        .filter(|t| t.is_finite() && *t >= -0.05 && *t <= end)
        .map(grid_index)
        .collect();
    slots.sort_unstable();
    slots.dedup();
    slots.into_iter().map(|s| s as f64 / GRID_FPS).collect()
}

/// The frame times of a moment: [`DENSE_FPS`] within [`DENSE_S`] of `t_s`,
/// [`SPARSE_FPS`] out to [`MOMENT_S`] (15 frames), inside the video
pub fn moment_times(t_s: f64, duration_s: Option<f64>) -> Vec<f64> {
    let t = on_grid(t_s);
    let dense = (DENSE_S * DENSE_FPS).round() as i32;
    let mut times: Vec<f64> = (-dense..=dense)
        .map(|i| t + f64::from(i) / DENSE_FPS)
        .collect();
    let mut s = DENSE_S + 1.0 / SPARSE_FPS;
    while s <= MOMENT_S.0 + 1e-9 {
        times.push(t - s);
        s += 1.0 / SPARSE_FPS;
    }
    let mut s = DENSE_S + 1.0 / SPARSE_FPS;
    while s <= MOMENT_S.1 + 1e-9 {
        times.push(t + s);
        s += 1.0 / SPARSE_FPS;
    }
    tidy(times, duration_s)
}

/// The start and end of what a message covers: the range, or the moment's
/// [`MOMENT_S`] around `t_s`
pub fn span(t_s: f64, t_end_s: Option<f64>) -> (f64, f64) {
    match t_end_s {
        Some(end) => (t_s, end),
        None => ((t_s - MOMENT_S.0).max(0.0), t_s + MOMENT_S.1),
    }
}

/// Frames of a stretch at `fps`, at most [`MAX_FRAMES`]
pub fn frame_count(span_s: f64, fps: f64) -> usize {
    ((span_s * fps).ceil() as usize).clamp(1, MAX_FRAMES)
}

/// Weights of the signal's slots (one per 1 / [`SIGNAL_FPS`] from the
/// range's start): each slot's change (mean absolute difference of its grey
/// thumbnail to the previous one) over the range's mean, capped, on a calm
/// floor, plus [`EVENT_WEIGHT`] within [`EVENT_S`] of an event
pub fn change_weights(diffs: &[f64], start_s: f64, events: &[f64]) -> Vec<f64> {
    let mean = diffs.iter().sum::<f64>() / diffs.len().max(1) as f64;
    diffs
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let change = if mean > 0.0 {
                (d / mean).min(CHANGE_CAP)
            } else {
                0.0
            };
            let t = start_s + (i as f64 + 0.5) / SIGNAL_FPS;
            let event = events.iter().any(|e| (e - t).abs() <= EVENT_S);
            CALM_WEIGHT + change + if event { EVENT_WEIGHT } else { 0.0 }
        })
        .collect()
}

/// `count` frame times over `start_s..end_s`, spread by `weights` (slots of
/// equal length over the range; empty for even spacing): the times at
/// even steps of the weights' running sum, so a slot twice as heavy gets
/// twice the frames. On the grid, without repeats.
pub fn weighted_times(start_s: f64, end_s: f64, count: usize, weights: &[f64]) -> Vec<f64> {
    let span = (end_s - start_s).max(0.0);
    let even = [1.0];
    let weights = if weights.iter().any(|w| *w > 0.0) {
        weights
    } else {
        &even[..]
    };
    let slot = span / weights.len() as f64;
    let total: f64 = weights.iter().map(|w| w.max(0.0)).sum();
    let mut times = Vec::with_capacity(count);
    let (mut i, mut before) = (0, 0.0);
    for k in 0..count {
        let target = (k as f64 + 0.5) / count as f64 * total;
        while i + 1 < weights.len() && before + weights[i].max(0.0) < target {
            before += weights[i].max(0.0);
            i += 1;
        }
        let w = weights[i].max(0.0);
        let within = if w > 0.0 { (target - before) / w } else { 0.5 };
        times.push(start_s + (i as f64 + within.clamp(0.0, 1.0)) * slot);
    }
    tidy(times, Some(end_s))
}

/// The second pass's frame times: [`KEY_OFFSETS`] around each key moment,
/// inside `start_s..=end_s`
pub fn key_times(moments: &[f64], start_s: f64, end_s: f64) -> Vec<f64> {
    let times = moments
        .iter()
        .flat_map(|m| KEY_OFFSETS.iter().map(move |o| m + o))
        .filter(|t| *t >= start_s - 0.05);
    tidy(times, Some(end_s))
}

/// Width and height of a frame scaled to `height` lines, never above the
/// source's: the width keeps the aspect ratio and is even, as ffmpeg's
/// `scale=-2:h` makes it
pub fn scaled_size(source: (u32, u32), height: u32) -> (u32, u32) {
    let (w, h) = source;
    if w == 0 || h == 0 {
        let h = height;
        return ((f64::from(h) * 16.0 / 9.0 / 2.0).round() as u32 * 2, h);
    }
    let out_h = height.min(h);
    let out_w = (f64::from(w) * f64::from(out_h) / f64::from(h) / 2.0).round() as u32 * 2;
    (out_w.max(2), out_h)
}

/// Input tokens of an image, about width × height / 750
pub fn image_tokens((w, h): (u32, u32)) -> u64 {
    (u64::from(w) * u64::from(h)).div_ceil(750)
}

/// What a message about the video sends: frames per call and their size
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Estimate {
    /// Frames of the first pass (0 without one) and their size
    pub scout: (usize, (u32, u32)),
    /// Frames sent with the answer (at most, with two passes) and their size
    pub answer: (usize, (u32, u32)),
}

impl Estimate {
    /// Image tokens of both calls
    pub fn tokens(&self) -> u64 {
        self.scout.0 as u64 * image_tokens(self.scout.1)
            + self.answer.0 as u64 * image_tokens(self.answer.1)
    }

    /// Whether the message takes two passes
    pub fn two_pass(&self) -> bool {
        self.scout.0 > 0
    }
}

/// What a message sends for a moment (`t_end_s` none) or a range, at `fps`
/// and `height` for a range, from a video of `source` size
pub fn estimate(
    source: (u32, u32),
    t_s: f64,
    t_end_s: Option<f64>,
    fps: f64,
    height: u32,
) -> Estimate {
    let Some(end) = t_end_s else {
        return Estimate {
            scout: (0, (0, 0)),
            answer: (
                moment_times(t_s, None).len(),
                scaled_size(source, MOMENT_HEIGHT),
            ),
        };
    };
    let span = end - t_s;
    if span > TWO_PASS_S {
        Estimate {
            scout: (
                frame_count(span, SCOUT_FPS),
                scaled_size(source, SCOUT_HEIGHT),
            ),
            answer: (KEY_MOMENTS * KEY_OFFSETS.len(), scaled_size(source, height)),
        }
    } else {
        Estimate {
            scout: (0, (0, 0)),
            answer: (frame_count(span, fps), scaled_size(source, height)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moments_are_dense_near_t() {
        let times = moment_times(30.03, None);
        assert_eq!(times.len(), 15);
        assert_eq!(times.first(), Some(&26.0));
        assert_eq!(times.last(), Some(&32.0));
        let near = times.iter().filter(|t| (**t - 30.0).abs() <= 1.0).count();
        assert_eq!(near, 11);
        assert!(times.contains(&30.0));
        assert!(times.contains(&29.8));
        // One a second further out
        assert!(times.contains(&27.0) && !times.contains(&27.5));
        // Clipped to the video
        let start = moment_times(0.5, Some(1.0));
        assert_eq!(start.first(), Some(&0.1));
        assert_eq!(start.last(), Some(&0.9));
        assert!(start.iter().all(|t| (0.0..=1.0).contains(t)));
    }

    #[test]
    fn busy_stretches_get_more_frames() {
        // 20 s at 2 slots a second: calm first half, busy second half
        let diffs: Vec<f64> = (0..40).map(|i| if i < 20 { 0.5 } else { 8.0 }).collect();
        let w = change_weights(&diffs, 0.0, &[]);
        let times = weighted_times(0.0, 20.0, 20, &w);
        assert_eq!(times.len(), 20);
        let busy = times.iter().filter(|t| **t >= 10.0).count();
        assert!(busy >= 15, "{times:?}");
        assert!(times.windows(2).all(|p| p[0] < p[1]));
        // A wave starting in a calm stretch draws frames to it
        let calm = alloc::vec![1.0; 40];
        let w = change_weights(&calm, 0.0, &[5.0]);
        let times = weighted_times(0.0, 20.0, 10, &w);
        let near = times.iter().filter(|t| (**t - 5.0).abs() <= 1.0).count();
        assert!(near >= 2, "{times:?}");
        // No signal: evenly spaced
        assert_eq!(
            weighted_times(10.0, 20.0, 5, &[]),
            [11.0, 13.0, 15.0, 17.0, 19.0]
        );
    }

    #[test]
    fn key_moments_get_frames_around_them() {
        let times = key_times(&[10.0, 10.4, 50.0], 9.5, 50.5);
        // Overlaps merged, clipped to the range
        assert_eq!(
            times,
            [9.6, 10.0, 10.4, 10.8, 11.0, 11.4, 49.0, 49.6, 50.0, 50.4]
        );
    }

    #[test]
    fn frames_are_never_upscaled() {
        assert_eq!(scaled_size((640, 360), 720), (640, 360));
        assert_eq!(scaled_size((1920, 1080), 720), (1280, 720));
        assert_eq!(scaled_size((1920, 1080), 480), (854, 480));
        assert_eq!(scaled_size((854, 480), 720), (854, 480));
        assert_eq!(scaled_size((1920, 1080), 360), (640, 360));
        // Unknown size: 16:9 at the height asked
        assert_eq!(scaled_size((0, 0), 480), (854, 480));
    }

    #[test]
    fn token_estimates() {
        assert_eq!(image_tokens((1280, 720)), 1229);
        assert_eq!(image_tokens((640, 360)), 308);
        let hd = (1920, 1080);
        // A moment: 15 frames at 720p
        let m = estimate(hd, 60.0, None, RANGE_FPS, RANGE_HEIGHT);
        assert_eq!(m.answer.0, 15);
        assert_eq!(m.tokens(), 15 * 1229);
        assert!(!m.two_pass());
        // The same moment of a 360p video costs a quarter
        assert_eq!(
            estimate((640, 360), 60.0, None, 1.0, 480).tokens(),
            15 * 308
        );
        // 15 s range at the defaults: one pass
        let short = estimate(hd, 0.0, Some(15.0), RANGE_FPS, RANGE_HEIGHT);
        assert_eq!(short.answer, (15, (854, 480)));
        assert!(!short.two_pass());
        // 30 s and 100 s: an overview, then at most 25 frames around the key moments
        let r30 = estimate(hd, 0.0, Some(30.0), RANGE_FPS, RANGE_HEIGHT);
        assert!(r30.two_pass());
        assert_eq!(r30.scout, (15, (640, 360)));
        assert_eq!(r30.answer, (25, (854, 480)));
        assert_eq!(r30.tokens(), 15 * 308 + 25 * 547);
        let r100 = estimate(hd, 0.0, Some(100.0), RANGE_FPS, RANGE_HEIGHT);
        assert_eq!(r100.scout.0, 50);
        // Short ranges at a high rate stop at MAX_FRAMES
        assert_eq!(frame_count(20.0, MAX_FPS), MAX_FRAMES);
    }
}
