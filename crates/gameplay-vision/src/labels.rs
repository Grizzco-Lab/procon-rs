//! Object boxes per video frame, shared with the labeling tool.
//!
//! An annotations folder holds `classes.json`, a JSON array of
//! [`ClassInfo`], and one file of JSON lines per segment,
//! `<annotations>/<session>/<segment file stem>.objects.jsonl`. Each line
//! is one labeled frame:
//!
//! ```json
//! {"frame": 120, "boxes": [{"class": "steelhead", "x": 0.41, "y": 0.22, "w": 0.1, "h": 0.18,
//!  "id": 7, "by": "model", "score": 0.83}]}
//! ```
//!
//! `x`, `y` are the top-left corner and `w`, `h` the size, as fractions of
//! the frame. `id` is an optional track id; `by` says whether a person or a
//! model drew the box, and `score` is the model's confidence. Detections and
//! tracks written by this crate use the same format. Keys this crate does
//! not know are kept when a file is read and written back.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Name of the class list in an annotations folder
pub const CLASSES_FILE: &str = "classes.json";

/// Extension of a segment's object file, after the segment's file stem
pub const OBJECTS_EXT: &str = ".objects.jsonl";

/// The class list written when `classes.json` is missing: Salmon Run's
/// Salmonids (bosses, then lesser ones), golden eggs and players.
/// `(name, label, color)`
pub const STARTER_CLASSES: [(&str, &str, &str); 16] = [
    ("smallfry", "Smallfry", "#9ccc65"),
    ("chum", "Chum", "#66bb6a"),
    ("cohock", "Cohock", "#2e7d32"),
    ("steelhead", "Steelhead", "#ef5350"),
    ("flyfish", "Flyfish", "#ab47bc"),
    ("scrapper", "Scrapper", "#8d6e63"),
    ("steel_eel", "Steel Eel", "#78909c"),
    ("stinger", "Stinger", "#ffa726"),
    ("maws", "Maws", "#26a69a"),
    ("drizzler", "Drizzler", "#42a5f5"),
    ("fish_stick", "Fish Stick", "#d4e157"),
    ("flipper_flopper", "Flipper-Flopper", "#29b6f6"),
    ("big_shot", "Big Shot", "#ec407a"),
    ("slammin_lid", "Slammin' Lid", "#7e57c2"),
    ("golden_egg", "Golden Egg", "#ffd600"),
    ("player", "Player", "#ffffff"),
];

/// One class in `classes.json`
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClassInfo {
    /// Identifier used in boxes, such as `steelhead`
    pub name: String,
    /// Display name, such as `Steelhead`
    pub label: String,
    /// Display color, `#rrggbb`
    pub color: String,
    /// Any other keys, kept as they are
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Who drew a box
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// A person, in the labeling tool; also assumed when `by` is missing,
    /// so an unknown box is never replaced
    #[default]
    User,
    /// A model, such as `prelabel`
    Model,
}

/// One object's box
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ObjectBox {
    /// Class name, as in `classes.json`
    pub class: String,
    /// Left edge, 0-1 of the frame width
    pub x: f64,
    /// Top edge, 0-1 of the frame height
    pub y: f64,
    /// Width, 0-1 of the frame width
    pub w: f64,
    /// Height, 0-1 of the frame height
    pub h: f64,
    /// Track id, the same object across frames
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    /// Who drew it
    #[serde(default)]
    pub by: Source,
    /// The model's confidence, 0-1
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    /// Any other keys, kept as they are
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl ObjectBox {
    /// A model's box, coordinates rounded to 0.0001 and the score to 0.001
    pub fn model(class: &str, [x, y, w, h]: [f64; 4], score: f64) -> Self {
        let round = |v: f64, scale: f64| (v * scale).round() / scale;
        Self {
            class: class.to_string(),
            x: round(x, 1e4),
            y: round(y, 1e4),
            w: round(w, 1e4),
            h: round(h, 1e4),
            id: None,
            by: Source::Model,
            score: Some(round(score, 1e3)),
            extra: serde_json::Map::new(),
        }
    }

    /// `[x, y, w, h]`
    pub fn rect(&self) -> [f64; 4] {
        [self.x, self.y, self.w, self.h]
    }
}

/// The boxes of one labeled frame
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FrameObjects {
    /// Frame number in the segment
    pub frame: u64,
    /// The objects; empty means the frame was looked at and holds none
    #[serde(default)]
    pub boxes: Vec<ObjectBox>,
    /// Any other keys, kept as they are
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl FrameObjects {
    /// A frame with `boxes` and no other keys
    pub fn new(frame: u64, boxes: Vec<ObjectBox>) -> Self {
        Self {
            frame,
            boxes,
            extra: serde_json::Map::new(),
        }
    }
}

/// Intersection over union of two `[x, y, w, h]` boxes
pub fn iou(a: [f64; 4], b: [f64; 4]) -> f64 {
    let iw = ((a[0] + a[2]).min(b[0] + b[2]) - a[0].max(b[0])).max(0.0);
    let ih = ((a[1] + a[3]).min(b[1] + b[3]) - a[1].max(b[1])).max(0.0);
    let inter = iw * ih;
    let union = a[2] * a[3] + b[2] * b[3] - inter;
    if union > 0.0 { inter / union } else { 0.0 }
}

/// `<annotations>/<session>/<stem>.objects.jsonl`
pub fn objects_path(annotations: &Path, session: &str, stem: &str) -> PathBuf {
    annotations
        .join(session)
        .join(alloc::format!("{stem}{OBJECTS_EXT}"))
}

/// Read an object file; a missing file has no frames
pub fn read_objects(path: &Path) -> Result<Vec<FrameObjects>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(i, line)| {
            serde_json::from_str(line)
                .with_context(|| format!("{}:{}: bad line", path.display(), i + 1))
        })
        .collect()
}

/// Write an object file, frames in order, one line each. The file is
/// written next to its final name and renamed over it, so a reader never
/// sees half a file.
pub fn write_objects(path: &Path, frames: &[FrameObjects]) -> Result<()> {
    let mut sorted: Vec<&FrameObjects> = frames.iter().collect();
    sorted.sort_by_key(|f| f.frame);
    let mut text = Vec::new();
    for frame in sorted {
        serde_json::to_writer(&mut text, frame)?;
        text.push(b'\n');
    }
    write_atomic(path, &text)
}

/// Write `bytes` to a temporary file beside `path`, then rename it
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let mut file =
        std::fs::File::create(&tmp).with_context(|| format!("cannot write {}", tmp.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&tmp, path).with_context(|| format!("cannot write {}", path.display()))
}

/// Read `<annotations>/classes.json`; `None` if it is missing
pub fn read_classes(annotations: &Path) -> Result<Option<Vec<ClassInfo>>> {
    let path = annotations.join(CLASSES_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .with_context(|| format!("cannot parse {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// The classes of `<annotations>/classes.json`, writing [`STARTER_CLASSES`]
/// there first if it is missing. Returns the classes and whether they were
/// just written.
pub fn read_or_create_classes(annotations: &Path) -> Result<(Vec<ClassInfo>, bool)> {
    if let Some(classes) = read_classes(annotations)? {
        return Ok((classes, false));
    }
    let classes: Vec<ClassInfo> = STARTER_CLASSES
        .iter()
        .map(|&(name, label, color)| ClassInfo {
            name: name.to_string(),
            label: label.to_string(),
            color: color.to_string(),
            extra: serde_json::Map::new(),
        })
        .collect();
    let mut text = serde_json::to_vec_pretty(&classes)?;
    text.push(b'\n');
    write_atomic(&annotations.join(CLASSES_FILE), &text)?;
    Ok((classes, true))
}

/// What [`merge_model_boxes`] did
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MergeStats {
    /// Frames that had no line and got one
    pub added: usize,
    /// Frames whose model boxes were replaced
    pub replaced: usize,
    /// Frames left alone because a person had labeled them
    pub kept: usize,
}

/// Whether a person has looked at a frame: it holds a box that is not a
/// model's, or no box at all (the labeling tool's "nothing here"; model
/// output never writes an empty frame)
pub fn is_reviewed(frame: &FrameObjects) -> bool {
    frame.boxes.is_empty() || frame.boxes.iter().any(|b| b.by != Source::Model)
}

/// Merge a model's boxes into existing labels.
///
/// The rule: a frame a person has labeled ([`is_reviewed`]) is never
/// changed, so neither their boxes nor their deletions are undone. A frame
/// that holds only model boxes gets the new model boxes in their place, and
/// a frame without a line gets one. Predicted frames without boxes add
/// nothing, and existing frames the model did not predict stay as they are.
pub fn merge_model_boxes(
    existing: Vec<FrameObjects>,
    predicted: Vec<FrameObjects>,
) -> (Vec<FrameObjects>, MergeStats) {
    let mut stats = MergeStats::default();
    let mut frames: alloc::collections::BTreeMap<u64, FrameObjects> =
        existing.into_iter().map(|f| (f.frame, f)).collect();
    for mut new in predicted {
        new.boxes.retain(|b| b.by == Source::Model);
        if new.boxes.is_empty() {
            continue;
        }
        match frames.get_mut(&new.frame) {
            None => {
                stats.added += 1;
                frames.insert(new.frame, new);
            }
            Some(old) if is_reviewed(old) => stats.kept += 1,
            Some(old) => {
                stats.replaced += 1;
                old.boxes = new.boxes;
            }
        }
    }
    (frames.into_values().collect(), stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_box(class: &str) -> ObjectBox {
        ObjectBox {
            by: Source::User,
            score: None,
            ..ObjectBox::model(class, [0.1, 0.1, 0.2, 0.2], 1.0)
        }
    }

    #[test]
    fn round_trip() {
        let line = r#"{"frame":12,"boxes":[{"class":"steelhead","x":0.41,"y":0.22,"w":0.1,"h":0.18,"id":7,"by":"model","score":0.83},{"class":"chum","x":0.0,"y":0.5,"w":0.05,"h":0.05,"by":"user","note":"half hidden"}],"reviewer":"joy"}"#;
        let frame: FrameObjects = serde_json::from_str(line).unwrap();
        assert_eq!(frame.boxes[0].id, Some(7));
        assert_eq!(frame.boxes[0].by, Source::Model);
        assert_eq!(frame.boxes[1].score, None);
        assert_eq!(frame.boxes[1].extra["note"], "half hidden");
        assert_eq!(frame.extra["reviewer"], "joy");
        assert_eq!(serde_json::to_string(&frame).unwrap(), line);

        let dir = std::env::temp_dir().join(format!("gv-labels-{}", std::process::id()));
        let path = objects_path(&dir, "s", "video-01");
        let frames = vec![FrameObjects::new(30, vec![user_box("chum")]), frame];
        write_objects(&path, &frames).unwrap();
        let back = read_objects(&path).unwrap();
        assert_eq!(back.iter().map(|f| f.frame).collect::<Vec<_>>(), [12, 30]);
        assert_eq!(back[0], frames[1]);
        assert_eq!(back[1], frames[0]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_by_is_user() {
        let b: ObjectBox =
            serde_json::from_str(r#"{"class":"maws","x":0,"y":0,"w":1,"h":1}"#).unwrap();
        assert_eq!(b.by, Source::User);
    }

    #[test]
    fn missing_file_is_empty() {
        assert!(
            read_objects(Path::new("/nonexistent/x.objects.jsonl"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn classes() {
        let dir = std::env::temp_dir().join(format!("gv-classes-{}", std::process::id()));
        assert_eq!(read_classes(&dir).unwrap(), None);
        let (classes, created) = read_or_create_classes(&dir).unwrap();
        assert!(created);
        assert_eq!(classes.len(), STARTER_CLASSES.len());
        let (again, created) = read_or_create_classes(&dir).unwrap();
        assert!(!created);
        assert_eq!(again, classes);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn merge_rule() {
        let model = |class| ObjectBox::model(class, [0.5, 0.5, 0.1, 0.1], 0.9);
        let existing = vec![
            // Reviewed: a user box next to a model box
            FrameObjects::new(1, vec![user_box("chum"), model("maws")]),
            // Reviewed: nothing there
            FrameObjects::new(2, vec![]),
            // Model only: replaced
            FrameObjects::new(3, vec![model("maws")]),
            // Not predicted again: kept
            FrameObjects::new(9, vec![model("stinger")]),
        ];
        let predicted = vec![
            FrameObjects::new(1, vec![model("steelhead")]),
            FrameObjects::new(2, vec![model("steelhead")]),
            FrameObjects::new(3, vec![model("steelhead")]),
            FrameObjects::new(4, vec![model("steelhead")]),
            FrameObjects::new(5, vec![]),
        ];
        let (merged, stats) = merge_model_boxes(existing.clone(), predicted);
        assert_eq!(
            stats,
            MergeStats {
                added: 1,
                replaced: 1,
                kept: 2
            }
        );
        assert_eq!(merged.len(), 5);
        assert_eq!(merged[0], existing[0]);
        assert_eq!(merged[1], existing[1]);
        assert_eq!(merged[2].boxes[0].class, "steelhead");
        assert_eq!(merged[3].frame, 4);
        assert_eq!(merged[4], existing[3]);
    }

    #[test]
    fn iou_values() {
        assert_eq!(iou([0.0, 0.0, 1.0, 1.0], [0.0, 0.0, 1.0, 1.0]), 1.0);
        assert_eq!(iou([0.0, 0.0, 1.0, 1.0], [2.0, 2.0, 1.0, 1.0]), 0.0);
        assert!((iou([0.0, 0.0, 2.0, 1.0], [1.0, 0.0, 2.0, 1.0]) - 1.0 / 3.0).abs() < 1e-12);
    }
}
