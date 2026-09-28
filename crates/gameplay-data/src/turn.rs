//! The camera turn per frame, the effect that gyro aiming and the right
//! stick share.
//!
//! In Splatoon 3 with motion controls the camera turns sideways with gyro
//! yaw *and* the right stick's x, and up and down with gyro pitch. Any mix
//! of yaw and stick that gives the same sideways turn looks the same on
//! screen, so AgentZero's IDM also predicts the turn itself (`camera_turn`
//! in its labels), and the true turn is what a prediction's aiming compares
//! with fairly. AgentZero defines it (`agentzero.idm.actions.turn`), in
//! pixels per frame at 640 x 360:
//!
//! ```text
//! x = a * yaw + b * (stick_x - STICK_CENTER),  y = c * pitch
//! ```
//!
//! with the gyro's rest bias removed ([`gyro_bias`]). `agentzero-refresh`
//! fits `a`, `b` and `c` per game settings (`agentzero.idm.turn`) and keeps
//! each session's fit under `turn` in AgentZero's [`SESSIONS_FILE`], which
//! [`read_turn_fits`] reads; the IDM trains on the turn those fits give.
//! Nothing here fits anything: the fits are applied as AgentZero wrote them,
//! in `f32` as AgentZero computes them, so the truth is in the prediction's
//! units.

use crate::align::FrameActions;
use crate::labels::{Label, round};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::path::Path;

/// AgentZero's facts about each session by folder name, the turn fits among
/// them (next to its `calibration.json`)
pub const SESSIONS_FILE: &str = "sessions.json";

/// Raw stick x the turn is measured from (AgentZero's `STICK_CENTER`: the
/// recorded Pro Controller rests a little above the nominal 2048)
pub const STICK_CENTER: f32 = 2100.0;

/// Frames turning less than this on every axis, in degrees per frame, count
/// as held still when measuring the gyro's rest bias (`GYRO_STILL_DEG`)
pub const GYRO_STILL_DEG: f32 = 0.3;

/// Still frames needed to measure the rest bias; with fewer it is zero
pub const MIN_STILL_FRAMES: usize = 30;

/// One session's camera turn fit
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
pub struct TurnFit {
    /// Pixels per degree of yaw (gyro z)
    pub a: f32,
    /// Pixels per raw unit of the right stick's x from [`STICK_CENTER`]
    pub b: f32,
    /// Pixels per degree of pitch (gyro y)
    pub c: f32,
}

impl TurnFit {
    /// The camera turn `[x, y]` of one frame, in pixels at 640 x 360, from
    /// its rotation in degrees (x, y, z) with the rest bias removed and its
    /// raw sticks (lx, ly, rx, ry)
    pub fn turn(&self, gyro: [f32; 3], sticks: [f32; 4]) -> [f32; 2] {
        [
            self.a * gyro[2] + self.b * (sticks[2] - STICK_CENTER),
            self.c * gyro[1],
        ]
    }
}

/// Each session's turn fit in a `sessions.json`, by session folder name;
/// sessions without one (or whose fit failed, `a` null) are left out, and a
/// missing file means none
pub fn read_turn_fits(path: &Path) -> Result<BTreeMap<String, TurnFit>> {
    if !path.is_file() {
        return Ok(BTreeMap::new());
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let entries: BTreeMap<String, Value> =
        serde_json::from_str(&text).with_context(|| format!("cannot parse {}", path.display()))?;
    Ok(entries
        .into_iter()
        .filter_map(|(name, entry)| Some((name, TurnFit::deserialize(entry.get("turn")?).ok()?)))
        .collect())
}

/// The controller's gyro reading at rest over a segment, in degrees per
/// frame (x, y, z), as AgentZero measures it (`gyro_bias`): the median over
/// the frames with reports that turn less than [`GYRO_STILL_DEG`] on every
/// axis; zeros with fewer than [`MIN_STILL_FRAMES`] of them
pub fn gyro_bias(actions: &FrameActions) -> [f32; 3] {
    let still: Vec<[f32; 3]> = actions
        .gyro
        .iter()
        .zip(&actions.mask)
        .filter(|(gyro, reported)| **reported && gyro.iter().all(|v| v.abs() < GYRO_STILL_DEG))
        .map(|(gyro, _)| *gyro)
        .collect();
    if still.len() < MIN_STILL_FRAMES {
        return [0.0; 3];
    }
    core::array::from_fn(|axis| median(still.iter().map(|gyro| gyro[axis]).collect()))
}

/// The median; the mean of the middle two for an even count, as numpy's
fn median(mut values: Vec<f32>) -> f32 {
    values.sort_by(f32::total_cmp);
    let half = values.len() / 2;
    if values.len() % 2 == 1 {
        values[half]
    } else {
        (values[half - 1] + values[half]) / 2.0
    }
}

/// Add the true camera turn to labels of `actions` (see [`frame_labels`]):
/// `camera_turn: [x, y]`, rounded to 0.01 as the IDM writes its own, on
/// frames with reports; `bias` is [`gyro_bias`] of the whole segment
///
/// [`frame_labels`]: crate::labels::frame_labels
pub fn add_camera_turn(
    labels: &mut [Label],
    actions: &FrameActions,
    fit: &TurnFit,
    bias: [f32; 3],
) {
    for label in labels {
        let n = label.frame as usize;
        if !actions.mask.get(n).is_some_and(|reported| *reported) {
            continue;
        }
        let gyro = core::array::from_fn(|axis| actions.gyro[n][axis] - bias[axis]);
        let turn = fit.turn(gyro, actions.sticks[n]).map(|v| round(v, 2));
        label
            .extra
            .insert("camera_turn".into(), serde_json::json!(turn));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::labels::frame_labels;

    const FIT: TurnFit = TurnFit {
        a: 7.2212,
        b: -0.0102907,
        c: -4.2668,
    };

    /// Frames at rest reading `rest`, then one turning frame and one
    /// without reports
    fn actions(still: usize, rest: [f32; 3]) -> FrameActions {
        let count = still + 2;
        let mut gyro = alloc::vec![rest; count];
        gyro[still] = [0.1, 1.0, 2.0];
        let mut sticks = alloc::vec![[2048.0, 2048.0, 2100.0, 2048.0]; count];
        sticks[still][2] = 3600.0;
        let mut mask = alloc::vec![true; count];
        mask[still + 1] = false;
        FrameActions {
            time_ms: (0..count).map(|n| n as f64 * 33.3).collect(),
            mask,
            buttons: alloc::vec![0; count],
            buttons_held: alloc::vec![0; count],
            sticks,
            gyro,
            accel: alloc::vec![[0.0; 3]; count],
        }
    }

    #[test]
    fn rest_bias() {
        // Turning frames and frames without reports do not count
        let rest = [0.05, -0.06, 0.01];
        assert_eq!(gyro_bias(&actions(40, rest)), rest);
        // Too few still frames: no bias
        assert_eq!(gyro_bias(&actions(29, rest)), [0.0; 3]);
        assert_eq!(median(alloc::vec![3.0, 1.0, 2.0, 10.0]), 2.5);
        assert_eq!(median(alloc::vec![3.0, 1.0, 2.0]), 2.0);
    }

    #[test]
    fn turn_of_labels() {
        let rest = [0.05, -0.06, 0.01];
        let actions = actions(40, rest);
        let bias = gyro_bias(&actions);
        let mut labels = frame_labels(&actions, 39..42);
        add_camera_turn(&mut labels, &actions, &FIT, bias);
        // At rest, with the stick at its centre: no turn
        assert_eq!(
            labels[0].extra["camera_turn"],
            serde_json::json!([0.0, 0.0])
        );
        // Yaw 1.99 turns 14.37 px, the stick 1500 units -15.44 px; pitch
        // 1.06 turns -4.52 px
        assert_eq!(
            labels[1].extra["camera_turn"],
            serde_json::json!([-1.07, -4.52])
        );
        // No reports, no turn
        assert!(!labels[2].extra.contains_key("camera_turn"));
    }

    #[test]
    fn fits_from_sessions_json() {
        let dir = std::env::temp_dir().join(alloc::format!("turn-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(SESSIONS_FILE);
        assert!(read_turn_fits(&path).unwrap().is_empty());
        std::fs::write(
            &path,
            r#"{
                "2026-09-25_11-26-22": {"practice": true, "turn": {"a": 7.2212,
                    "b": -0.0102907, "c": -4.2668, "source": "settings", "own": {}}},
                "2026-09-25_11-52-07": {"turn": {"a": null, "b": null, "c": null}},
                "2026-09-25_11-55-36": {"practice": false}
            }"#,
        )
        .unwrap();
        let fits = read_turn_fits(&path).unwrap();
        assert_eq!(fits.len(), 1);
        assert_eq!(fits["2026-09-25_11-26-22"], FIT);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
