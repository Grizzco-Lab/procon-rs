# gameplay-vision

Object detection and tracking on recorded Salmon Run sessions: the first step
toward placing enemies on the stage in 3D. A library and a CLI,
`gameplay-vision`, built on [candle](https://github.com/huggingface/candle)
0.11, running on the CPU by default. The studio's **Vision** app runs the same
detection, tracking and prelabeling from the dashboard (see the main README);
the CLI stays for scripts and batch work.

```bash
cargo build --release -p gameplay-vision
BIN=target/release/gameplay-vision
S=/path/to/Dataset/2026-09-25_11-26-22

# Detect: every 300th frame from frame 3000, 8 frames; per-frame timings printed
$BIN detect $S --start 3000 --step 300 --count 8 --out detections --png detections/png
# Track: detections -> tracks with ids (detections/<session>/video-01.tracks.jsonl)
$BIN track detections/2026-09-25_11-26-22/video-01.objects.jsonl
# Prelabel: model boxes into the shared labels, for correction in the labeling tool
$BIN prelabel $S --start 0 --step 30 --track --weights salmon.safetensors --classes classes.json
$BIN prelabel $S --input detections/2026-09-25_11-26-22/video-01.tracks.jsonl --map person=player
# Render any object file (detections, tracks, labels) as PNGs
$BIN render $S detections/2026-09-25_11-26-22/video-01.tracks.jsonl --out png --frames 3061,3071
```

Without `--weights`, the pretrained COCO YOLOv8 (`--size n|s|m|l|x`, default
`s`) is downloaded from the Hugging Face hub (`lmz/candle-yolo-v8`) into
`~/.cache/huggingface`. Our own weights are a safetensors file with the same
tensor names (`net.*`, `fpn.*`, `head.*`) and any class count, plus their class
names (`--classes`: a JSON array, a `classes.json` or one name per line).

## GPU

The `cuda` feature runs the network on the first NVIDIA GPU. It needs the CUDA
toolkit (`nvcc`) at build time; without it the build stops in `cudarc` with
"`nvcc --version` failed".

```bash
cargo build --release -p gameplay-vision --features cuda
$BIN detect $S --count 300             # GPU when present
$BIN detect $S --count 300 --cpu       # same binary, CPU, for comparison
```

Every frame prints decode, preprocess, network and postprocess times; the
summary gives mean, median and 95th percentile per stage, and frames per
second after the first frame (which includes one-time setup). The network
time waits for the device (`Device::synchronize`), so GPU numbers are real.

## Modules

| Module | What |
|---|---|
| `frames` | A segment's frames from an ffmpeg subprocess as 640x360 RGB; frame `n` at `n / fps` (constant-rate sessions only) |
| `yolo` | YOLOv8 network, adapted from candle's `yolo-v8` example (MIT OR Apache-2.0) |
| `detect` | Frame to boxes: padding to 640x384, the network, per-class NMS |
| `track` | SORT-like tracker: Hungarian matching on IoU per class, constant velocity (alpha-beta filter), confirmation after 3 hits, death after 15 missed frames |
| `labels` | The label format shared with the labeling tool, and the prelabel merge rule |
| `render` | Frames with boxes drawn, as PNG, through ffmpeg's `drawbox`/`drawtext` |

## Label format

`<annotations>/classes.json` lists the classes (`name`, `label`, `color`);
`<annotations>/<session>/<segment file stem>.objects.jsonl` holds one line per
labeled frame:

```json
{"frame": 120, "boxes": [{"class": "steelhead", "x": 0.41, "y": 0.22, "w": 0.1, "h": 0.18, "id": 7, "by": "model", "score": 0.83}]}
```

`x`, `y` (top-left) and `w`, `h` are fractions of the frame; `id` is a track id;
`by` is `user` or `model` (missing counts as `user`). Keys this crate does not
know are kept. `detect` and `track` write the same format, so any object file
can be rendered, tracked or prelabeled.

The annotations folder defaults to `Annotations` next to the dataset folder
that holds the session; `prelabel` refuses a folder inside the dataset. If
`classes.json` is missing, `prelabel` writes a starter list (the Salmonids,
`golden_egg`, `player`) and says so. Boxes of classes not in `classes.json` are
dropped (and counted); `--map from=to` renames first.

**Merge rule.** A frame a person has labeled is never changed: one that holds
any box not by the model, or no box at all (the labeling tool's "nothing
here"). A frame with only model boxes gets the new model boxes instead; a frame
without a line gets one. So correcting a frame, including deleting a wrong
model box, is never undone by the next prelabel. Files are written to a
temporary name and renamed; a prelabel racing an open labeling session on the
same segment can still lose that session's next save, so prelabel segments
nobody is labeling.

## What pretrained models see in Salmon Run

Measured on 40 frames across the six sessions of 2026-09-25 (16, 4, 4, 12, 2,
2 frames, evenly spaced), 640x360, CPU (Ryzen 9 9950X, 16 cores), confidence
0.25, NMS 0.45:

| Model | ms / frame, mean | p50 | p95 | Boxes in 40 frames |
|---|---|---|---|---|
| YOLOv8n (COCO) | 144 | 142 | 183 | 37 |
| YOLOv8s (COCO) | 253 | 256 | 299 | 64 |
| YOLOv8m (COCO) | 473 | 468 | 560 | 79 |

Decoding costs under 1 ms per frame after the first (ffmpeg runs ahead);
preprocessing and NMS about 1 ms each. candle's CPU convolutions use about
three cores here (user time / wall time ≈ 2.7), so the GPU should be far
faster; compare with the `cuda` build.

**What they find.** Salmonids are not COCO classes, and COCO models find
almost none of them. Nearly every box is a false positive: the special gauge
in the top-right corner is a `clock` in most frames (a stable one: the tracker
keeps it as one track over 100+ frames), ships and structures on the water are
`boat`, UI panels are `tv`/`train`/`refrigerator`, ink tanks and weapons are
`bottle`/`cell phone`. The few boxes on relevant objects have the wrong class:
golden eggs as `bowl`/`donut`/`sports ball`, a boss once as `person`, the
lobby's practice targets as `vase`. Lowering the confidence to 0.1 adds more
of the same. Over 150 consecutive frames, the tracker turns 370 detections
into 19 tracks, only one of them long (the HUD `clock`).

**Open-vocabulary.** candle 0.11 has no open-vocabulary detector (no
OWL-ViT/OWLv2, Grounding DINO, YOLO-World or Florence-2). Its PaliGemma port
can be prompted with `detect <thing>`, but the weights are gated (Gemma terms)
and a 3B model takes seconds per frame on a CPU. As a cheap test, CLIP
ViT-B/32 (MIT) scored 93 sliding windows per frame (96 and 160 px) against
text prompts for eleven Salmonids and eight background descriptions: 195 of
744 windows went to a Salmonid with p > 0.4, almost all of them on sky, water
or ink, and a golden egg came out as `stinger`. 1.9 s per frame. CLIP knows
Splatoon too little to name Salmonids from text; labeled data is needed.

## Design note: Salmon Run detection

### What to label first

1. **Lesser Salmonids and golden eggs** (`smallfry`, `chum`, `cohock`,
   `golden_egg`): most frequent on screen and what most of a wave is about.
2. **Bosses by frequency**: in a normal wave `steelhead`, `scrapper`,
   `flyfish`, `stinger`, `maws`, `drizzler`, `steel_eel`, `big_shot`,
   `slammin_lid`, `fish_stick`, `flipper_flopper`. Count them from the
   prelabels of the first model and label the common ones first.
3. **`player`** (teammates), for trajectories and occlusion.

Label everything of a chosen class in a frame (a missing box teaches the model
"background"). Leave the HUD alone and decide one rule for boss alert icons and
names drawn over the scene (skip them). Tag frames outside a wave (lobby,
practice area, results) so they can be excluded or used as negatives.

**How many.** For fine-tuning a pretrained detector: about 150-300 boxes per
class gives a usable first model; 1,000+ per class for a robust one. Rare
bosses: at least 50-100 boxes over several stages and lighting (night, fog,
tides). Pick frames 1-2 s apart (30-60 frames at 30 fps): neighbouring frames
are near copies. Spread them over stages, waves and sessions, and hold out
whole sessions for evaluation (frames of one session are correlated).

### Active learning loop

1. Label about 300 frames by hand across sessions.
2. Train; evaluate on held-out sessions (mAP@0.5 per class).
3. `prelabel --track` the next 1,000-2,000 frames; the labeling tool shows
   the model boxes for correction, which is several times faster than drawing.
4. Review first the frames where the model is least sure: scores near the
   threshold, tracks that break and restart, classes that flip within a track,
   frames with many boxes.
5. Retrain with the corrected frames and repeat. Stop when corrections per
   frame fall below about one.

### Fine-tuning path and licenses

- **Avoid Ultralytics code and weights**: the `ultralytics` package and its
  YOLOv8/v11 weights are AGPL-3.0 (or a paid license). The COCO weights used
  here (`lmz/candle-yolo-v8`) are converted from them; they are fine for these
  measurements but should not end up in anything we ship, and fine-tuning from
  them would inherit the license. YOLO-World is GPL-3.0.
- **Permissive detectors to fine-tune**: RT-DETR, D-FINE, DEIM (Apache-2.0,
  COCO-pretrained weights), YOLOX (Apache-2.0), DETR (Apache-2.0). Candle's
  `yolo` module holds only the architecture (MIT OR Apache-2.0); a model of
  that architecture trained by us from scratch carries no Ultralytics license.
- **Training in PyTorch** (recommended): the ecosystem (augmentation,
  assigners, losses, mixed precision) is there, and the AgentZero project is
  Python already. Train an Apache-2.0 detector as the *teacher*, whose
  predictions go into `prelabel --input` (the label format decouples the two).
  Then train a small YOLOv8-architecture *student* from scratch on the labeled
  plus teacher-labeled frames (tens of thousands of frames are cheap to
  pseudo-label), save it as safetensors with this crate's tensor names, and run
  it here with `--weights`. That gives a license-clean, fast model in Rust.
- **Training in candle**: possible (autograd, AdamW, the `mnist-training`
  example), but YOLO's loss (task-aligned assignment, DFL, CIoU) and data
  augmentation would have to be written, and CPU backward passes are slow.
  Worth it only once the loss and data pipeline are settled in PyTorch.

## Design note: enemies in 3D

### What it takes

- **Stage geometry**: a height map or coarse mesh per stage, in stage
  coordinates. Game assets are Nintendo's; building our own is cleaner:
  structure from motion (COLMAP, BSD) over our own recordings of a stage, or a
  hand-traced top-down map with platform heights. Monocular depth
  (Depth Anything V2 in candle; the small model is Apache-2.0, the larger ones
  are non-commercial) helps densify.
- **Camera pose per frame**: rotation and position. The camera orbits behind
  the player; its yaw and pitch follow the gyro (scaled by the recorded
  motion sensitivity, `session.json` `game_settings`) and the right stick
  (stick sensitivity), and it is reset by game events (respawn, super jump,
  camera reset). Integrating the recorded gyro gives smooth relative rotation
  with drift; it needs absolute fixes from the image (visual localization
  against the stage reconstruction: features + PnP).
- **Player position**: integrating the left stick is too rough (speed depends
  on weapon, ink, swim form and slopes); visual localization gives position
  and rotation together, and the gyro fills the frames between.
- **Enemy position**: cast a ray through the bottom center of the box (the
  ground contact) onto the stage geometry. Check it with the box height and
  the class's known size (a Chum is roughly player-sized, a Steelhead much
  taller), which also gives a depth when the feet are hidden. Flyers and
  Salmonids on walls need class-specific rules. Tracks smooth positions over
  time and bridge occlusions.

### Our recordings vs others' videos

Our sessions carry the controller log (gyro, sticks, buttons), game settings
and a measured video delay, so rotation between frames is known up to drift and
the image only has to correct it. Others' videos have only pixels (and a HUD,
resolution and frame rate of their own): pose must come from visual
localization alone, which works only once a stage reconstruction exists. Plan:
build everything on our recordings, then reuse the reconstructions and the
detector for others' videos.

### Phases

| Phase | Milestone | Check |
|---|---|---|
| 0 (this crate) | Detection, tracking, label format, prelabel, timings | Unit tests; runs on real sessions |
| 1 | Salmon Run detector v1 from 1-2k labeled frames, active learning running | mAP@0.5 ≥ 0.6 on lesser Salmonids and eggs, held-out sessions |
| 2 | Stable 2D tracks: HUD mask, re-identification after occlusion, class vote per track | ID switches per minute on labeled clips |
| 3 | Camera rotation from the gyro: sensitivity mapping, drift correction from background motion | Yaw/pitch error against rotation estimated from the image |
| 4 | One stage reconstructed, frames localized against it | Share of frames localized; reprojection error |
| 5 | Enemies on the stage: ground-contact ray casts, top-down map and 3D view of tracks | Position error on hand-placed checkpoints |
| 6 | More stages, then others' videos | Same metrics per stage |
