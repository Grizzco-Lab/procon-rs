//! Per-frame controller labels as JSON lines.
//!
//! The ground truth exported from a session and a model's predictions share
//! this format, so the two compare line by line. Each line is one video
//! frame of a segment:
//!
//! ```json
//! {"frame": 12, "valid": true, "buttons": ["zr", "y"], "left_stick": [2048.0, 2101.5],
//!  "right_stick": [1990.0, 2048.0], "gyro_deg": [0.01, -0.35, 0.2]}
//! ```
//!
//! `buttons` are the buttons pressed during the frame (names of
//! [`BUTTONS`]), sticks are raw 12-bit `[x, y]`, `gyro_deg` is the rotation
//! over the frame in degrees and `valid` is false for frames without
//! controller reports. Sticks are rounded to 0.1 and the gyro to 0.0001.
//! Other keys, such as confidences, are kept; only `frame` is required when
//! reading, so partial predictions load too.

use crate::align::FrameActions;
use crate::controller::BUTTONS;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use anyhow::{Context, Result};
use core::ops::Range;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;

/// The label of one frame
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Label {
    /// Frame number in the segment
    pub frame: u64,
    /// Whether any controller report falls in the frame
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid: Option<bool>,
    /// Buttons pressed during the frame
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buttons: Option<Vec<String>>,
    /// Left stick `[x, y]`, raw 12-bit
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left_stick: Option<[f64; 2]>,
    /// Right stick `[x, y]`, raw 12-bit
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right_stick: Option<[f64; 2]>,
    /// Rotation over the frame in degrees (x, y, z)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gyro_deg: Option<[f64; 3]>,
    /// Any other keys, kept as they are
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Round to `digits` decimals, halves to even, as Python's `round` does
fn round(value: f32, digits: usize) -> f64 {
    alloc::format!("{:.digits$}", f64::from(value))
        .parse()
        .unwrap()
}

/// The labels of `frames` of a segment's actions
pub fn frame_labels(actions: &FrameActions, frames: Range<usize>) -> Vec<Label> {
    frames
        .map(|n| {
            let [lx, ly, rx, ry] = actions.sticks[n].map(|v| round(v, 1));
            Label {
                frame: n as u64,
                valid: Some(actions.mask[n]),
                buttons: Some(
                    BUTTONS
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| actions.buttons[n] >> i & 1 == 1)
                        .map(|(_, (name, _, _))| name.to_string())
                        .collect(),
                ),
                left_stick: Some([lx, ly]),
                right_stick: Some([rx, ry]),
                gyro_deg: Some(actions.gyro[n].map(|v| round(v, 4))),
                extra: serde_json::Map::new(),
            }
        })
        .collect()
}

/// Write labels, one JSON object per line
pub fn write_labels(path: &Path, labels: &[Label]) -> Result<()> {
    let mut text = Vec::new();
    for label in labels {
        serde_json::to_writer(&mut text, label)?;
        text.push(b'\n');
    }
    std::fs::File::create(path)
        .and_then(|mut file| file.write_all(&text))
        .with_context(|| format!("cannot write {}", path.display()))
}

/// Read a labels file; blank lines are skipped
pub fn read_labels(path: &Path) -> Result<Vec<Label>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(i, line)| {
            serde_json::from_str(line).with_context(|| format!("{} line {}", path.display(), i + 1))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actions() -> FrameActions {
        FrameActions {
            time_ms: alloc::vec![0.0, 33.3, 66.7],
            mask: alloc::vec![true, true, false],
            // zr and sr_left in frame 1
            buttons: alloc::vec![0, 1 << 7 | 1 << 18, 0],
            buttons_held: alloc::vec![0; 3],
            sticks: alloc::vec![[2048.0, 2048.25, 100.0, 4000.0]; 3],
            gyro: alloc::vec![[0.0; 3], [0.1, -0.25, 1.5], [0.0; 3]],
            accel: alloc::vec![[0.0; 3]; 3],
        }
    }

    #[test]
    fn labels() {
        let labels = frame_labels(&actions(), 0..3);
        let line = serde_json::to_string(&labels[1]).unwrap();
        assert_eq!(
            line,
            r#"{"frame":1,"valid":true,"buttons":["zr","sr_left"],"left_stick":[2048.0,2048.2],"right_stick":[100.0,4000.0],"gyro_deg":[0.1,-0.25,1.5]}"#
        );
        assert_eq!(labels[2].valid, Some(false));
        assert_eq!(frame_labels(&actions(), 1..2), labels[1..2]);
    }

    #[test]
    fn round_trip_keeps_other_keys() {
        let dir = std::env::temp_dir().join(alloc::format!("labels-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("labels.jsonl");
        let mut labels = frame_labels(&actions(), 0..3);
        labels[0]
            .extra
            .insert("confidence".into(), serde_json::json!(0.9));
        write_labels(&path, &labels).unwrap();
        std::fs::write(
            &path,
            std::fs::read_to_string(&path).unwrap() + "\n{\"frame\": 7, \"buttons\": [\"a\"]}\n",
        )
        .unwrap();
        let read = read_labels(&path).unwrap();
        assert_eq!(read[..3], labels[..]);
        assert_eq!(read[3].frame, 7);
        assert_eq!(read[3].valid, None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
