//! Video delays per session (`calibration.json`).
//!
//! The file maps session folder names to entries about `video_delay_ms`
//! (the input-to-video latency). AgentZero's calibrator writes them; the
//! studio's Inkspector adds delays set by hand. Each entry has a `source`:
//!
//! - `manual`: set by hand; always applied, and never replaced by the
//!   calibrator (it keeps its own result under `computed`);
//! - `session`: measured from the session itself (aiming and button events);
//!   applied when its `confidence` is `high` or `medium`;
//! - `setup`: the session had no usable estimate of its own, so it falls back
//!   to the delay of its setup era, under `setup`.
//!
//! [`Calibration::applied`] picks the delay in that order. Older entries
//! without a `source` apply their delay when confident. Other keys
//! (per-signal numbers, method version, ...) are kept as they are.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::Path;

/// Confidences whose own estimate is applied
pub const APPLIED_CONFIDENCES: [&str; 2] = ["high", "medium"];

/// One session's entry
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Calibration {
    /// The session's own delay, or the one set by hand; absent or null when
    /// nothing could be measured
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_delay_ms: Option<f64>,
    /// Confidence of the own estimate: `high`, `medium`, `low` or `none`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<String>,
    /// `manual`, `session` or `setup`; absent in older entries
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// 90% interval of the own estimate, in ms
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_ms: Option<[f64; 2]>,
    /// The setup era's delay, for entries that fall back to it
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<SetupDelay>,
    /// Any other keys, kept as they are
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// The delay of a setup era, pooled over its sessions with a delay of their
/// own
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SetupDelay {
    /// Pooled delay
    pub video_delay_ms: f64,
    /// Interval covering the sessions' own uncertainty and how much they differ
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_ms: Option<[f64; 2]>,
    /// Any other keys (era, sessions, ...), kept as they are
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Where an applied delay comes from
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Set by hand
    Manual,
    /// Measured from the session itself
    Session,
    /// The setup era's delay
    Setup,
}

/// The delay to apply to a session, with its source and uncertainty
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct AppliedDelay {
    pub video_delay_ms: f64,
    pub source: Source,
    /// 90% interval in ms; none for delays set by hand
    pub interval_ms: Option<[f64; 2]>,
}

impl Calibration {
    /// The delay to apply: set by hand, else the session's own when
    /// confident, else its setup era's
    pub fn applied(&self) -> Option<AppliedDelay> {
        let source = self.source.as_deref();
        if source == Some("manual") {
            return self.video_delay_ms.map(|delay| AppliedDelay {
                video_delay_ms: delay,
                source: Source::Manual,
                interval_ms: None,
            });
        }
        let confident = self
            .confidence
            .as_deref()
            .is_some_and(|c| APPLIED_CONFIDENCES.contains(&c));
        if confident
            && source != Some("setup")
            && let Some(delay) = self.video_delay_ms
        {
            return Some(AppliedDelay {
                video_delay_ms: delay,
                source: Source::Session,
                interval_ms: self.interval_ms,
            });
        }
        self.setup.as_ref().map(|setup| AppliedDelay {
            video_delay_ms: setup.video_delay_ms,
            source: Source::Setup,
            interval_ms: setup.interval_ms,
        })
    }

    /// The delay to apply, in ms (see [`Calibration::applied`])
    pub fn applied_delay_ms(&self) -> Option<f64> {
        self.applied().map(|applied| applied.video_delay_ms)
    }
}

/// Entries by session folder name
pub type Calibrations = BTreeMap<String, Calibration>;

/// Read a calibration file; a missing file means no entries
pub fn read_calibrations(path: &Path) -> Result<Calibrations> {
    if !path.is_file() {
        return Ok(Calibrations::new());
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("cannot parse {}", path.display()))
}

/// Set a session's delay by hand. The entry before (unless it was set by
/// hand too) is kept under `computed`, so removing the delay restores it.
pub fn set_manual_delay(path: &Path, session: &str, delay_ms: f64) -> Result<()> {
    anyhow::ensure!(delay_ms.is_finite(), "the delay must be a number");
    edit(path, |entries| {
        let before = entries.remove(session).unwrap_or(Value::Object(Map::new()));
        let computed = if before["source"] == "manual" {
            before.get("computed").cloned()
        } else {
            Some(before.clone())
        };
        let mut entry = Map::new();
        entry.insert("source".to_string(), "manual".into());
        entry.insert("video_delay_ms".to_string(), delay_ms.into());
        if let Some(era) = before.get("era") {
            entry.insert("era".to_string(), era.clone());
        }
        if let Some(computed) = computed.filter(|c| c.as_object().is_some_and(|c| !c.is_empty())) {
            entry.insert("computed".to_string(), computed);
        }
        entries.insert(session.to_string(), Value::Object(entry));
    })
}

/// Remove a delay set by hand, back to the computed entry (if any); false
/// when the session had none set by hand
pub fn remove_manual_delay(path: &Path, session: &str) -> Result<bool> {
    let mut removed = false;
    edit(path, |entries| {
        if entries
            .get(session)
            .is_some_and(|e| e["source"] == "manual")
        {
            removed = true;
            let before = entries.remove(session);
            if let Some(computed) = before.and_then(|mut e| e.get_mut("computed").map(Value::take))
            {
                entries.insert(session.to_string(), computed);
            }
        }
    })?;
    Ok(removed)
}

/// Change the file's entries and write it back atomically (a temporary file
/// renamed over it), in the calibrator's style: two-space indents, keys
/// sorted, a final newline
fn edit(path: &Path, change: impl FnOnce(&mut Map<String, Value>)) -> Result<()> {
    let mut entries: Map<String, Value> = if path.is_file() {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("cannot parse {}", path.display()))?
    } else {
        Map::new()
    };
    change(&mut entries);
    let text = serde_json::to_string_pretty(&entries)? + "\n";
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, text)
        .with_context(|| format!("cannot write {}", temporary.display()))?;
    std::fs::rename(&temporary, path).with_context(|| format!("cannot replace {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries() -> Calibrations {
        serde_json::from_str(
            r#"{"old": {"video_delay_ms": 409.0, "confidence": "high", "method": 3},
                "low": {"video_delay_ms": 697.0, "confidence": "low"},
                "session": {"video_delay_ms": 205.0, "confidence": "high", "source": "session",
                            "interval_ms": [184.0, 226.0]},
                "setup": {"video_delay_ms": 210.0, "confidence": "low", "source": "setup",
                          "setup": {"video_delay_ms": 284.0, "interval_ms": [145.0, 423.0],
                                    "era": "arrival stamps"}},
                "unknown": {"video_delay_ms": null, "confidence": "none"},
                "manual": {"video_delay_ms": 212.0, "source": "manual",
                           "computed": {"video_delay_ms": 999.0, "confidence": "high"}}}"#,
        )
        .unwrap()
    }

    #[test]
    fn applied_in_order() {
        let calibrations = entries();
        let applied = |name: &str| calibrations[name].applied();
        assert_eq!(applied("old").unwrap().source, Source::Session);
        assert_eq!(applied("low"), None);
        let session = applied("session").unwrap();
        assert_eq!(session.video_delay_ms, 205.0);
        assert_eq!(session.interval_ms, Some([184.0, 226.0]));
        let setup = applied("setup").unwrap();
        assert_eq!((setup.video_delay_ms, setup.source), (284.0, Source::Setup));
        assert_eq!(applied("unknown"), None);
        let manual = applied("manual").unwrap();
        assert_eq!(
            (manual.video_delay_ms, manual.source),
            (212.0, Source::Manual)
        );
        assert_eq!(calibrations["old"].extra["method"], 3);
    }

    #[test]
    fn missing_file() {
        let calibrations = read_calibrations(Path::new("/nonexistent/calibration.json")).unwrap();
        assert!(calibrations.is_empty());
    }

    #[test]
    fn set_and_remove_by_hand() {
        let dir = std::env::temp_dir().join(alloc::format!("calibration-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("calibration.json");
        std::fs::write(
            &path,
            "{\n  \"a\": {\n    \"confidence\": \"low\",\n    \"era\": \"arrival stamps\",\n    \"video_delay_ms\": 697.0\n  }\n}\n",
        )
        .unwrap();
        set_manual_delay(&path, "a", 210.0).unwrap();
        set_manual_delay(&path, "b", 150.0).unwrap();
        // A second change by hand keeps the computed entry
        set_manual_delay(&path, "a", 212.0).unwrap();
        let calibrations = read_calibrations(&path).unwrap();
        assert_eq!(calibrations["a"].applied().unwrap().video_delay_ms, 212.0);
        assert_eq!(calibrations["a"].extra["computed"]["video_delay_ms"], 697.0);
        assert_eq!(calibrations["a"].extra["era"], "arrival stamps");
        assert_eq!(calibrations["b"].applied().unwrap().source, Source::Manual);

        assert!(remove_manual_delay(&path, "a").unwrap());
        assert!(remove_manual_delay(&path, "b").unwrap());
        assert!(!remove_manual_delay(&path, "a").unwrap());
        // Back to the file as it was, in the same style
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{\n  \"a\": {\n    \"confidence\": \"low\",\n    \"era\": \"arrival stamps\",\n    \"video_delay_ms\": 697.0\n  }\n}\n"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
