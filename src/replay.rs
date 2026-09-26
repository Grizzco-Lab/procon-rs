//! Replaying actions to the Switch instead of the physical controller
//!
//! An [`Action`] is one controller state as a JSON line:
//!
//! ```json
//! {"t_ms": 40, "buttons": ["zr", "a"], "left_stick": [2048, 3500], "right_stick": [1200, 2048], "gyro": [0, -300, 12]}
//! ```
//!
//! Every field is optional, and each one given replaces the controller's own
//! value: `buttons` lists the pressed buttons (names as in
//! [`ButtonState`](crate::keystate::ButtonState)), sticks are raw 12-bit
//! `[x, y]` (center ≈ 2048), `gyro` and `accel` are raw IMU readings (0.07 °/s
//! and 1/4096 g per unit) for all three samples of a report. `t_ms` is when
//! the line plays, counted from the first line; the proxy ignores it.
//!
//! With `"mix": true` the line is combined with the controller instead:
//! buttons pressed on either count, for each stick and the gyro whichever moves
//! more wins, and the accelerometer stays the controller's.
//!
//! The proxy listens on its `[replay]` port for one client at a time; the
//! studio's replay panel is one, and a model can be another. While a client is
//! connected, the latest line it sent is applied to every input report on its
//! way to the Switch (and to what is recorded); when it disconnects, the
//! physical controller takes over again. The physical controller stays plugged
//! in either way: it answers the Switch's handshake.

use crate::dump::{FRAME_SIZE, Frame};
use crate::recorder::CONTROLLER_FILE;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;

/// Standard input report: buttons, sticks and three IMU samples
const REPORT_FULL: u8 = 0x30;
/// Subcommand reply: buttons and sticks, no IMU
const REPORT_REPLY: u8 = 0x21;
/// Stick value at rest
const STICK_CENTER: i32 = 2048;
/// Offset of the first of three IMU samples in a full report
const IMU_OFFSET: usize = 13;
/// Bytes per IMU sample: accel x, y, z then gyro x, y, z, each i16 LE
const IMU_SAMPLE: usize = 12;

/// Button names with their byte in the report and bit mask
const BUTTONS: [(&str, usize, u8); 22] = [
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

/// One controller state to play; see the module docs for the format
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    /// When to play it, in milliseconds from the first line
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t_ms: Option<u64>,
    /// Names of the pressed buttons; all others are released
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buttons: Option<Vec<String>>,
    /// Raw 12-bit `[x, y]`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left_stick: Option<[u16; 2]>,
    /// Raw 12-bit `[x, y]`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right_stick: Option<[u16; 2]>,
    /// Raw gyro `[x, y, z]`, 0.07 °/s per unit
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gyro: Option<[i16; 3]>,
    /// Raw accelerometer `[x, y, z]`, 1/4096 g per unit
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accel: Option<[i16; 3]>,
    /// Combine with the controller instead of replacing it
    #[serde(default, skip_serializing_if = "core::ops::Not::not")]
    pub mix: bool,
}

impl Action {
    /// Parse one JSON line, checking button names and stick ranges
    pub fn parse(line: &str) -> Result<Self> {
        let action: Action = serde_json::from_str(line)?;
        for name in action.buttons.iter().flatten() {
            if !BUTTONS.iter().any(|(n, _, _)| n == name) {
                bail!("unknown button {:?}", name);
            }
        }
        for [x, y] in [action.left_stick, action.right_stick]
            .into_iter()
            .flatten()
        {
            if x > 0xFFF || y > 0xFFF {
                bail!("stick values are 12-bit (0-4095), got [{}, {}]", x, y);
            }
        }
        Ok(action)
    }

    /// The state an input report carries, or `None` for other reports
    pub fn from_report(report: &[u8]) -> Option<Self> {
        if report.len() < 12 || !matches!(report[0], REPORT_FULL | REPORT_REPLY) {
            return None;
        }
        let buttons = BUTTONS
            .iter()
            .filter(|(_, byte, mask)| report[*byte] & mask != 0)
            .map(|(name, _, _)| name.to_string())
            .collect();
        let mut action = Action {
            buttons: Some(buttons),
            left_stick: Some(read_stick(&report[6..9])),
            right_stick: Some(read_stick(&report[9..12])),
            ..Default::default()
        };
        if report[0] == REPORT_FULL && report.len() >= IMU_OFFSET + 3 * IMU_SAMPLE {
            // Average the three samples into one reading per axis
            let mean = |axis: usize| {
                let sum: i32 = (0..3)
                    .map(|s| {
                        let at = IMU_OFFSET + s * IMU_SAMPLE + axis * 2;
                        i16::from_le_bytes([report[at], report[at + 1]]) as i32
                    })
                    .sum();
                (sum / 3) as i16
            };
            action.accel = Some([mean(0), mean(1), mean(2)]);
            action.gyro = Some([mean(3), mean(4), mean(5)]);
        }
        Some(action)
    }

    /// Write the given fields into an input report, or combine them with it
    /// when [`mix`](Self::mix) is set; other reports are left alone
    pub fn apply(&self, report: &mut [u8]) {
        if report.len() < 12 || !matches!(report[0], REPORT_FULL | REPORT_REPLY) {
            return;
        }
        if let Some(pressed) = &self.buttons {
            if !self.mix {
                report[3..6].fill(0);
            }
            for (name, byte, mask) in BUTTONS {
                if pressed.iter().any(|p| p == name) {
                    report[byte] |= mask;
                }
            }
        }
        for (stick, at) in [(self.left_stick, 6), (self.right_stick, 9)] {
            let Some(stick) = stick else { continue };
            let reach = |[x, y]: [u16; 2]| {
                let (dx, dy) = (x as i32 - STICK_CENTER, y as i32 - STICK_CENTER);
                dx * dx + dy * dy
            };
            if !self.mix || reach(stick) > reach(read_stick(&report[at..at + 3])) {
                write_stick(&mut report[at..at + 3], stick);
            }
        }
        if report[0] != REPORT_FULL || report.len() < IMU_OFFSET + 3 * IMU_SAMPLE {
            return;
        }
        for sample in 0..3 {
            let at = IMU_OFFSET + sample * IMU_SAMPLE;
            if let Some(accel) = self.accel
                && !self.mix
            {
                write_axes(&mut report[at..at + 6], accel);
            }
            if let Some(gyro) = self.gyro {
                let speed = |axes: [i16; 3]| axes.iter().map(|&v| (v as i32).pow(2)).sum::<i32>();
                let own = read_axes(&report[at + 6..at + 12]);
                if !self.mix || speed(gyro) > speed(own) {
                    write_axes(&mut report[at + 6..at + 12], gyro);
                }
            }
        }
    }
}

/// Read a recorded `controller.bin`, a session folder holding one, or a
/// JSON-lines file of actions (`.jsonl`, every line with `t_ms`)
pub fn load(path: &Path) -> Result<Vec<Action>> {
    let file = if path.is_dir() {
        path.join(CONTROLLER_FILE)
    } else {
        path.to_path_buf()
    };
    let mut actions = Vec::new();
    if file.extension().is_some_and(|ext| ext == "jsonl") {
        let text = std::fs::read_to_string(&file)
            .with_context(|| format!("cannot read {}", file.display()))?;
        let mut last_ms = 0;
        for (number, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let action = Action::parse(line).with_context(|| format!("line {}", number + 1))?;
            let Some(t_ms) = action.t_ms else {
                bail!("line {}: every line needs t_ms", number + 1)
            };
            if t_ms < last_ms {
                bail!(
                    "line {}: t_ms goes back from {} to {}",
                    number + 1,
                    last_ms,
                    t_ms
                );
            }
            last_ms = t_ms;
            actions.push(action);
        }
    } else {
        let bytes =
            std::fs::read(&file).with_context(|| format!("cannot read {}", file.display()))?;
        let mut first_ms = None;
        for chunk in bytes.as_chunks::<FRAME_SIZE>().0 {
            let frame = Frame::parse(chunk);
            let Some(mut action) = Action::from_report(frame.payload()) else {
                continue;
            };
            let timestamp_ms = frame.timestamp_ms;
            let first = *first_ms.get_or_insert(timestamp_ms);
            action.t_ms = Some(timestamp_ms - first);
            actions.push(action);
        }
    }
    if actions.is_empty() {
        bail!("no actions in {}", file.display());
    }
    Ok(actions)
}

/// Read three i16 LE values
fn read_axes(bytes: &[u8]) -> [i16; 3] {
    core::array::from_fn(|i| i16::from_le_bytes([bytes[2 * i], bytes[2 * i + 1]]))
}

/// Write three i16 LE values
fn write_axes(bytes: &mut [u8], axes: [i16; 3]) {
    for (i, value) in axes.iter().enumerate() {
        bytes[2 * i..2 * i + 2].copy_from_slice(&value.to_le_bytes());
    }
}

/// Unpack a 12-bit `[x, y]` pair from three report bytes
fn read_stick(bytes: &[u8]) -> [u16; 2] {
    let x = bytes[0] as u16 | ((bytes[1] as u16 & 0x0F) << 8);
    let y = (bytes[1] as u16 >> 4) | ((bytes[2] as u16) << 4);
    [x, y]
}

/// Pack a 12-bit `[x, y]` pair into three report bytes
fn write_stick(bytes: &mut [u8], [x, y]: [u16; 2]) {
    bytes[0] = x as u8;
    bytes[1] = ((x >> 8) as u8 & 0x0F) | ((y as u8 & 0x0F) << 4);
    bytes[2] = (y >> 4) as u8;
}

/// The action a connected replay client wants applied, shared with the proxy loop
#[derive(Clone, Default)]
pub struct Replay(Arc<Mutex<Option<Action>>>);

impl Replay {
    /// Accept replay clients on `port`, one at a time, in a background thread
    pub fn listen(port: u16) -> Result<Self> {
        let listener = TcpListener::bind(("0.0.0.0", port))
            .with_context(|| format!("failed to listen for replay clients on port {}", port))?;
        log::info!("Waiting for replay clients on port {}", port);
        let replay = Replay::default();
        let shared = replay.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let peer = stream.peer_addr().ok();
                log::info!("Replay client {:?} took over the controller", peer);
                let _ = stream.set_nodelay(true);
                for line in BufReader::new(stream).lines() {
                    let Ok(line) = line else { break };
                    if line.trim().is_empty() {
                        continue;
                    }
                    match Action::parse(&line) {
                        Ok(action) => *shared.0.lock().unwrap() = Some(action),
                        Err(e) => log::warn!("Ignoring replay line {:?}: {}", line, e),
                    }
                }
                *shared.0.lock().unwrap() = None;
                log::info!("Replay client {:?} left; controller is back", peer);
            }
        });
        Ok(replay)
    }

    /// Apply the current action, if any, to an input report
    pub fn apply(&self, report: &mut [u8]) {
        if let Some(action) = self.0.lock().unwrap().as_ref() {
            action.apply(report);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_report() -> [u8; 64] {
        let mut report = [0u8; 64];
        report[0] = REPORT_FULL;
        report[3] = 0x80; // ZR
        write_stick(&mut report[6..9], [100, 4000]);
        write_stick(&mut report[9..12], [2048, 1]);
        for sample in 0..3 {
            let at = IMU_OFFSET + sample * IMU_SAMPLE;
            report[at + 6..at + 8].copy_from_slice(&(-300i16).to_le_bytes());
        }
        report
    }

    #[test]
    fn report_round_trips() {
        let report = full_report();
        let action = Action::from_report(&report).unwrap();
        assert_eq!(action.buttons.as_deref(), Some(&["zr".to_string()][..]));
        assert_eq!(action.left_stick, Some([100, 4000]));
        assert_eq!(action.right_stick, Some([2048, 1]));
        assert_eq!(action.gyro, Some([-300, 0, 0]));

        let mut replayed = [0u8; 64];
        replayed[0] = REPORT_FULL;
        action.apply(&mut replayed);
        assert_eq!(replayed, report);
    }

    #[test]
    fn missing_fields_keep_the_controller() {
        let mut report = full_report();
        Action::parse(r#"{"buttons": ["a", "zl"]}"#)
            .unwrap()
            .apply(&mut report);
        assert_eq!(report[3..6], [0x08, 0x00, 0x80]);
        assert_eq!(read_stick(&report[6..9]), [100, 4000]);
    }

    #[test]
    fn mix_combines_with_the_controller() {
        let mut report = full_report();
        // The controller holds ZR, left stick far out, gyro -300 on x
        Action::parse(
            r#"{"mix": true, "buttons": ["a"], "left_stick": [2100, 2048], "right_stick": [0, 2048], "gyro": [10, 0, 0], "accel": [1, 2, 3]}"#,
        )
        .unwrap()
        .apply(&mut report);
        assert_eq!(report[3], 0x88); // ZR and A
        assert_eq!(read_stick(&report[6..9]), [100, 4000]); // controller moves more
        assert_eq!(read_stick(&report[9..12]), [0, 2048]); // replay moves more
        assert_eq!(read_axes(&report[IMU_OFFSET + 6..]), [-300, 0, 0]);
        assert_eq!(read_axes(&report[IMU_OFFSET..]), [0, 0, 0]); // accel untouched
    }

    #[test]
    fn rejects_bad_lines() {
        assert!(Action::parse(r#"{"buttons": ["turbo"]}"#).is_err());
        assert!(Action::parse(r#"{"left_stick": [5000, 0]}"#).is_err());
        assert!(Action::parse(r#"{"rumble": true}"#).is_err());
    }
}
