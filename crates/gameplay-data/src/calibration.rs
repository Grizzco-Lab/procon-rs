//! Measured video delays per session (`calibration.json`).
//!
//! The file maps session folder names to measurements of `video_delay_ms`
//! (the input-to-video latency) with a confidence. Only `high` and `medium`
//! measurements are applied; the rest are kept for inspection. Other keys
//! (per-signal numbers, method version, ...) are kept as they are.

use alloc::collections::BTreeMap;
use alloc::string::String;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Confidences whose delay is applied
pub const APPLIED_CONFIDENCES: [&str; 2] = ["high", "medium"];

/// One session's measurement
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Calibration {
    /// Measured delay; absent or null when nothing could be measured
    #[serde(default)]
    pub video_delay_ms: Option<f64>,
    /// `high`, `medium`, `low` or `none`
    #[serde(default)]
    pub confidence: Option<String>,
    /// Any other keys, kept as they are
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Calibration {
    /// The delay to apply: the measured one, if its confidence is high or
    /// medium
    pub fn applied_delay_ms(&self) -> Option<f64> {
        let confident = self
            .confidence
            .as_deref()
            .is_some_and(|c| APPLIED_CONFIDENCES.contains(&c));
        self.video_delay_ms.filter(|_| confident)
    }
}

/// Measurements by session folder name
pub type Calibrations = BTreeMap<String, Calibration>;

/// Read a calibration file; a missing file means no measurements
pub fn read_calibrations(path: &Path) -> Result<Calibrations> {
    if !path.is_file() {
        return Ok(Calibrations::new());
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("cannot parse {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_confident_delays_apply() {
        let calibrations: Calibrations = serde_json::from_str(
            r#"{"a": {"video_delay_ms": 409.0, "confidence": "high", "method": 3},
                "b": {"video_delay_ms": 232.0, "confidence": "medium"},
                "c": {"video_delay_ms": 697.0, "confidence": "low"},
                "d": {"video_delay_ms": null, "confidence": "none"}}"#,
        )
        .unwrap();
        let applied = |name: &str| calibrations[name].applied_delay_ms();
        assert_eq!(applied("a"), Some(409.0));
        assert_eq!(applied("b"), Some(232.0));
        assert_eq!(applied("c"), None);
        assert_eq!(applied("d"), None);
        assert_eq!(calibrations["a"].extra["method"], 3);
    }

    #[test]
    fn missing_file() {
        let calibrations = read_calibrations(Path::new("/nonexistent/calibration.json")).unwrap();
        assert!(calibrations.is_empty());
    }
}
