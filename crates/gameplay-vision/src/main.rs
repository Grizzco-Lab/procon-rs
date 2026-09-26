//! `gameplay-vision`: detect and track objects in recorded sessions, and
//! prelabel them for the labeling tool.
//!
//! ```text
//! gameplay-vision detect   <session> [--start N --step K --count C] [--out detections]
//! gameplay-vision track    <objects.jsonl> [--output tracks.jsonl]
//! gameplay-vision prelabel <session> [--annotations DIR] [--input objects.jsonl] [--track]
//! gameplay-vision render   <session> <objects.jsonl> --out DIR [--frames 1,2,3]
//! ```
//!
//! `detect` and `prelabel` print each frame's timings (decode, preprocess,
//! network, postprocess) and a summary, to compare CPU and GPU runs.

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use gameplay_vision::detect::{self, Detector, Timing, Weights};
use gameplay_vision::frames::{FrameRange, FrameReader, Segment};
use gameplay_vision::labels::{self, FrameObjects, ObjectBox};
use gameplay_vision::render;
use gameplay_vision::track::{self, TrackerConfig};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(about = "Object detection and tracking on gameplay video")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Detect objects in a segment's frames and write them as JSON lines
    Detect {
        #[command(flatten)]
        source: Source,
        #[command(flatten)]
        model: Model,
        /// Folder for `<session>/<segment>.objects.jsonl`
        #[arg(long, default_value = "detections")]
        out: PathBuf,
        /// Also save each frame with its boxes as PNG in this folder
        #[arg(long)]
        png: Option<PathBuf>,
    },
    /// Track the boxes of an object file over frames
    Track {
        /// Object file from `detect`
        input: PathBuf,
        /// Output file; default: the input with `.tracks.jsonl`
        #[arg(long)]
        output: Option<PathBuf>,
        #[command(flatten)]
        tracker: Tracking,
    },
    /// Write model boxes into the shared labels, never touching frames a
    /// person has labeled
    Prelabel {
        #[command(flatten)]
        source: Source,
        #[command(flatten)]
        model: Model,
        /// Annotations folder; default: `Annotations` next to the dataset
        /// folder that holds the session
        #[arg(long)]
        annotations: Option<PathBuf>,
        /// Take boxes from this object file (from `detect` or `track`)
        /// instead of running the model
        #[arg(long)]
        input: Option<PathBuf>,
        /// Track the boxes first, so they carry ids
        #[arg(long)]
        track: bool,
        #[command(flatten)]
        tracker: Tracking,
        /// Rename a model class, `from=to` (such as `person=player`);
        /// repeatable. Classes not in `classes.json` are dropped.
        #[arg(long = "map", value_parser = parse_map)]
        maps: Vec<(String, String)>,
    },
    /// Save frames with the boxes of an object file drawn, as PNG
    Render {
        /// Session folder
        session: PathBuf,
        /// Object file
        objects: PathBuf,
        /// Segment number, from 1
        #[arg(long, default_value_t = 1)]
        segment: usize,
        /// Output folder
        #[arg(long)]
        out: PathBuf,
        /// Frames to render, comma-separated; default: every frame in the file
        #[arg(long, value_delimiter = ',')]
        frames: Vec<u64>,
    },
}

/// Which frames of which segment
#[derive(Args)]
struct Source {
    /// Session folder (with `session.json`)
    session: PathBuf,
    /// Segment number, from 1
    #[arg(long, default_value_t = 1)]
    segment: usize,
    /// First frame
    #[arg(long, default_value_t = 0)]
    start: u64,
    /// Read every `step`-th frame
    #[arg(long, default_value_t = 1)]
    step: u64,
    /// Most frames to read; default: to the end
    #[arg(long)]
    count: Option<u64>,
}

/// Which network and how to run it
#[derive(Args)]
struct Model {
    /// Model size: n, s, m, l or x
    #[arg(long, default_value_t = 's')]
    size: char,
    /// Our own weights (safetensors); default: pretrained COCO from the hub
    #[arg(long, requires = "classes")]
    weights: Option<PathBuf>,
    /// Class names of `--weights`: JSON array, `classes.json` or one per line
    #[arg(long)]
    classes: Option<PathBuf>,
    /// Run on the CPU even when a GPU is available
    #[arg(long)]
    cpu: bool,
    /// Lowest score kept
    #[arg(long, default_value_t = 0.25)]
    confidence: f32,
    /// IoU for non-maximum suppression
    #[arg(long, default_value_t = 0.45)]
    nms: f32,
}

/// Tracker settings
#[derive(Args)]
struct Tracking {
    /// Lowest IoU between prediction and detection that matches
    #[arg(long, default_value_t = 0.3)]
    min_iou: f64,
    /// Matches in a row before a track gets an id
    #[arg(long, default_value_t = 3)]
    min_hits: u32,
    /// Frames a track survives without a match
    #[arg(long, default_value_t = 15)]
    max_age: u64,
}

impl Tracking {
    fn config(&self) -> TrackerConfig {
        TrackerConfig {
            min_iou: self.min_iou,
            min_hits: self.min_hits,
            max_age: self.max_age,
            ..Default::default()
        }
    }
}

fn parse_map(s: &str) -> Result<(String, String), String> {
    s.split_once('=')
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .ok_or_else(|| format!("expected from=to, got {s:?}"))
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Cmd::Detect {
            source,
            model,
            out,
            png,
        } => {
            let segment = Segment::open(&source.session, source.segment - 1)?;
            let frames = run_detector(&segment, &source, &model)?;
            let path = labels::objects_path(&out, &segment.session, segment.stem());
            labels::write_objects(&path, &frames)?;
            println!("wrote {}", path.display());
            if let Some(dir) = png {
                render_all(&segment, &frames, &dir, &[])?;
            }
        }
        Cmd::Track {
            input,
            output,
            tracker,
        } => {
            let frames = labels::read_objects(&input)?;
            let tracks = track::track_frames(&frames, tracker.config());
            let output = output.unwrap_or_else(|| {
                let name = input.file_name().unwrap_or_default().to_string_lossy();
                let stem = name.strip_suffix(labels::OBJECTS_EXT).unwrap_or(&name);
                input.with_file_name(format!("{stem}.tracks.jsonl"))
            });
            labels::write_objects(&output, &tracks)?;
            print_track_summary(&frames, &tracks);
            println!("wrote {}", output.display());
        }
        Cmd::Prelabel {
            source,
            model,
            annotations,
            input,
            track: do_track,
            tracker,
            maps,
        } => {
            let segment = Segment::open(&source.session, source.segment - 1)?;
            let annotations = match annotations {
                Some(dir) => dir,
                None => default_annotations(&source.session)?,
            };
            let mut frames = match input {
                Some(path) => labels::read_objects(&path)?,
                None => run_detector(&segment, &source, &model)?,
            };
            if do_track {
                frames = track::track_frames(&frames, tracker.config());
            }
            prelabel(&segment, &annotations, frames, &maps)?;
        }
        Cmd::Render {
            session,
            objects,
            segment,
            out,
            frames: wanted,
        } => {
            let segment = Segment::open(&session, segment - 1)?;
            let mut frames = labels::read_objects(&objects)?;
            if !wanted.is_empty() {
                frames.retain(|f| wanted.contains(&f.frame));
            }
            let colors = default_annotations(&session)
                .ok()
                .and_then(|dir| labels::read_classes(&dir).ok().flatten())
                .unwrap_or_default()
                .into_iter()
                .map(|c| (c.name, c.color))
                .collect::<Vec<_>>();
            render_all(&segment, &frames, &out, &colors)?;
        }
    }
    Ok(())
}

/// `Annotations` next to the dataset folder holding `session`
fn default_annotations(session: &Path) -> Result<PathBuf> {
    let session = session
        .canonicalize()
        .with_context(|| format!("cannot find {}", session.display()))?;
    let dataset = session
        .parent()
        .context("session folder has no parent (the dataset folder)")?;
    let root = dataset
        .parent()
        .context("dataset folder has no parent for Annotations")?;
    Ok(root.join("Annotations"))
}

/// Run the detector over the frames of `source`, printing timings
fn run_detector(segment: &Segment, source: &Source, model: &Model) -> Result<Vec<FrameObjects>> {
    let weights = match (&model.weights, &model.classes) {
        (Some(path), Some(classes)) => Weights::File {
            path: path.clone(),
            size: model.size,
            classes: detect::read_class_names(classes)?,
        },
        _ => Weights::Coco(model.size),
    };
    let start = Instant::now();
    let mut detector = Detector::load(&weights, detect::device(model.cpu)?)?;
    detector.confidence = model.confidence;
    detector.nms_iou = model.nms;
    println!(
        "loaded yolov8{} ({} classes) on {:?} in {:.0} ms",
        model.size,
        detector.classes().len(),
        detector.device().location(),
        ms(start.elapsed())
    );

    let range = FrameRange {
        first: source.start,
        step: source.step,
        count: source.count,
    };
    let mut reader = FrameReader::start(segment, range)?;
    let mut out = Vec::new();
    let mut times: Vec<(Duration, Timing)> = Vec::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    loop {
        let wait = Instant::now();
        let Some(frame) = reader.next() else { break };
        let frame = frame?;
        let decode = wait.elapsed();
        let (boxes, timing) = detector.detect(&frame.rgb)?;
        let names: Vec<String> = boxes
            .iter()
            .map(|b| format!("{} {:.2}", b.class, b.score.unwrap_or(0.0)))
            .collect();
        println!(
            "frame {:6}: decode {:6.1} pre {:5.1} net {:7.1} post {:5.1} ms | {}",
            frame.number,
            ms(decode),
            ms(timing.preprocess),
            ms(timing.forward),
            ms(timing.postprocess),
            names.join(", ")
        );
        for b in &boxes {
            *counts.entry(b.class.clone()).or_default() += 1;
        }
        times.push((decode, timing));
        if !boxes.is_empty() {
            out.push(FrameObjects::new(frame.number, boxes));
        }
    }
    print_timing_summary(&times);
    let found: Vec<String> = counts.iter().map(|(c, n)| format!("{c} {n}")).collect();
    println!(
        "{} frames with boxes: {}",
        out.len(),
        if found.is_empty() {
            "none".to_string()
        } else {
            found.join(", ")
        }
    );
    Ok(out)
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Mean, median and 95th percentile of each stage
fn print_timing_summary(times: &[(Duration, Timing)]) {
    if times.is_empty() {
        println!("no frames read");
        return;
    }
    let stats = |mut v: Vec<f64>| {
        v.sort_by(f64::total_cmp);
        let mean = v.iter().sum::<f64>() / v.len() as f64;
        let pick = |q: f64| v[((v.len() - 1) as f64 * q).round() as usize];
        format!(
            "mean {mean:7.1}  p50 {:7.1}  p95 {:7.1}",
            pick(0.5),
            pick(0.95)
        )
    };
    let col = |f: fn(&(Duration, Timing)) -> Duration| times.iter().map(|t| ms(f(t))).collect();
    println!("{} frames, ms per frame:", times.len());
    println!("  decode      {}", stats(col(|t| t.0)));
    println!("  preprocess  {}", stats(col(|t| t.1.preprocess)));
    println!("  network     {}", stats(col(|t| t.1.forward)));
    println!("  postprocess {}", stats(col(|t| t.1.postprocess)));
    println!("  detect      {}", stats(col(|t| t.1.total())));
    // The first frame includes one-time setup (allocations, kernels)
    if times.len() > 1 {
        let steady: Vec<f64> = times[1..].iter().map(|t| ms(t.1.total())).collect();
        let mean = steady.iter().sum::<f64>() / steady.len() as f64;
        println!(
            "  after the first frame: {mean:.1} ms, {:.1} frames/s",
            1000.0 / mean
        );
    }
}

fn print_track_summary(detections: &[FrameObjects], tracks: &[FrameObjects]) {
    let mut lengths: BTreeMap<u64, (String, usize)> = BTreeMap::new();
    for b in tracks.iter().flat_map(|f| &f.boxes) {
        let entry = lengths
            .entry(b.id.unwrap_or(0))
            .or_insert((b.class.clone(), 0));
        entry.1 += 1;
    }
    let boxes = |frames: &[FrameObjects]| frames.iter().map(|f| f.boxes.len()).sum::<usize>();
    println!(
        "{} detections in {} frames -> {} tracks, {} tracked boxes",
        boxes(detections),
        detections.len(),
        lengths.len(),
        boxes(tracks)
    );
    for (id, (class, n)) in lengths {
        println!("  #{id} {class}: {n} frames");
    }
}

/// Merge `frames` into the segment's label file under `annotations`
fn prelabel(
    segment: &Segment,
    annotations: &Path,
    mut frames: Vec<FrameObjects>,
    maps: &[(String, String)],
) -> Result<()> {
    // Never write into the dataset: compare absolute paths
    let video = std::path::absolute(&segment.video)?;
    let dataset = video.parent().and_then(Path::parent);
    if let Some(dataset) = dataset
        && std::path::absolute(annotations)?.starts_with(dataset)
    {
        bail!(
            "{} is inside the dataset folder; labels go next to it",
            annotations.display()
        );
    }
    let (classes, created) = labels::read_or_create_classes(annotations)?;
    if created {
        println!(
            "{} was missing: wrote the starter list of {} Salmon Run classes",
            annotations.join(labels::CLASSES_FILE).display(),
            classes.len()
        );
    }
    let known = |c: &str| classes.iter().any(|k| k.name == c);
    let mut dropped: BTreeMap<String, usize> = BTreeMap::new();
    for frame in &mut frames {
        frame.boxes = core::mem::take(&mut frame.boxes)
            .into_iter()
            .filter_map(|mut b: ObjectBox| {
                if let Some((_, to)) = maps.iter().find(|(from, _)| *from == b.class) {
                    b.class = to.clone();
                }
                b.by = labels::Source::Model;
                if known(&b.class) {
                    Some(b)
                } else {
                    *dropped.entry(b.class).or_default() += 1;
                    None
                }
            })
            .collect();
    }
    if !dropped.is_empty() {
        let list: Vec<String> = dropped.iter().map(|(c, n)| format!("{c} {n}")).collect();
        println!(
            "dropped boxes of classes not in classes.json: {}",
            list.join(", ")
        );
    }
    let path = labels::objects_path(annotations, &segment.session, segment.stem());
    let existing = labels::read_objects(&path)?;
    let (merged, stats) = labels::merge_model_boxes(existing, frames);
    labels::write_objects(&path, &merged)?;
    println!(
        "{}: {} frames added, {} replaced (model only), {} kept (labeled by a person)",
        path.display(),
        stats.added,
        stats.replaced,
        stats.kept
    );
    Ok(())
}

/// Save every frame of `frames` with its boxes as `<dir>/<frame>.png`
fn render_all(
    segment: &Segment,
    frames: &[FrameObjects],
    dir: &Path,
    colors: &[(String, String)],
) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    for f in frames {
        let out = dir.join(format!("{}-{:06}.png", segment.session, f.frame));
        render::render_frame(segment, f.frame, &f.boxes, colors, &out)?;
    }
    println!("saved {} frames in {}", frames.len(), dir.display());
    Ok(())
}
