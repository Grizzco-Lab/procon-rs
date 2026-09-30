# gameplay-vision

Object detection and tracking on recorded Salmon Run sessions: the first step
toward placing enemies on the stage in 3D. A library and a CLI,
`gameplay-vision`, built on [candle](https://github.com/huggingface/candle)
0.11, running on the CPU by default. Grizzco Lab's **Vision** app runs the same
detection, tracking and prelabeling from the dashboard (see the main README);
the CLI stays for scripts and batch work.
It also reads the Salmon Run HUD (wave number and timer) from any video,
for placing comments like "W2 83s" ([below](#salmon-run-hud-waves-and-timer)).

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
| `hud` | The Salmon Run HUD (wave, timer, eggs) read from any video, and its wave table (`wave_starts.json`); see below |

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
`golden_egg`, `player`, `dead_player`, `basket`, the King Salmonids
`cohozuna`, `horrorboros`, `megalodontia`, then `snatcher` and the Salmonids
of the known occurrences `goldie`, `griller`, `mudmouth`, `gold_mudmouth`,
`chinook`, `mothership`: every named row of Lean's `CoopEnemyInfo`, the same
list as the labeling tool's) and says so. Boxes of classes not in `classes.json` are
dropped (and counted); `--map from=to` renames first.

**Merge rule.** A frame a person has labeled is never changed: one that holds
any box not by the model, or no box at all (the labeling tool's "nothing
here"). A frame with only model boxes gets the new model boxes instead; a frame
without a line gets one. So correcting a frame, including deleting a wrong
model box, is never undone by the next prelabel. Files are written to a
temporary name and renamed; a prelabel racing an open labeling session on the
same segment can still lose that session's next save, so prelabel segments
nobody is labeling.

## Salmon Run HUD: waves and timer

`hud` reads the Salmon Run HUD in the top-left corner (wave label, the
wave's countdown timer, the golden egg counter) from any video, ours or
others', and turns it into a per-video wave table, so a comment like "W2
83s" can be placed at a video time. No model and no GPU: connected blobs and
template matching on the CPU.

```bash
BIN=target/release/gameplay-vision
# Wave table of a video, every 0.5 s (default); writes <stem>.wave_starts.json next to it
$BIN hud scan clip.mp4
$BIN hud scan Dataset/2026-09-25_11-26-22/video-01.mkv --out /tmp/s1.wave_starts.json --track /tmp/s1.track.jsonl
# Video time of "W2 83s"
$BIN hud time /tmp/s1.wave_starts.json 2 83          # 168.25
# What the reader sees at some times (with the HUD crops as PNG)
$BIN hud read clip.mp4 --at 12.5,30 --png /tmp/hud
# Rebuild the templates from our own recordings (below)
$BIN hud learn Dataset/2026-09-25_11-26-22/video-01.mkv --out src/hud/templates.txt
```

`--region x,y,w,h` gives the game picture in the video's pixels when the
automatic one (the frame without black bars) is wrong, such as a phone
filming a TV; `--templates` reads with another templates file.

### `wave_starts.json`

```json
{
  "video": "video-01.mkv",
  "duration_s": 427.766,
  "every_s": 0.5,
  "region": { "x": 0, "y": 0, "w": 640, "h": 360 },
  "samples": 856,
  "hud_samples": 682,
  "waves": [
    { "wave": 1, "start_video_s": 30.75, "end_video_s": 130.75, "timer_at_start": 100.0,
      "wave_read": true, "extra": false, "readings": 194, "agree": 0.99 }
  ]
}
```

- `wave`: 1, 2, 3, ...; an extra wave (`XTRAWAVE`, the King Salmonid) is
  `4` with `extra: true`. `wave_read` is false when the number was not read
  from the label but counted on from the wave before.
- `start_video_s`: when the countdown starts (the switch to 99 is one second
  later), or when the wave is first seen in a clip that starts mid-wave.
  `end_video_s`: when the timer reaches 0, or when the wave is last seen (a
  wipe, the end of the clip, a cut).
- `timer_at_start`: the countdown at `start_video_s`, fractional (the
  display shows the whole number above it); 100 for a whole wave.
- `readings` / `agree`: timer readings (1 to 99) inside the wave and the
  share that fit its countdown; a low `agree` means a doubtful wave.

**Timer to video time.** The display switches to `T` at

```text
video_s = start_video_s + timer_at_start - T
```

for the entry of wave `W` whose `[start_video_s - 1, end_video_s + 1]` holds
the result (`WaveTable::to_video_time(wave, timer_s)`; `None` when no part
of the video shows that moment). An edited video (a VOD review with cuts) can
list one wave several times, one entry per continuous part; take the first
entry that holds the moment. `WaveTable::at(t)` is the inverse: wave and
timer at a video time.

From Rust: `hud::scan(path, every_s, region, hud::Reader::builtin())`
returns the table and the samples; `hud::read(&HudCrop)` reads one crop
(`HudCrop::from_frame` cuts it from a whole RGB frame).

### How it works

1. **Crop** (`hud::video`): ffmpeg decodes, keeps one frame per `every_s`
   (`fps=...:round=up`, so sample `k` is the frame at `k * every_s`), crops
   the top-left 31.25% x 16.7% of the game picture and scales it to 400x120
   (a 1280x720 frame's corner at its own size), so glyph sizes do not depend
   on the resolution. The game picture is the frame without black bars,
   found on nine frames spread over the video.
2. **Blobs** (`hud::glyph`, `hud::Parts`): pixels whose smaller of red and
   green is above 170 are text: white and yellow digits pass, the dark band
   behind them and the orange band of the last seconds do not. 4-connected
   blobs; the timer is the leftmost row of one to three digit-sized blobs
   in the lower half, the wave digit the rightmost tall blob (23 px or more)
   just above it, the egg counter the smaller blobs on the timer's row to the
   right: the `/`, and on either side the glyphs next to it, each within 0.85
   of the `/`'s height of the one before (the round egg icon is left out).
3. **Templates**: each blob is scaled to 20x24 by its height (a `1` stays
   narrow) and compared with mean glyphs per character (score: 1 minus the
   mean absolute difference, at least 0.8). `src/hud/templates.txt` (8 KB)
   holds timer digits 0-9, wave digits 1-5 and `/`: all but the wave digits 4
   and 5 from session 2026-09-25_11-26-22 (360p, Chinese UI), those from
   eleven Eggstra Work waves of each number in Azu's streams (720p, Japanese
   UI). The counter's digits are matched with the timer's, at 0.85: once the
   quota is met, sparkles swarm around the counter, and one stuck to a digit
   lowers its score, which leaves the counter unread rather than read short or
   wrong. A count of 100 or more is set in narrower digits (10 px wide at
   720p, the others 13), stretched sideways by 1.25 before matching.
4. **Physics** (`hud::waves`): within a wave, every right reading `T` at
   `t` gives the same `t + T` (within the second a value stays shown).
   Readings are grouped by that sum (within 1.5 s); a wave needs 4 readings
   of 3 different values, and of two groups seen at the same time the smaller
   is dropped, which removes misreads. The countdown's zero is the middle of
   what the sums in the densest one-second window allow. The wave number is
   the label's clear majority, else the wave before plus one.

**Learning templates** (`hud learn`, `hud::learn`): the timer is the only
three-digit number (`100`, shown while the wave is about to start), so the
frame where its digit count drops from three to two is the switch to 99; from
there every frame's value is known, and each glyph is added to its
character's mean (frames within two frames of a switch are skipped; the wave
digit is the wave's order, `--first-wave` for the first; the counter's `/` is
its third glyph from the right). As a check, the drop from two digits to one
(10 to 9) came exactly 90.000 s later in all five waves. `--fit` labels frames
with the wave table read by the current templates instead, for footage
without a wave start (for example Splatoon 2 clips, `--game s2`). Learned
templates replace those of the same game, role and character; the others in
the file are kept, and `--only` keeps only some of the learned ones: the wave
digits 4 and 5 came from clips of single Eggstra Work waves, from 4 s before
the countdown to 30 s into it (`hud learn w4/*.mkv --first-wave 4 --only
wave:4`, then the same for 5 with `--templates` the result).

### Accuracy and speed

Measured on 2026-09-26 (Ryzen 9 9950X), templates from session 11-26-22.

**Our recordings** (six sessions, 360p, Chinese UI; truth from `hud learn`'s
exact switch times at 30 fps): 5 of 5 waves found (11-26-22: waves 1-3;
13-06-16, which starts at wave 2: waves 2-3), numbers right, no false waves in
the four lobby sessions (one lone false reading in 253 samples). Countdown
start error -0.22, +0.08, -0.18, -0.05, +0.22 s (sampling every 0.5 s bounds
it by 0.25 s); "W2 83s" in 11-26-22 lands at 168.25 s against 168.17 s.
Samples inside the countdowns: 11-26-22 98.5% read right, 1.5% unread (white
flashes, splats over the HUD), no wrong reading; 13-06-16 98.0% right, 1.8%
unread, one wrong (54 read as 5 with the 4 covered), dropped by the fit.

**Others' footage** (46 Discord attachments, 5-388 s, 360p to 1080p, 30 and
60 fps, English, German, Spanish, Japanese and Chinese UI, Switch captures,
screen and phone recordings; references are the fit and a visual check):

| | Clips | Waves found | Samples in waves read | Reads agreeing with the fit |
|---|---|---|---|---|
| Splatoon 3 | 42 | 36 (4 have no wave: lobby, matchmaking, another mode) | 93.4% | 96.6% |
| Splatoon 2 (2020-2022) | 4 | 4 | 94.4% | 99.2% |

All 42 found waves carry the right number (checked against the frames), the
extra wave included. Of 43 random frames inside found waves, the 42 with the
HUD visible showed exactly the wave and timer the table gives. Missed: a very blurry 360p phone video, and a phone
filming a TV with the HUD's corner outside the frame (needs `--region`, and
cannot be read where the picture is cut). Readings that disagree come from
phone and screen recordings (bluish white, blur) and from videos whose own
timing drifts from the game's (dropped frames: up to half a second off);
edited VOD reviews give one entry per part. Splatoon 2's HUD has the same
layout and close enough digits that the Splatoon 3 templates read it;
templates learned from two S2 clips (`--fit --game s2`) read the four S2
clips exactly as well, so none are shipped yet.

**Top players' streams** (AgentZero's corpus, 720p, Japanese and Chinese UI;
2026-09-30; labels are the counter as AgentZero's `eggs` cleaned it, frames
where it holds half a second either side). The egg counter, frames read right
/ unread / wrong, before and after the counter's reading was reworked:

| | Frames | Before | After |
|---|---|---|---|
| Before the quota is met (42 waves) | 2,288 | 97.4 / 2.0 / 0.6% | 97.3 / 2.7 / 0.0% |
| After it: sparkles, flashes (83 waves) | 4,813 | 94.8 / 2.0 / 3.2% | 94.1 / 5.7 / 0.3% |
| Our sessions 11-26-22 and 13-06-16 (360p) | 4,283 | 97.2 / 2.5 / 0.3% | 97.1 / 2.8 / 0.0% |

Counts of 100 or more: 0 and 6 were read as 8 in most frames (106 as 108,
100 as 108 or 188); 100 random frames read with three digits now, checked by
eye, all show the count read. The wrong reads left after the quota are mostly
a digit a sparkle hides whole (32 read as 3), which the counter's cleaning
drops as it only rises. Eggstra Work's waves (50 clips of 8 s, other jobs than
the learning's): wave 4 read in 97.2% of frames (before: none), wave 5 in
90.1% (before: 86% as 3), 1 to 3 as before (88-97%, the rest unread).

**Speed**: decoding is almost all of it. 0.6 s per minute of 360p video
(7 min in 4.0 s) and about 1.8 s per minute of 720p60 or 1080p30 (86 s of
1080p in 2.6 s; ffmpeg uses about eight cores), finding the black bars
included (about 0.5 s). Short clips are dominated by ffmpeg's start-up.

**Left to do**: Splatoon 2 templates from longer S2 footage (YouTube);
locating the HUD when the picture is cropped, shifted or has an overlay
(facecam, stream layout, a call's name tag over the wave label) instead of
`--region`.

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
