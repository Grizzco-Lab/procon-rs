//! Frame to detections: a YOLOv8 network, its class names and the
//! pre- and post-processing around it.
//!
//! A frame is padded at the bottom to a multiple of 32 (360 to 384 rows,
//! gray as in YOLO's letterboxing) rather than stretched, so boxes map back
//! to the frame without scaling. Scores above the threshold go through
//! per-class non-maximum suppression.

use crate::frames::{HEIGHT, WIDTH};
use crate::labels::ObjectBox;
use anyhow::{Context, Result, bail};
use candle_core::{DType, Device, Module, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::object_detection::{Bbox, non_maximum_suppression};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::yolo::{Multiples, YoloV8};

/// The 80 COCO classes of the pretrained weights, in output order
pub const COCO_CLASSES: [&str; 80] = [
    "person",
    "bicycle",
    "car",
    "motorcycle",
    "airplane",
    "bus",
    "train",
    "truck",
    "boat",
    "traffic light",
    "fire hydrant",
    "stop sign",
    "parking meter",
    "bench",
    "bird",
    "cat",
    "dog",
    "horse",
    "sheep",
    "cow",
    "elephant",
    "bear",
    "zebra",
    "giraffe",
    "backpack",
    "umbrella",
    "handbag",
    "tie",
    "suitcase",
    "frisbee",
    "skis",
    "snowboard",
    "sports ball",
    "kite",
    "baseball bat",
    "baseball glove",
    "skateboard",
    "surfboard",
    "tennis racket",
    "bottle",
    "wine glass",
    "cup",
    "fork",
    "knife",
    "spoon",
    "bowl",
    "banana",
    "apple",
    "sandwich",
    "orange",
    "broccoli",
    "carrot",
    "hot dog",
    "pizza",
    "donut",
    "cake",
    "chair",
    "couch",
    "potted plant",
    "bed",
    "dining table",
    "toilet",
    "tv",
    "laptop",
    "mouse",
    "remote",
    "keyboard",
    "cell phone",
    "microwave",
    "oven",
    "toaster",
    "sink",
    "refrigerator",
    "book",
    "clock",
    "vase",
    "scissors",
    "teddy bear",
    "hair drier",
    "toothbrush",
];

/// Hugging Face repository of the pretrained COCO weights (converted from
/// Ultralytics; AGPL-3.0)
pub const COCO_REPO: &str = "lmz/candle-yolo-v8";

/// Rows the network sees: [`HEIGHT`] rounded up to a multiple of 32
const INPUT_HEIGHT: usize = HEIGHT.div_ceil(32) * 32;

/// Gray of the padding, as in YOLO's letterboxing
const PAD: f32 = 114.0 / 255.0;

/// Where the weights come from
#[derive(Clone, Debug)]
pub enum Weights {
    /// The pretrained COCO weights of a size, from the Hugging Face hub
    Coco(char),
    /// A safetensors file with the given size and class names
    File {
        /// The safetensors file
        path: PathBuf,
        /// Model size: `n`, `s`, `m`, `l` or `x`
        size: char,
        /// Class names in output order
        classes: Vec<String>,
    },
}

/// A detection network ready to run
pub struct Detector {
    model: YoloV8,
    classes: Vec<String>,
    device: Device,
    /// Lowest score kept
    pub confidence: f32,
    /// IoU above which the lower-scored of two same-class boxes is dropped
    pub nms_iou: f32,
}

/// Time spent on one frame
#[derive(Clone, Copy, Debug, Default)]
pub struct Timing {
    /// Frame to input tensor, including the copy to the device
    pub preprocess: Duration,
    /// The network, until the device is done
    pub forward: Duration,
    /// Scores to boxes, including the copy back and NMS
    pub postprocess: Duration,
}

impl Timing {
    /// The whole frame
    pub fn total(&self) -> Duration {
        self.preprocess + self.forward + self.postprocess
    }
}

/// Download (or find in the cache) `yolov8<size>.safetensors` of
/// [`COCO_REPO`]
pub fn coco_weights(size: char) -> Result<PathBuf> {
    let api = hf_hub::api::sync::Api::new()?;
    let file = format!("yolov8{size}.safetensors");
    api.model(COCO_REPO.to_string())
        .get(&file)
        .with_context(|| format!("cannot download {file} from {COCO_REPO}"))
}

/// The device to run on: the first CUDA GPU if built with the `cuda`
/// feature and one is present, unless `cpu` is set
pub fn device(cpu: bool) -> Result<Device> {
    if cpu {
        Ok(Device::Cpu)
    } else {
        Ok(Device::cuda_if_available(0)?)
    }
}

impl Detector {
    /// Load `weights` onto `device`
    pub fn load(weights: &Weights, device: Device) -> Result<Self> {
        let (path, size, classes) = match weights {
            Weights::Coco(size) => (
                coco_weights(*size)?,
                *size,
                COCO_CLASSES.iter().map(|s| s.to_string()).collect(),
            ),
            Weights::File {
                path,
                size,
                classes,
            } => (path.clone(), *size, classes.clone()),
        };
        let Some(multiples) = Multiples::of(size) else {
            bail!("unknown model size {size:?}, expected one of n, s, m, l, x");
        };
        // SAFETY: the weights file is not modified while mapped
        let vb = unsafe { VarBuilder::from_mmaped_safetensors(&[&path], DType::F32, &device)? };
        let model = YoloV8::load(vb, multiples, classes.len())
            .with_context(|| format!("cannot load {}", path.display()))?;
        Ok(Self {
            model,
            classes,
            device,
            confidence: 0.25,
            nms_iou: 0.45,
        })
    }

    /// Class names in output order
    pub fn classes(&self) -> &[String] {
        &self.classes
    }

    /// The device the network runs on
    pub fn device(&self) -> &Device {
        &self.device
    }

    /// Detect objects in a [`WIDTH`] x [`HEIGHT`] RGB frame; boxes are model
    /// boxes in frame fractions, highest score first
    pub fn detect(&self, rgb: &[u8]) -> Result<(Vec<ObjectBox>, Timing)> {
        let mut timing = Timing::default();
        let start = Instant::now();
        let input = self.input(rgb)?;
        timing.preprocess = start.elapsed();

        let start = Instant::now();
        let pred = self.model.forward(&input)?.squeeze(0)?;
        self.device.synchronize()?;
        timing.forward = start.elapsed();

        let start = Instant::now();
        let boxes = self.boxes(&pred)?;
        timing.postprocess = start.elapsed();
        Ok((boxes, timing))
    }

    /// `(1, 3, INPUT_HEIGHT, WIDTH)` in 0..1, padded at the bottom
    fn input(&self, rgb: &[u8]) -> Result<Tensor> {
        if rgb.len() != WIDTH * HEIGHT * 3 {
            bail!("frame is {} bytes, not {WIDTH}x{HEIGHT} RGB", rgb.len());
        }
        let mut chw = vec![PAD; 3 * INPUT_HEIGHT * WIDTH];
        let plane = INPUT_HEIGHT * WIDTH;
        for (i, px) in rgb.as_chunks::<3>().0.iter().enumerate() {
            for c in 0..3 {
                chw[c * plane + i] = f32::from(px[c]) / 255.0;
            }
        }
        Ok(Tensor::from_vec(
            chw,
            (1, 3, INPUT_HEIGHT, WIDTH),
            &self.device,
        )?)
    }

    /// Network output `(4 + classes, anchors)` to boxes after NMS
    fn boxes(&self, pred: &Tensor) -> Result<Vec<ObjectBox>> {
        let rows: Vec<Vec<f32>> = pred.t()?.to_device(&Device::Cpu)?.to_vec2()?;
        let mut by_class: Vec<Vec<Bbox<()>>> = vec![Vec::new(); self.classes.len()];
        for row in rows {
            let (class, &score) = row[4..]
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .context("model has no classes")?;
            if score < self.confidence {
                continue;
            }
            let (cx, cy, w, h) = (row[0], row[1], row[2], row[3]);
            by_class[class].push(Bbox {
                xmin: cx - w / 2.0,
                ymin: cy - h / 2.0,
                xmax: cx + w / 2.0,
                ymax: cy + h / 2.0,
                confidence: score,
                data: (),
            });
        }
        non_maximum_suppression(&mut by_class, self.nms_iou);
        let (fw, fh) = (WIDTH as f32, HEIGHT as f32);
        let mut boxes: Vec<ObjectBox> = by_class
            .iter()
            .enumerate()
            .flat_map(|(class, found)| {
                found.iter().map(move |b| {
                    // Clip to the frame; the padding is below it
                    let x0 = b.xmin.clamp(0.0, fw);
                    let x1 = b.xmax.clamp(0.0, fw);
                    let y0 = b.ymin.clamp(0.0, fh);
                    let y1 = b.ymax.clamp(0.0, fh);
                    let rect = [x0 / fw, y0 / fh, (x1 - x0) / fw, (y1 - y0) / fh];
                    ObjectBox::model(
                        &self.classes[class],
                        rect.map(f64::from),
                        f64::from(b.confidence),
                    )
                })
            })
            .filter(|b| b.w > 0.0 && b.h > 0.0)
            .collect();
        boxes.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
        Ok(boxes)
    }
}

/// Class names from a file: a JSON array of names, a JSON array of
/// `classes.json` entries, or one name per line
pub fn read_class_names(path: &Path) -> Result<Vec<String>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    if let Ok(names) = serde_json::from_str::<Vec<String>>(&text) {
        return Ok(names);
    }
    if let Ok(classes) = serde_json::from_str::<Vec<crate::labels::ClassInfo>>(&text) {
        return Ok(classes.into_iter().map(|c| c.name).collect());
    }
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect())
}
