//! Object labels drawn in the Inkspector's labeling mode
//!
//! Boxes around things on screen, per video frame, for training a detector.
//! The format is shared with the vision code, which reads the labels and
//! writes its own boxes (`"by": "model"`) for the user to accept or correct:
//!
//! - `<annotations>/classes.json`: a JSON array of `{"name", "label",
//!   "color"}`, created with [`STARTER_CLASSES`] when missing and edited by
//!   hand afterwards
//! - `<annotations>/<session>/<segment file stem>.objects.jsonl`: one JSON
//!   line per labeled frame, sorted by frame:
//!
//! ```json
//! {"frame": 12, "boxes": [{"class": "chum", "x": 0.41, "y": 0.52, "w": 0.06, "h": 0.1, "by": "user"}]}
//! ```
//!
//! `x`, `y` are the box's top-left corner and `w`, `h` its size, all as
//! fractions (0–1) of the frame's width and height. `id` and `score` are
//! optional; other keys are kept as they are.
//!
//! Files are replaced atomically (a temporary file renamed over them). A
//! frame is saved together with the model boxes the page loaded for it
//! (`base`), so model boxes written since, which the user never saw, are
//! kept.

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Classes written to a new `classes.json`: (name, label, color)
pub const STARTER_CLASSES: [(&str, &str, &str); 16] = [
    ("smallfry", "Smallfry", "#8bd450"),
    ("chum", "Chum", "#4fb3ff"),
    ("cohock", "Cohock", "#2f6fdf"),
    ("steelhead", "Steelhead", "#f5c518"),
    ("flyfish", "Flyfish", "#ff8a3d"),
    ("scrapper", "Scrapper", "#9aa3ad"),
    ("steel_eel", "Steel Eel", "#b86bff"),
    ("stinger", "Stinger", "#ff5c8a"),
    ("maws", "Maws", "#20c7a8"),
    ("drizzler", "Drizzler", "#6a8cff"),
    ("fish_stick", "Fish Stick", "#c9a26b"),
    ("flipper_flopper", "Flipper-Flopper", "#3dd6d0"),
    ("big_shot", "Big Shot", "#e0564a"),
    ("slammin_lid", "Slammin' Lid", "#7f8c3a"),
    ("golden_egg", "Golden Egg", "#ffd23f"),
    ("player", "Player", "#ff6fd8"),
];

/// Name of the class list in the annotations folder
pub const CLASSES_FILE: &str = "classes.json";

/// File name suffix of a segment's labels
pub const OBJECTS_SUFFIX: &str = ".objects.jsonl";

/// A class of object
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ObjectClass {
    /// Name in the labels, such as `steel_eel`
    pub name: String,
    /// Name shown to people, such as `Steel Eel`
    pub label: String,
    /// Box color, such as `#b86bff`
    pub color: String,
}

/// One box on a frame
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ObjectBox {
    /// Class name
    pub class: String,
    /// Left edge, as a fraction of the frame's width
    pub x: f64,
    /// Top edge, as a fraction of the frame's height
    pub y: f64,
    /// Width, as a fraction of the frame's width
    pub w: f64,
    /// Height, as a fraction of the frame's height
    pub h: f64,
    /// Identity of the object across frames, if tracked
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    /// Who drew it: `user` or `model`
    #[serde(default = "user")]
    pub by: String,
    /// The model's confidence
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    /// Any other keys, kept as they are
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn user() -> String {
    "user".to_string()
}

impl ObjectBox {
    fn by_model(&self) -> bool {
        self.by == "model"
    }
}

/// The boxes of one frame: a line of a `.objects.jsonl` file
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FrameObjects {
    /// Frame number in the segment
    pub frame: u64,
    /// Boxes on the frame
    pub boxes: Vec<ObjectBox>,
    /// Any other keys, kept as they are
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// The annotations folder; writes go through one lock
pub struct Annotations {
    dir: PathBuf,
    writing: Mutex<()>,
}

impl Annotations {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            writing: Mutex::default(),
        }
    }

    /// The annotations folder
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The class list, written with [`STARTER_CLASSES`] first if missing
    pub fn classes(&self) -> Result<Vec<ObjectClass>> {
        let path = self.dir.join(CLASSES_FILE);
        if !path.exists() {
            let _writing = self.writing.lock().unwrap();
            if !path.exists() {
                let starter: Vec<ObjectClass> = STARTER_CLASSES
                    .iter()
                    .map(|(name, label, color)| ObjectClass {
                        name: name.to_string(),
                        label: label.to_string(),
                        color: color.to_string(),
                    })
                    .collect();
                write_atomic(&path, &serde_json::to_vec_pretty(&starter)?)?;
            }
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("bad {}", path.display()))
    }

    /// The labels file of a segment of a session
    pub fn path(&self, session: &str, segment: &str) -> Result<PathBuf> {
        for name in [session, segment] {
            ensure!(
                !name.is_empty() && !name.contains(['/', '\\']) && !name.starts_with('.'),
                "bad name {name:?}"
            );
        }
        let stem = Path::new(segment)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(segment);
        Ok(self
            .dir
            .join(session)
            .join(format!("{stem}{OBJECTS_SUFFIX}")))
    }

    /// Every labeled frame of a segment, by frame; none if not labeled yet
    pub fn read(&self, session: &str, segment: &str) -> Result<BTreeMap<u64, FrameObjects>> {
        read_objects(&self.path(session, segment)?)
    }

    /// Replace the boxes of `frame` with `boxes`, keeping model boxes that
    /// are in the file but not in `base` (the model boxes the page loaded):
    /// those arrived later and the user has not seen them. A frame left
    /// without boxes is no longer labeled. Answers with the frame as saved.
    pub fn save_frame(
        &self,
        session: &str,
        segment: &str,
        frame: u64,
        boxes: Vec<ObjectBox>,
        base: &[ObjectBox],
    ) -> Result<FrameObjects> {
        let path = self.path(session, segment)?;
        let _writing = self.writing.lock().unwrap();
        let mut frames = read_objects(&path)?;
        let old = frames.remove(&frame);
        let mut saved = FrameObjects {
            frame,
            boxes,
            extra: Map::new(),
        };
        if let Some(old) = old {
            let unseen = old
                .boxes
                .into_iter()
                .filter(|b| b.by_model() && !base.contains(b) && !saved.boxes.contains(b));
            saved.boxes.extend(unseen.collect::<Vec<_>>());
            saved.extra = old.extra;
        }
        if !saved.boxes.is_empty() {
            frames.insert(frame, saved.clone());
        }
        let mut text = String::new();
        for line in frames.values() {
            text.push_str(&serde_json::to_string(line)?);
            text.push('\n');
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("cannot create {}", dir.display()))?;
        }
        write_atomic(&path, text.as_bytes())?;
        Ok(saved)
    }
}

impl Annotations {
    /// Merge a model's boxes into a segment's labels by the
    /// `gameplay-vision` prelabel rule: frames a person has labeled are
    /// never changed, frames with only model boxes get the new ones, frames
    /// without a line get one. Written under the same lock as the labeling
    /// mode's saves. Answers with the file and what was done.
    pub fn merge_model_boxes(
        &self,
        session: &str,
        segment: &str,
        predicted: Vec<gameplay_vision::labels::FrameObjects>,
    ) -> Result<(PathBuf, gameplay_vision::labels::MergeStats)> {
        use gameplay_vision::labels;
        let path = self.path(session, segment)?;
        let _writing = self.writing.lock().unwrap();
        let (merged, stats) = labels::merge_model_boxes(labels::read_objects(&path)?, predicted);
        let mut text = String::new();
        for line in &merged {
            text.push_str(&serde_json::to_string(line)?);
            text.push('\n');
        }
        write_atomic(&path, text.as_bytes())?;
        Ok((path, stats))
    }
}

/// The lines of a labels file by frame; a missing file has none
pub fn read_objects(path: &Path) -> Result<BTreeMap<u64, FrameObjects>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    let mut frames = BTreeMap::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let objects: FrameObjects = serde_json::from_str(line)
            .with_context(|| format!("{} line {}", path.display(), i + 1))?;
        frames.insert(objects.frame, objects);
    }
    Ok(frames)
}

/// Write `bytes` to a temporary file next to `path`, then rename it over
/// `path`, so readers see the old file or the new one, never half of it
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let name = path.file_name().context("no file name")?.to_string_lossy();
    let temporary = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let result = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.with_context(|| format!("cannot write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh folder under the system's temporary folder
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("procon-objects-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn a_box(class: &str, x: f64, by: &str) -> ObjectBox {
        ObjectBox {
            class: class.to_string(),
            x,
            y: 0.25,
            w: 0.1,
            h: 0.2,
            id: None,
            by: by.to_string(),
            score: (by == "model").then_some(0.9),
            extra: Map::new(),
        }
    }

    #[test]
    fn line_round_trip() {
        let line = r#"{"frame":7,"boxes":[{"class":"chum","x":0.1,"y":0.2,"w":0.3,"h":0.4,"id":3,"by":"model","score":0.5,"track":"a"}],"note":"x"}"#;
        let objects: FrameObjects = serde_json::from_str(line).unwrap();
        assert_eq!(objects.boxes[0].id, Some(Value::from(3)));
        assert_eq!(objects.boxes[0].extra["track"], "a");
        assert_eq!(objects.extra["note"], "x");
        let again: Value = serde_json::from_str(&serde_json::to_string(&objects).unwrap()).unwrap();
        assert_eq!(again, serde_json::from_str::<Value>(line).unwrap());
        // "by" defaults to the user
        let plain: ObjectBox =
            serde_json::from_str(r#"{"class":"maws","x":0,"y":0,"w":1,"h":1}"#).unwrap();
        assert_eq!(plain.by, "user");
    }

    #[test]
    fn save_keeps_unseen_model_boxes() {
        let annotations = Annotations::new(scratch("save"));
        let (s, seg) = ("2026-09-25_11-26-22", "video-01.mkv");
        assert!(annotations.read(s, seg).unwrap().is_empty());

        // The model labels frame 3 while the page shows frame 3 with one box
        let seen = a_box("chum", 0.1, "model");
        annotations
            .save_frame(s, seg, 3, vec![seen.clone()], &[])
            .unwrap();
        let unseen = a_box("maws", 0.5, "model");
        annotations
            .save_frame(s, seg, 3, vec![seen.clone(), unseen.clone()], &[])
            .unwrap();

        // The user, who loaded only `seen`, accepts it as their own
        let mut accepted = seen.clone();
        accepted.by = "user".to_string();
        let saved = annotations
            .save_frame(
                s,
                seg,
                3,
                vec![accepted.clone()],
                core::slice::from_ref(&seen),
            )
            .unwrap();
        assert_eq!(saved.boxes, vec![accepted.clone(), unseen.clone()]);

        annotations
            .save_frame(s, seg, 1, vec![a_box("player", 0.3, "user")], &[])
            .unwrap();
        let path = annotations.path(s, seg).unwrap();
        assert!(path.ends_with("2026-09-25_11-26-22/video-01.objects.jsonl"));
        let text = std::fs::read_to_string(&path).unwrap();
        let frames: Vec<u64> = text
            .lines()
            .map(|l| serde_json::from_str::<FrameObjects>(l).unwrap().frame)
            .collect();
        assert_eq!(frames, [1, 3]);

        // Deleting every box it saw leaves the frame to the unseen one; then
        // deleting that one too unlabels the frame
        annotations
            .save_frame(s, seg, 3, vec![], core::slice::from_ref(&unseen))
            .unwrap();
        assert!(!annotations.read(s, seg).unwrap().contains_key(&3));
        let _ = std::fs::remove_dir_all(annotations.dir());
    }

    #[test]
    fn classes_start_with_the_starter_list() {
        let annotations = Annotations::new(scratch("classes"));
        let classes = annotations.classes().unwrap();
        assert_eq!(classes.len(), STARTER_CLASSES.len());
        assert_eq!(classes[0].name, "smallfry");
        // Edited by hand, it is read as it is
        std::fs::write(
            annotations.dir().join(CLASSES_FILE),
            r##"[{"name":"egg","label":"Egg","color":"#fff"}]"##,
        )
        .unwrap();
        assert_eq!(annotations.classes().unwrap()[0].name, "egg");
        let _ = std::fs::remove_dir_all(annotations.dir());
    }

    #[test]
    fn atomic_write_replaces_whole_files() {
        let dir = scratch("atomic");
        let path = dir.join("a").join("file.json");
        write_atomic(&path, b"first").unwrap();
        write_atomic(&path, b"second").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        // No temporary file is left behind
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["file.json"]);
        // A folder in the way fails without touching it
        std::fs::create_dir_all(dir.join("taken")).unwrap();
        assert!(write_atomic(&dir.join("taken"), b"x").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_stay_inside_the_folder() {
        let annotations = Annotations::new(PathBuf::from("/a"));
        assert!(annotations.path("..", "v.mkv").is_err());
        assert!(annotations.path("s", "../v.mkv").is_err());
        assert!(annotations.path("", "v.mkv").is_err());
        assert_eq!(
            annotations.path("s", "v.mkv").unwrap(),
            Path::new("/a/s/v.objects.jsonl")
        );
    }
}
