//! Controller actions per video frame.
//!
//! Frame `n` covers `[t_n, t_n + frame_ms)`, where `t_n` is when it was on
//! screen on the host clock. Two offsets map both streams onto the time the
//! player saw the frame and pressed the buttons:
//!
//! - `controller_shift_ms` is added to the proxy's report timestamps (both
//!   machines sync to NTP, so it is usually 0);
//! - `video_delay_ms` is the latency from input to the recorded frame: the
//!   frame at video time `t` shows the input from `t - video_delay_ms`.
//!
//! Sums over each frame come from prefix sums, in the order the reports and
//! IMU samples arrived.

use crate::controller::{BUTTONS, ControllerLog};
use alloc::vec::Vec;

/// Actions over each video frame of a segment
///
/// Frames without any report in their interval have `mask` false and zeros
/// elsewhere.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FrameActions {
    /// Host Unix ms at which each frame was on screen
    pub time_ms: Vec<f64>,
    /// Whether any input report falls in the frame
    pub mask: Vec<bool>,
    /// Buttons pressed in any report of the frame, as masks over [`BUTTONS`]
    pub buttons: Vec<u32>,
    /// Buttons pressed in every report of the frame
    pub buttons_held: Vec<u32>,
    /// Mean raw 12-bit sticks (lx, ly, rx, ry)
    pub sticks: Vec<[f32; 4]>,
    /// Rotation over the frame in degrees (x, y, z)
    pub gyro: Vec<[f32; 3]>,
    /// Mean acceleration in g (x, y, z)
    pub accel: Vec<[f32; 3]>,
}

impl FrameActions {
    /// Number of frames
    pub fn len(&self) -> usize {
        self.time_ms.len()
    }

    /// Whether there are no frames
    pub fn is_empty(&self) -> bool {
        self.time_ms.is_empty()
    }
}

/// On-screen times of a constant-rate segment's frames: frame `n` at
/// `start_unix_ms + n * 1000 / fps - video_delay_ms`
pub fn constant_rate_times(
    start_unix_ms: u64,
    count: usize,
    fps: f64,
    video_delay_ms: f64,
) -> Vec<f64> {
    (0..count)
        .map(|n| start_unix_ms as f64 + (n * 1000) as f64 / fps - video_delay_ms)
        .collect()
}

/// On-screen times of a variable-rate segment's frames, from their
/// presentation times in the file (ms, sorted)
pub fn variable_rate_times(start_unix_ms: u64, pts_ms: &[f64], video_delay_ms: f64) -> Vec<f64> {
    pts_ms
        .iter()
        .map(|pts| start_unix_ms as f64 + pts - video_delay_ms)
        .collect()
}

/// Column-wise running sums with a leading zero row, and per-interval
/// differences of them
struct Prefix<const N: usize>(Vec<[f64; N]>);

impl<const N: usize> Prefix<N> {
    fn new(rows: impl Iterator<Item = [f64; N]>) -> Self {
        let mut sums = alloc::vec![[0.0; N]];
        for row in rows {
            let last = sums[sums.len() - 1];
            sums.push(core::array::from_fn(|i| last[i] + row[i]));
        }
        Self(sums)
    }

    fn between(&self, lo: usize, hi: usize) -> [f64; N] {
        core::array::from_fn(|i| self.0[hi][i] - self.0[lo][i])
    }
}

/// Indices `[lo, hi)` of the sorted `times` inside `[start, stop)`
fn span(times: &[f64], start: f64, stop: f64) -> (usize, usize) {
    (
        times.partition_point(|t| *t < start),
        times.partition_point(|t| *t < stop),
    )
}

/// Aggregate the controller log over video frames
///
/// `frame_time_ms` are the sorted on-screen times of the frames and
/// `frame_ms` their duration (`1000 / fps`). Buttons are pressed if pressed
/// in any (`buttons`) or every (`buttons_held`) report of the frame, sticks
/// and acceleration are means, and the gyro is integrated to degrees over
/// the IMU samples in the frame.
pub fn align(
    log: &ControllerLog,
    frame_time_ms: &[f64],
    frame_ms: f64,
    controller_shift_ms: f64,
) -> FrameActions {
    let report_ms: Vec<f64> = log
        .time_ms
        .iter()
        .map(|t| *t as f64 + controller_shift_ms)
        .collect();
    let imu_ms: Vec<f64> = log
        .imu_time_ms
        .iter()
        .map(|t| t + controller_shift_ms)
        .collect();
    let pressed = Prefix::<{ BUTTONS.len() }>::new(
        log.buttons
            .iter()
            .map(|bits| core::array::from_fn(|i| f64::from(bits >> i & 1))),
    );
    let sticks = Prefix::new(log.sticks.iter().map(|s| s.map(f64::from)));
    let rotation = Prefix::new(
        log.gyro
            .iter()
            .zip(&log.imu_dt_ms)
            .map(|(g, dt)| g.map(|rate| f64::from(rate) * dt / 1000.0)),
    );
    let accel = Prefix::new(log.accel.iter().map(|a| a.map(f64::from)));

    let mut actions = FrameActions {
        time_ms: frame_time_ms.to_vec(),
        ..FrameActions::default()
    };
    for &start in frame_time_ms {
        let stop = start + frame_ms;
        let (lo, hi) = span(&report_ms, start, stop);
        let count = (hi - lo) as f64;
        let per_button = pressed.between(lo, hi);
        let bits = |held: fn(f64, f64) -> bool| {
            (0..BUTTONS.len())
                .filter(|i| held(per_button[*i], count))
                .fold(0u32, |bits, i| bits | 1 << i)
        };
        actions.mask.push(hi > lo);
        actions.buttons.push(bits(|n, _| n > 0.0));
        actions.buttons_held.push(if hi > lo {
            bits(|n, count| n == count)
        } else {
            0
        });
        let divisor = count.max(1.0);
        actions
            .sticks
            .push(sticks.between(lo, hi).map(|sum| (sum / divisor) as f32));

        let (lo, hi) = span(&imu_ms, start, stop);
        let divisor = ((hi - lo) as f64).max(1.0);
        actions
            .gyro
            .push(rotation.between(lo, hi).map(|sum| sum as f32));
        actions
            .accel
            .push(accel.between(lo, hi).map(|sum| (sum / divisor) as f32));
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A log whose reports press Y or not and rotate at a constant rate,
    /// three IMU samples at each report's time
    fn log(time_ms: &[u64], pressed: &[bool], gyro_dps: f32) -> ControllerLog {
        let n = time_ms.len();
        ControllerLog {
            time_ms: time_ms.to_vec(),
            seq: (0..n as u32).collect(),
            report_id: alloc::vec![0x30; n],
            forward_us: alloc::vec![0; n],
            buttons: pressed.iter().map(|p| u32::from(*p)).collect(),
            sticks: alloc::vec![[2048; 4]; n],
            imu_time_ms: time_ms.iter().flat_map(|t| [*t as f64; 3]).collect(),
            imu_dt_ms: alloc::vec![10.0 / 3.0; 3 * n],
            gyro: alloc::vec![[gyro_dps; 3]; 3 * n],
            accel: alloc::vec![[0.0; 3]; 3 * n],
            dropped: 0,
        }
    }

    #[test]
    fn frames() {
        let log = log(
            &[0, 10, 20, 30, 80],
            &[false, true, false, true, true],
            100.0,
        );
        let actions = align(&log, &[0.0, 25.0, 50.0], 25.0, 0.0);
        assert_eq!(actions.mask, [true, true, false]);
        assert_eq!(actions.buttons, [1, 1, 0]);
        assert_eq!(actions.buttons_held, [0, 1, 0]);
        // Reports at 0, 10, 20: 9 samples of 10/3 ms at 100 deg/s = 3 degrees
        assert!((actions.gyro[0][0] - 3.0).abs() < 1e-6);
        assert_eq!(actions.gyro[2], [0.0; 3]);
        assert_eq!(actions.sticks[0], [2048.0; 4]);
        assert_eq!(actions.sticks[2], [0.0; 4]);
        // Shifting the controller by +30 ms moves all reports past the first frame
        let shifted = align(&log, &[0.0, 25.0, 50.0], 25.0, 30.0);
        assert_eq!(shifted.mask, [false, true, true]);
    }

    #[test]
    fn interval_edges() {
        // A report exactly at a frame's start belongs to it, at its end to the next
        let log = log(&[25], &[true], 0.0);
        let actions = align(&log, &[0.0, 25.0], 25.0, 0.0);
        assert_eq!(actions.mask, [false, true]);
    }

    #[test]
    fn empty_log_and_no_frames() {
        let actions = align(&ControllerLog::default(), &[0.0, 33.3], 33.3, 0.0);
        assert_eq!(actions.mask, [false, false]);
        assert_eq!(actions.gyro, [[0.0; 3]; 2]);
        assert!(align(&log(&[0], &[true], 1.0), &[], 33.3, 0.0).is_empty());
    }

    #[test]
    fn frame_times() {
        let times = constant_rate_times(1000, 3, 30.0, 100.0);
        assert_eq!(
            times,
            [
                900.0,
                1000.0 + 1000.0 / 30.0 - 100.0,
                1000.0 + 2000.0 / 30.0 - 100.0
            ]
        );
        assert_eq!(
            variable_rate_times(1000, &[0.0, 17.0], 10.0),
            [990.0, 1007.0]
        );
    }
}
