//! Controller logs (`controller.bin`): the input reports of a session in
//! columns, heartbeats and non-input reports removed.
//!
//! Only report ids 0x30 (standard full input) and 0x21 (subcommand reply)
//! carry input; 0x30 also carries three IMU samples. Buttons and sticks stay
//! raw; the gyro is converted to deg/s and the accelerometer to g.

use crate::frame::{REPORT_SIZE, parse_frames};
use alloc::vec::Vec;
use anyhow::{Context, Result};
use std::path::Path;

/// Report ids that carry buttons and sticks
pub const INPUT_REPORT_IDS: [u8; 2] = [0x30, 0x21];

/// The only report id that carries IMU samples
pub const IMU_REPORT_ID: u8 = 0x30;

/// Button name, report byte and bit mask; bit `i` of a button mask is
/// `BUTTONS[i]`
pub const BUTTONS: [(&str, usize, u8); 22] = [
    ("y", 3, 0x01),
    ("x", 3, 0x02),
    ("b", 3, 0x04),
    ("a", 3, 0x08),
    ("sr_right", 3, 0x10),
    ("sl_right", 3, 0x20),
    ("r", 3, 0x40),
    ("zr", 3, 0x80),
    ("minus", 4, 0x01),
    ("plus", 4, 0x02),
    ("r_stick", 4, 0x04),
    ("l_stick", 4, 0x08),
    ("home", 4, 0x10),
    ("capture", 4, 0x20),
    ("down", 5, 0x01),
    ("up", 5, 0x02),
    ("right", 5, 0x04),
    ("left", 5, 0x08),
    ("sr_left", 5, 0x10),
    ("sl_left", 5, 0x20),
    ("l", 5, 0x40),
    ("zl", 5, 0x80),
];

/// Stick columns: raw 12-bit, center about 2048
pub const STICK_NAMES: [&str; 4] = ["lx", "ly", "rx", "ry"];

/// Byte offset of the first of the three IMU samples in a 0x30 report; each
/// sample is six little-endian i16 (accel x, y, z, gyro x, y, z)
pub const IMU_OFFSET: usize = 13;

/// Gyro sensitivity in deg/s per raw unit
pub const GYRO_DPS_PER_LSB: f32 = 0.07;

/// Accelerometer sensitivity in g per raw unit (±8 g range)
pub const ACCEL_G_PER_LSB: f32 = 1.0 / 4096.0;

/// Longest report interval the IMU samples are taken to cover, so a pause
/// in reports cannot turn into a large rotation
pub const MAX_REPORT_INTERVAL_MS: f64 = 50.0;

/// Buttons pressed in a report, as a mask over [`BUTTONS`]
pub fn parse_buttons(report: &[u8; REPORT_SIZE]) -> u32 {
    BUTTONS
        .iter()
        .enumerate()
        .filter(|(_, (_, byte, mask))| report[*byte] & mask != 0)
        .fold(0, |bits, (i, _)| bits | 1 << i)
}

/// The two 12-bit sticks of a report, columns [`STICK_NAMES`]
pub fn parse_sticks(report: &[u8; REPORT_SIZE]) -> [u16; 4] {
    let stick = |offset: usize| {
        let [b0, b1, b2] = [0, 1, 2].map(|i| u16::from(report[offset + i]));
        [b0 | (b1 & 0x0F) << 8, b1 >> 4 | b2 << 4]
    };
    let [lx, ly] = stick(6);
    let [rx, ry] = stick(9);
    [lx, ly, rx, ry]
}

/// The three raw IMU samples of a 0x30 report, oldest first
pub fn parse_imu(report: &[u8; REPORT_SIZE]) -> [[i16; 6]; 3] {
    core::array::from_fn(|sample| {
        core::array::from_fn(|axis| {
            let at = IMU_OFFSET + 12 * sample + 2 * axis;
            i16::from_le_bytes([report[at], report[at + 1]])
        })
    })
}

/// Input reports of one recording in columns
///
/// Report columns have one entry per input report (ids 0x30 and 0x21); IMU
/// columns have one entry per IMU sample (three per 0x30 report). Times are
/// Unix ms on the proxy's clock.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ControllerLog {
    /// When the proxy read each report
    pub time_ms: Vec<u64>,
    /// Sequence numbers
    pub seq: Vec<u32>,
    /// Report ids, 0x30 or 0x21
    pub report_id: Vec<u8>,
    /// Microseconds until the console took each report; 0 when unknown
    pub forward_us: Vec<u16>,
    /// Pressed buttons as masks over [`BUTTONS`]
    pub buttons: Vec<u32>,
    /// Raw sticks, columns [`STICK_NAMES`]
    pub sticks: Vec<[u16; 4]>,
    /// Estimated time of each IMU sample
    pub imu_time_ms: Vec<f64>,
    /// Time each IMU sample stands for
    pub imu_dt_ms: Vec<f64>,
    /// Angular rate in deg/s (x, y, z)
    pub gyro: Vec<[f32; 3]>,
    /// Acceleration in g (x, y, z)
    pub accel: Vec<[f32; 3]>,
    /// Reports missing from the sequence
    pub dropped: u64,
}

impl ControllerLog {
    /// Read a `controller.bin` file
    pub fn read(path: &Path) -> Result<Self> {
        let bytes =
            std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
        Ok(Self::parse(&bytes))
    }

    /// Decode the bytes of a `controller.bin` file
    ///
    /// The three IMU samples of a 0x30 report are taken to be equally
    /// spaced over the interval since the previous 0x30 report, ending at
    /// the report's timestamp. That interval is capped at
    /// [`MAX_REPORT_INTERVAL_MS`]; the first report uses the median
    /// interval.
    pub fn parse(bytes: &[u8]) -> Self {
        let frames: Vec<_> = parse_frames(bytes).filter(|f| !f.is_heartbeat()).collect();
        let mut log = Self {
            // Sequence numbers wrap, and so do their differences
            dropped: frames
                .windows(2)
                .map(|pair| u64::from(pair[1].seq.wrapping_sub(pair[0].seq)).saturating_sub(1))
                .sum(),
            ..Self::default()
        };
        let inputs = frames
            .iter()
            .filter(|f| INPUT_REPORT_IDS.contains(&f.report[0]));
        let mut imu_reports = Vec::new();
        for frame in inputs {
            log.time_ms.push(frame.timestamp_ms);
            log.seq.push(frame.seq);
            log.report_id.push(frame.report[0]);
            log.forward_us.push(frame.forward_us);
            log.buttons.push(parse_buttons(&frame.report));
            log.sticks.push(parse_sticks(&frame.report));
            if frame.report[0] == IMU_REPORT_ID {
                imu_reports.push((frame.timestamp_ms as f64, parse_imu(&frame.report)));
            }
        }

        let intervals = report_intervals(&imu_reports.iter().map(|r| r.0).collect::<Vec<_>>());
        for ((time_ms, samples), interval) in imu_reports.iter().zip(intervals) {
            // Samples end at the report time: t - 2/3 dt, t - 1/3 dt, t
            for (step, sample) in [2.0 / 3.0, 1.0 / 3.0, 0.0].iter().zip(samples) {
                log.imu_time_ms.push(time_ms - interval * step);
                log.imu_dt_ms.push(interval / 3.0);
                let value = |axis: usize| f32::from(sample[axis]);
                log.accel
                    .push([0, 1, 2].map(|axis| value(axis) * ACCEL_G_PER_LSB));
                log.gyro
                    .push([3, 4, 5].map(|axis| value(axis) * GYRO_DPS_PER_LSB));
            }
        }
        log
    }

    /// Number of input reports
    pub fn len(&self) -> usize {
        self.time_ms.len()
    }

    /// Whether there are no input reports
    pub fn is_empty(&self) -> bool {
        self.time_ms.is_empty()
    }
}

/// Time each IMU report covers: the interval since the previous one (the
/// median interval for the first), capped at [`MAX_REPORT_INTERVAL_MS`]
fn report_intervals(times: &[f64]) -> Vec<f64> {
    let mut intervals: Vec<f64> = times.windows(2).map(|w| w[1] - w[0]).collect();
    let first = median(&intervals).unwrap_or(0.0);
    if !times.is_empty() {
        intervals.insert(0, first);
    }
    intervals
        .into_iter()
        .map(|i| i.clamp(0.0, MAX_REPORT_INTERVAL_MS))
        .collect()
}

/// Median, the mean of the two middle values for an even count
fn median(values: &[f64]) -> Option<f64> {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    match n {
        0 => None,
        _ if n % 2 == 1 => Some(sorted[n / 2]),
        _ => Some((sorted[n / 2 - 1] + sorted[n / 2]) / 2.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{FRAME_SIZE, Frame};

    /// A 0x30 report with ZR, Plus and ZL pressed, known sticks and IMU
    fn report() -> [u8; REPORT_SIZE] {
        let mut report = [0; REPORT_SIZE];
        report[0] = 0x30;
        report[3..6].copy_from_slice(&[0x80, 0x02, 0x80]);
        // Left stick (x=0x123, y=0x456), right stick (x=0xFFF, y=0x000)
        report[6..9].copy_from_slice(&[0x23, 0x61, 0x45]);
        report[9..12].copy_from_slice(&[0xFF, 0x0F, 0x00]);
        for (i, value) in (-9i16..9).enumerate() {
            report[IMU_OFFSET + 2 * i..IMU_OFFSET + 2 * i + 2]
                .copy_from_slice(&value.to_le_bytes());
        }
        report
    }

    fn file(frames: &[Frame]) -> Vec<u8> {
        frames.iter().flat_map(|f| f.to_bytes()).collect()
    }

    #[test]
    fn parse_report() {
        let report = report();
        let pressed: Vec<_> = (0..BUTTONS.len())
            .filter(|i| parse_buttons(&report) & 1 << i != 0)
            .map(|i| BUTTONS[i].0)
            .collect();
        assert_eq!(pressed, ["zr", "plus", "zl"]);
        assert_eq!(parse_sticks(&report), [0x123, 0x456, 0xFFF, 0x000]);
        let imu = parse_imu(&report);
        assert_eq!(imu[0], [-9, -8, -7, -6, -5, -4]);
        assert_eq!(imu[2][5], 8);
    }

    #[test]
    fn parse_log() {
        let mut frames = [0, 1, 2, 3, 4].map(|_| Frame {
            timestamp_ms: 0,
            packet_size: 64,
            seq: 0,
            forward_us: 0,
            report: report(),
        });
        for (frame, (time, size, seq, forward)) in frames.iter_mut().zip([
            (1000, 64, 7, 0),
            (1010, 64, 8, 800),
            (1015, 0, 8, 0),
            (1020, 64, 9, 0),
            (1030, 64, 12, 1200),
        ]) {
            frame.timestamp_ms = time;
            frame.packet_size = size;
            frame.seq = seq;
            frame.forward_us = forward;
        }
        // Not an input report
        frames[3].report[0] = 0x81;
        let log = ControllerLog::parse(&file(&frames));
        assert_eq!(log.time_ms, [1000, 1010, 1030]);
        assert_eq!(log.forward_us, [0, 800, 1200]);
        assert_eq!(log.dropped, 2);
        assert_eq!(log.buttons.len(), 3);
        // Three samples per report, spread over the previous interval (the
        // first report takes the median of 10 and 20)
        assert_eq!(log.imu_time_ms[..3], [1000.0 - 10.0, 1000.0 - 5.0, 1000.0]);
        assert_eq!(
            log.imu_time_ms[3..],
            [
                1010.0 - 20.0 / 3.0,
                1010.0 - 10.0 / 3.0,
                1010.0,
                1030.0 - 40.0 / 3.0,
                1030.0 - 20.0 / 3.0,
                1030.0
            ]
        );
        assert_eq!(log.gyro[0], [-6.0 * 0.07, -5.0 * 0.07, -4.0 * 0.07]);
        assert_eq!(log.accel[0], [-9.0 / 4096.0, -8.0 / 4096.0, -7.0 / 4096.0]);
    }

    #[test]
    fn interval_cap_and_wrapping_seq() {
        let frame = |time, seq| Frame {
            timestamp_ms: time,
            packet_size: 64,
            seq,
            forward_us: 0,
            report: report(),
        };
        let log = ControllerLog::parse(&file(&[frame(0, u32::MAX), frame(500, 0), frame(510, 2)]));
        assert_eq!(log.dropped, 1);
        // The first report takes the median (255 ms), capped like the second
        let expected: Vec<f64> = [50.0 / 3.0; 6].into_iter().chain([10.0 / 3.0; 3]).collect();
        assert_eq!(log.imu_dt_ms, expected);
    }

    #[test]
    fn empty_file() {
        let log = ControllerLog::parse(&[]);
        assert!(log.is_empty());
        assert!(log.gyro.is_empty());
        assert_eq!(log.dropped, 0);
        let only_heartbeat = ControllerLog::parse(&[0; FRAME_SIZE]);
        assert!(only_heartbeat.is_empty());
    }
}
