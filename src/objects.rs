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

/// Classes written to a new `classes.json`: (name, label, color). The same
/// list, in the same order, as `gameplay_vision::labels::STARTER_CLASSES`
/// (which says where it comes from). Only ever appended to: labels name
/// classes, and the keys of the labeling mode follow the order.
pub const STARTER_CLASSES: [(&str, &str, &str); 28] = [
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
    ("dead_player", "Dead Player (rescue)", "#a0a0ff"),
    ("basket", "Egg Basket", "#ffffff"),
    ("cohozuna", "Cohozuna", "#ff3b3b"),
    ("horrorboros", "Horrorboros", "#c0392b"),
    ("megalodontia", "Megalodontia", "#8e44ad"),
    ("snatcher", "Snatcher", "#d35fb7"),
    ("goldie", "Goldie", "#e6a500"),
    ("griller", "Griller", "#2e8b57"),
    ("mudmouth", "Mudmouth", "#8b5a2b"),
    ("gold_mudmouth", "Gold Mudmouth", "#eedd82"),
    ("chinook", "Chinook", "#87ceeb"),
    ("mothership", "Mothership", "#ff00ff"),
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

    /// The track id, if it is a whole number
    pub fn track_id(&self) -> Option<u64> {
        self.id.as_ref().and_then(Value::as_u64)
    }

    /// Whether two boxes are the same drawing, ids aside
    fn same_place(&self, other: &ObjectBox) -> bool {
        (&self.class, self.x, self.y, self.w, self.h)
            == (&other.class, other.x, other.y, other.w, other.h)
    }
}

/// How many frames after `start` Follow covers, going forward (towards
/// frame `total - 1`) or backward (towards 0): at most `count`, up to the
/// end of the segment. Labeled frames do not stop it ([`apply_followed`]
/// decides per object).
pub fn follow_span(start: u64, count: u64, forward: bool, total: u64) -> u64 {
    let room = if forward {
        total.saturating_sub(start + 1)
    } else {
        start
    };
    count.min(room)
}

/// Overlap above which a person's box of the same class stands for the
/// followed object on a frame ([`apply_followed`])
pub const COVERED_IOU: f64 = 0.3;

/// Intersection over union of two boxes
pub fn iou(a: &ObjectBox, b: &ObjectBox) -> f64 {
    let w = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
    let h = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
    if w <= 0.0 || h <= 0.0 {
        return 0.0;
    }
    let inter = w * h;
    let union = a.w * a.h + b.w * b.h - inter;
    if union > 0.0 { inter / union } else { 0.0 }
}

/// What [`apply_followed`] did
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct FollowWrite {
    /// Frames written
    pub written: usize,
    /// Followed boxes left out because a person's box already stands for
    /// the object there
    pub covered: usize,
}

/// Write Follow's boxes into a segment's frames: `followed` holds, per
/// frame, the boxes of the objects still followed there, whose ids are
/// among `ids`. Per object: on every frame the model boxes of those ids (an
/// earlier Follow, or an object lost since) give way to the new boxes, and
/// a new box is added unless a person's box of the same class overlaps it
/// by more than [`COVERED_IOU`] (that box is the object's already). A line
/// a person left empty ("looked at, nothing here") is not changed. Boxes of
/// people are never changed; other model boxes stay. A frame left without
/// boxes loses its line, since an empty line would mean "looked at,
/// nothing here".
pub fn apply_followed(
    frames: &mut BTreeMap<u64, FrameObjects>,
    followed: &[(u64, Vec<ObjectBox>)],
    ids: &[u64],
) -> FollowWrite {
    let mut done = FollowWrite::default();
    for (frame, boxes) in followed {
        if frames.get(frame).is_some_and(|f| f.boxes.is_empty()) {
            done.covered += boxes.len();
            continue;
        }
        let line = frames.entry(*frame).or_insert_with(|| FrameObjects {
            frame: *frame,
            boxes: Vec::new(),
            extra: Map::new(),
        });
        line.boxes
            .retain(|b| !(b.by_model() && b.track_id().is_some_and(|id| ids.contains(&id))));
        for new in boxes {
            let covered = line
                .boxes
                .iter()
                .any(|b| !b.by_model() && b.class == new.class && iou(b, new) > COVERED_IOU);
            if covered {
                done.covered += 1;
            } else {
                line.boxes.push(new.clone());
            }
        }
        if line.boxes.is_empty() {
            frames.remove(frame);
        }
        done.written += 1;
    }
    done
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
        let mut saved = self.save_frames(session, segment, vec![(frame, boxes, base.to_vec())])?;
        Ok(saved.remove(0))
    }

    /// Save several frames as [`Annotations::save_frame`] saves one, each
    /// `(frame, boxes, base)`, in one write of the file (Accept up to here).
    /// Answers with the frames as saved, in the order given.
    pub fn save_frames(
        &self,
        session: &str,
        segment: &str,
        changes: Vec<(u64, Vec<ObjectBox>, Vec<ObjectBox>)>,
    ) -> Result<Vec<FrameObjects>> {
        let path = self.path(session, segment)?;
        let _writing = self.writing.lock().unwrap();
        let mut frames = read_objects(&path)?;
        let saved = changes
            .into_iter()
            .map(|(frame, boxes, base)| replace_frame(&mut frames, frame, boxes, &base))
            .collect();
        write_objects(&path, &frames)?;
        Ok(saved)
    }

    /// Track ids of `boxes`, drawn on `frame`, before Follow: a box's own
    /// whole-number id, else a new one above every id in the segment's
    /// file. Boxes of the frame that match one of `boxes` get its id in the
    /// file too, so a later Follow from any frame continues the same
    /// tracks. Answers with the ids in the order of `boxes`.
    pub fn follow_ids(
        &self,
        session: &str,
        segment: &str,
        frame: u64,
        boxes: &[ObjectBox],
    ) -> Result<Vec<u64>> {
        let path = self.path(session, segment)?;
        let _writing = self.writing.lock().unwrap();
        let mut frames = read_objects(&path)?;
        let mut next = frames
            .values()
            .flat_map(|f| &f.boxes)
            .chain(boxes)
            .filter_map(ObjectBox::track_id)
            .max()
            .map_or(1, |id| id + 1);
        let ids: Vec<u64> = boxes
            .iter()
            .map(|b| {
                b.track_id().unwrap_or_else(|| {
                    next += 1;
                    next - 1
                })
            })
            .collect();
        let mut changed = false;
        if let Some(line) = frames.get_mut(&frame) {
            for (b, &id) in boxes.iter().zip(&ids) {
                if let Some(saved) = line.boxes.iter_mut().find(|s| s.same_place(b))
                    && saved.track_id() != Some(id)
                {
                    saved.id = Some(Value::from(id));
                    changed = true;
                }
            }
        }
        if changed {
            write_objects(&path, &frames)?;
        }
        Ok(ids)
    }

    /// Write Follow's boxes by the rule of [`apply_followed`], under the
    /// lock of the labeling mode's saves
    pub fn write_followed(
        &self,
        session: &str,
        segment: &str,
        followed: &[(u64, Vec<ObjectBox>)],
        ids: &[u64],
    ) -> Result<FollowWrite> {
        let path = self.path(session, segment)?;
        let _writing = self.writing.lock().unwrap();
        let mut frames = read_objects(&path)?;
        let done = apply_followed(&mut frames, followed, ids);
        write_objects(&path, &frames)?;
        Ok(done)
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

/// Replace the boxes of `frame` in `frames` by the rule of
/// [`Annotations::save_frame`]; answers with the frame as saved
fn replace_frame(
    frames: &mut BTreeMap<u64, FrameObjects>,
    frame: u64,
    boxes: Vec<ObjectBox>,
    base: &[ObjectBox],
) -> FrameObjects {
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
    saved
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

/// Write a labels file, one line per frame in frame order
fn write_objects(path: &Path, frames: &BTreeMap<u64, FrameObjects>) -> Result<()> {
    let mut text = String::new();
    for line in frames.values() {
        text.push_str(&serde_json::to_string(line)?);
        text.push('\n');
    }
    write_atomic(path, text.as_bytes())
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
    fn save_frames_accepts_several_frames_in_one_write() {
        let annotations = Annotations::new(scratch("save-frames"));
        let (s, seg) = ("2026-09-25_11-26-22", "video-01.mkv");
        let seen = a_box("chum", 0.1, "model");
        let unseen = a_box("maws", 0.5, "model");
        annotations
            .save_frame(s, seg, 4, vec![seen.clone(), unseen.clone()], &[])
            .unwrap();
        annotations
            .save_frame(s, seg, 5, vec![seen.clone()], &[])
            .unwrap();

        // The page saw only `seen` on frames 4 and 5 and accepts both
        let mut accepted = seen.clone();
        accepted.by = "user".to_string();
        accepted.score = None;
        let saved = annotations
            .save_frames(
                s,
                seg,
                vec![
                    (5, vec![accepted.clone()], vec![seen.clone()]),
                    (4, vec![accepted.clone()], vec![seen.clone()]),
                ],
            )
            .unwrap();
        assert_eq!(saved[0].frame, 5);
        assert_eq!(saved[0].boxes, vec![accepted.clone()]);
        assert_eq!(saved[1].boxes, vec![accepted.clone(), unseen.clone()]);
        let frames = annotations.read(s, seg).unwrap();
        assert_eq!(frames[&4].boxes, vec![accepted.clone(), unseen]);
        assert_eq!(frames[&5].boxes, vec![accepted]);
        assert!(frames.values().all(|f| f.boxes[0].by == "user"));
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
    fn starter_classes_match_the_vision_crate() {
        assert_eq!(STARTER_CLASSES, gameplay_vision::labels::STARTER_CLASSES);
    }

    /// A model box of track `id`
    fn followed(id: u64, x: f64) -> ObjectBox {
        ObjectBox {
            id: Some(Value::from(id)),
            ..a_box("chum", x, "model")
        }
    }

    fn line(frame: u64, boxes: Vec<ObjectBox>) -> (u64, FrameObjects) {
        (
            frame,
            FrameObjects {
                frame,
                boxes,
                extra: Map::new(),
            },
        )
    }

    #[test]
    fn follow_covers_count_frames_up_to_the_ends() {
        assert_eq!(follow_span(10, 60, true, 100), 60);
        assert_eq!(follow_span(10, 60, false, 100), 10);
        assert_eq!(follow_span(95, 60, true, 100), 4);
        assert_eq!(follow_span(99, 60, true, 100), 0);
        assert_eq!(follow_span(0, 60, false, 100), 0);
    }

    #[test]
    fn overlap() {
        let a = a_box("chum", 0.1, "user");
        assert!((iou(&a, &a) - 1.0).abs() < 1e-9);
        // Half as wide apart: a third of the union
        assert!((iou(&a, &a_box("chum", 0.15, "user")) - 1.0 / 3.0).abs() < 1e-9);
        assert_eq!(iou(&a, &a_box("chum", 0.5, "user")), 0.0);
    }

    #[test]
    fn followed_boxes_are_placed_per_object() {
        let other = a_box("maws", 0.6, "model");
        let others_user = a_box("maws", 0.6, "user");
        let same_user = a_box("chum", 0.21, "user");
        let far_user = a_box("chum", 0.8, "user");
        let mut frames: BTreeMap<u64, FrameObjects> = [
            // Model only: the old box of track 1 gives way, track 7 stays
            line(11, vec![followed(1, 0.1), followed(7, 0.5), other.clone()]),
            // A person labeled other objects: the followed box joins them
            line(12, vec![others_user.clone()]),
            // Looked at, nothing there
            line(13, vec![]),
            // A person's chum box over the followed one stands for it; the
            // old box of track 1 goes all the same
            line(14, vec![same_user.clone(), followed(1, 0.1)]),
            // A chum box elsewhere is another chum
            line(16, vec![far_user.clone()]),
            // Track 1 was lost here: its old box goes, the line with it
            line(17, vec![followed(1, 0.1)]),
        ]
        .into_iter()
        .collect();
        let result = [
            (11, vec![followed(1, 0.2)]),
            (12, vec![followed(1, 0.2)]),
            (13, vec![followed(1, 0.2)]),
            (14, vec![followed(1, 0.2)]),
            (15, vec![followed(1, 0.2)]),
            (16, vec![followed(1, 0.2)]),
            (17, vec![]),
        ];
        let done = apply_followed(&mut frames, &result, &[1]);
        assert_eq!(
            done,
            FollowWrite {
                written: 6,
                covered: 2
            }
        );
        assert_eq!(
            frames[&11].boxes,
            [followed(7, 0.5), other, followed(1, 0.2)]
        );
        assert_eq!(frames[&12].boxes, [others_user, followed(1, 0.2)]);
        assert!(frames[&13].boxes.is_empty());
        assert_eq!(frames[&14].boxes, [same_user]);
        assert_eq!(frames[&15].boxes, [followed(1, 0.2)]);
        assert_eq!(frames[&16].boxes, [far_user, followed(1, 0.2)]);
        assert!(!frames.contains_key(&17));
    }

    #[test]
    fn follow_ids_are_kept_and_written_on_the_frame() {
        let annotations = Annotations::new(scratch("ids"));
        let (s, seg) = ("s", "video-01.mkv");
        let tracked = followed(5, 0.1);
        let fresh = a_box("chum", 0.3, "user");
        annotations
            .save_frame(s, seg, 3, vec![tracked.clone(), fresh.clone()], &[])
            .unwrap();
        // A box the file does not have (not saved yet) still gets an id
        let unsaved = a_box("maws", 0.7, "user");
        let ids = annotations
            .follow_ids(s, seg, 3, &[tracked.clone(), fresh.clone(), unsaved])
            .unwrap();
        assert_eq!(ids, [5, 6, 7]);
        let frame = &annotations.read(s, seg).unwrap()[&3];
        assert_eq!(frame.boxes[0], tracked);
        assert_eq!(frame.boxes[1].track_id(), Some(6));
        assert_eq!(frame.boxes[1].by, "user");
        // Asking again changes nothing
        let mut again = fresh.clone();
        again.id = Some(Value::from(6));
        assert_eq!(annotations.follow_ids(s, seg, 3, &[again]).unwrap(), [6]);
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
