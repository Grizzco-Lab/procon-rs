# Contributing

How the code is laid out, how the pieces fit, and how to work on it. For what
the project does and how to run it, see the [README](README.md).

## Development

```bash
cargo build --release                  # both binaries for this machine
cargo test --workspace                 # procon and every crate's unit tests
cargo clippy --workspace --all-targets
cargo fmt

cargo run --example fake_proxy [port]  # synthetic controller, no Pi needed (default port 7331)
./scripts/run.sh                       # the studio with config.toml
./scripts/deploy.sh [ssh-host]         # cross-compile, copy and restart the proxy on the Pi
./scripts/run-proxy.sh                 # build and run the proxy on the Pi itself
```

The proxy is cross-compiled for `aarch64-unknown-linux-musl`: a static binary
linked by Rust's bundled `rust-lld` (`.cargo/config.toml`), so no C cross
toolchain or Pi sysroot is needed. `rust-toolchain.toml` adds the target.

To work on the studio without hardware, run `fake_proxy` on a free port, point
a copy of `config.toml` at it (`[proxy] address`, a different `[web] port`,
`[video] input = "screen"` or `""`) and run
`target/release/procon --config <copy>`. Dashboard settings are saved next to
that copy (`<name>.state.json`).

`scripts/gadget_procon.sh` and `scripts/cleanup_gadget.sh` set up and remove
the USB gadget by hand; the proxy does this itself (`src/gadget.rs`).

### Python bindings

`crates/gameplay-data` builds a Python module, `gameplay_data`, with maturin
(`pyproject.toml`, the `python` feature). AgentZero depends on it as an
editable path dependency, so `uv` rebuilds it when the Rust sources change.

## Conventions

- Comments and docs in English; use `///` and `//!` doc comments.
- Add and remove dependencies with `cargo add` / `cargo rm`, with as few
  features as needed, and no unused packages. Edit `Cargo.toml` by hand only for
  what cargo cannot do; never edit `Cargo.lock` by hand.
- Prefer `core` and `alloc` over `std` where possible.
- Run `cargo fmt` after editing Rust, and `prettier --write` for HTML (the
  dashboard in `web/` and the write-up in `doc/`).
- Keep it simple.

## Layout

| Path | What |
|---|---|
| `src/bin/procon-proxy.rs` | USB proxy binary (`proxy.toml`): reset the controller, set up the gadget, stream frames, forward |
| `src/bin/main.rs` | Studio binary `procon` (`config.toml`): frame receiver, video, recorder, replay player, dashboard |
| `src/config.rs` | Both configuration files |
| `src/device.rs` | The physical controller through hidapi; `reset()` replugs it through sysfs |
| `src/gadget.rs` | USB gadget with the Pro Controller's IDs (usb-gadget crate) |
| `src/proxy.rs` | Forwarding between controller and Switch, on two threads |
| `src/wake.rs` | USB remote wakeup on Home, through the DWC2 registers |
| `src/priority.rs` | Real-time priority and optional CPU affinity |
| `src/dump.rs` | `Dumper` trait, `AsyncDumper` (own thread), `FileDumper`, `MultiDumper` |
| `src/stream.rs` | Frame link: `FrameStreamer` on the proxy, `receive_frames` in the studio |
| `src/replay.rs` | Replay `Action`s, loading them, and the proxy's replay port |
| `src/player.rs` | The studio's Replay panel: plays actions to the replay port |
| `src/parser.rs`, `src/keystate.rs` | Input reports parsed into buttons, sticks and IMU samples |
| `src/motion.rs` | Controller orientation from the IMU for Splatoon mode |
| `src/recorder.rs` | Session folders and `controller.bin`: start/pause/resume/stop |
| `src/video.rs` | ffmpeg capture: input list, grabber, preview and recording encoders |
| `src/audio.rs` | Capture card sound from PulseAudio, for recordings |
| `src/studio.rs` | Coordinator: sessions, `session.json`, dashboard commands, saved settings |
| `src/web.rs` | Dashboard server (warp): page, WebSocket, command API, Inkspector API |
| `src/inspect.rs` | Inkspector backend: sessions, frames, labels, delays |
| `src/objects.rs` | Object labels of the Inkspector's labeling mode: `classes.json`, `<session>/<segment>.objects.jsonl`, atomic writes, Follow's write rules |
| `src/follow.rs` | Follow: boxes carried over the next frames by AgentZero's SAM 2 tracker, proxied from a thread; starts the tracker |
| `src/cuttlefish.rs` | Cuttlefish app backend: review folders (`review.json` and the video), video bytes with ranges, yt-dlp downloads into new reviews, migration of the older flat layout, "Ask Cuttlefish" with the shared knowledge store |
| `src/knowledge.rs` | Cuttlefish's Knowledge tab: the store and embedder loaded once, search, ask, translate, glossary, import jobs, inbox uploads, overview, assets and thumbnails, document deletion |
| `src/vision.rs` | Vision app backend: detection runs on a thread, timings, stored results through our classes, dataset overview, send to labels |
| `src/predictor.rs` | Predictor app backend: `agentzero-predict` runs as a child process, stored predictions, windows of predictions and truth, agreement numbers |
| `crates/gameplay-data` | Recording format, alignment, labels, calibration; Python bindings |
| `crates/gameplay-vision` | Object detection (YOLOv8 in candle) and tracking on session video; object labels and prelabels; CLI `gameplay-vision` (see its README) |
| `crates/cuttlefish` | AI reviewer backend and CLI `cuttlefish`: knowledge store (importers, inbox, name tables, assets, embeddings, search, glossary) and `Reviewer` for the Anthropic API (see its README) |
| `web/` | Dashboard page (`index.html`, `style.css`, `app.js`, `controller3d.js`, `inspect.js`, `sketch.js` drawing layer, `label.js`, `cuttlefish.js`, `knowledge.js`, `vision.js`, `predictor.js`, `i18n.js` and `i18n-zh.js` for the language, `icons/` icon set and gallery), embedded into the binary |
| `examples/fake_proxy.rs` | Streams a synthetic controller like the proxy |
| `doc/` | Setup and dashboard write-up with screenshots, published to GitHub Pages |

## How it works

### Proxy (on the Pi)

1. `device::reset()` replugs the controller through sysfs (`authorized` 0/1).
   A controller the console knows over Bluetooth connects to it wirelessly
   when idle, and the USB handshake then stalls; the reset drops that link.
2. `ProConGadget` sets up the USB gadget (Nintendo's vendor and product IDs)
   and returns the `/dev/hidg*` device.
3. `FrameStreamer` listens on `[stream] port`; with `[dump] autostart` a
   `Recorder` also keeps a local session. Both sit behind a `MultiDumper` in an
   `AsyncDumper`, whose thread drops frames rather than block the proxy.
4. `Proxy` forwards input reports (controller → Switch) and output reports
   (Switch → controller: rumble, LEDs, subcommands) on separate threads, since
   a write to the controller blocks for about 9 ms. The proxy raises its
   priority first (nice -10, `SCHED_FIFO` as root), and threads started after
   inherit it. Each frame's `forward_us` is the time from reading a
   report to the Switch taking it; the dashboard shows it as "Proxy +x ms".
5. While a client is connected to the replay port, the latest action replaces
   (or, with `mix`, combines with) each input report before it is forwarded
   and recorded.
6. While the Switch sleeps, reports are dropped and Home signals remote wakeup
   (`wake.rs`).

The controller asks to be polled every 8 ms and only sends on that beat; do not
change its polling interval (`usbhid.jspoll`): on the Pi 4 that breaks the
output endpoint and the Switch's handshake.

### Frame link

The proxy sends an 8-byte header, then 80-byte frames (the `controller.bin`
record, `gameplay_data::frame`), and an empty frame (a heartbeat) after a second
without reports. The studio counts sequence gaps as dropped frames and keeps
the smallest `host_now - proxy_timestamp` over 10 s as the clock offset.

### Studio (on the PC)

- Frames from the link go through a `MultiDumper` to the `Recorder` and the
  dashboard's live feed.
- `Video` runs three kinds of ffmpeg: a grabber that owns the input and turns it
  into raw 1080p frames; a preview encoder (low-latency H.264 as fragmented MP4,
  one fragment per frame, played by a `<video>` element); and one recording
  encoder per video file, fed from Record on. The grabber logs each frame's
  kernel capture time (`-ts mono2abs -copyts` + `showinfo`); a recording starts
  at its first frame's capture time, and frames reach it at a constant rate. A
  queue of late frames (over 120 ms for 3 s while idle) restarts the grabber.
- `Audio` reads the `[video] audio_input` PulseAudio source all the time in
  10 ms chunks and keeps the last 2 s; a recording's sound starts at the sample
  that arrived with its first frame and goes to the encoder on fd 3, as an Opus
  track in the same file.
- `Studio` starts and stops the recorder and video together, writes
  `session.json` (including the `GameSettings` set on the dashboard) and saves
  dashboard settings to `<config>.state.json`.

### Dashboard

`src/web.rs` serves the page embedded from `web/`, a WebSocket (`/ws`: a
`state` message per input report, `status` twice a second, preview fMP4
fragments as binary), `POST /api/command` (a `studio::Command` such as
`{"action":"start"}`) and `/api/inspect/...` (see `src/inspect.rs`), `/api/cuttlefish/...` (see
`src/cuttlefish.rs` and `src/knowledge.rs`), `/api/vision/...` (see
`src/vision.rs`) and `/api/predictor/...` (see `src/predictor.rs`). Everything
that reads files, runs ffmpeg or a model is kept off the async workers
(`spawn_blocking`, or a thread of its own for jobs).

Startup does nothing slow before the server listens: the last replay file
(`Player::restore`, which keeps naming the file until it is loaded, so saved
settings keep it) and the migration of old reviews run on threads, since both
may sit on a network mount; the knowledge store, detectors and the
`agentzero-predict` options load on first use. Browsers abort requests all the
time (a video's range request when seeking, a frame it no longer needs, a
reload during a slow request); warp reports each as a connection error
(`IncompleteMessage`, connection reset, broken pipe), which the studio's logger
lowers to debug (`is_client_abort` in `src/bin/main.rs`). `scripts/run.sh`
builds first (15–45 s after a code update, over a minute after a dependency
change) and then runs the binary.

The page holds five apps switched by the hash (`#studio`, `#inspect/...`,
`#cuttlefish/...`, `#vision/...`, `#predictor/...`) without reloading; the app
links are a left rail or a top-bar switch (`data-nav`), in the order the user
dragged them into (or moved with Alt+arrows; `procon-app-order`, new apps go
last, "Reset app order" in the View menu), and the View menu sets
`data-theme`, `data-layout` and `data-nav`, remembered in `localStorage`, and
the language. `web/i18n.js` is the language layer: a dictionary per language
(English in it, Simplified Chinese in `web/i18n-zh.js`), `t(key, values)`,
and `data-i18n*` attributes on the page's elements; a change fires
`lang-change` for what scripts draw. The Cuttlefish app and its Knowledge view
use it; other apps can adopt it key by key. The Studio's preview pauses and its views stop
drawing while another app is shown. `web/controller3d.js` loads three.js from
jsdelivr and extrudes the SVG view's outline; the SVG stays as the fallback. The
input overlay (`drawInputHud` in `app.js`) is shared by the Studio's video and
the Inkspector.

`web/icons/` is the icon set, embedded whole (include_dir) and served at
`/icons/`: `app-*`, `class-*` (named after `classes.json`) and `ui-*` SVGs,
drawn by hand on a 24×24 grid with 2 px round strokes in `currentColor` and
one ink accent filled with `var(--icon-accent, currentColor)`. Each file's
drawing is `<g id="i">`, so the page uses it as
`<svg class="app-icon" viewBox="0 0 24 24"><use href="icons/app-studio.svg#i"/></svg>`
and it takes the theme's colours. `/icons/` is a gallery of every icon on each
theme. Our own doodles in the Salmon Run spirit; never Nintendo's artwork.

### Inkspector

`src/inspect.rs` reads sessions under `[inspect] root` with `gameplay-data`.
Frames are decoded by ffmpeg on request (a seek, then a short window at 360p)
and cached; labels come from `gameplay_data::align` at the requested delay; a
segment's sound is served as WebM with byte ranges. `POST
/api/inspect/delay` sets or removes a delay by hand in the calibration file.
`web/inspect.js` keeps its state in the hash.

The labeling mode (`src/objects.rs`, `web/label.js`) saves boxes frame by
frame; the scrubber marks labeled frames on one canvas (`drawMarks`). Follow
(`src/follow.rs`) sends a frame's boxes and the video path to the tracker
(`agentzero-track-serve` in AgentZero: SAM 2.1 tiny through transformers,
streaming, JSON lines per frame) from a thread and writes its boxes every ten
frames under the labeling lock. `follow_span` stops a Follow before the first
frame a person labeled; `apply_followed` skips frames labeled meanwhile and
replaces the model boxes of the followed track ids; `follow_ids` gives boxes
without an id a new one and writes it on the start frame. The page polls
`GET follow/job`; `POST follow/start` runs `[inspect] tracker_command` in its
own process group, stopped with the studio.

### Cuttlefish and its knowledge

`src/cuttlefish.rs` keeps each review as a folder, `<reviews>/<id>/review.json`
plus the video when it lives there (`video.file`): a YouTube range downloads
into a new review folder, a local file can be copied in, and a session review
points at its recording. The file name is checked to be a plain name, so a
path never leaves its folder. `Cuttlefish::migrate` moves reviews of the
older flat layout into folders at startup, with their YouTube videos from the
old download cache. A review may hold `notes` on the whole video. A YouTube
review without its title gets it (with channel and upload date) from
`yt-dlp --skip-download` on a thread, once per run, when it is listed or
opened; a save keeps those fields when the page's copy lacks them. It also
serves videos, and single thumbnails (`GET thumb`, ffmpeg, cached in memory)
for the player's neighbours strip; `src/knowledge.rs` holds
the `cuttlefish` crate's `Store` and `E5Embedder`, loaded once on first use and
shared by "Ask Cuttlefish" (`cuttlefish::review::review` over the borrowed
store, embedder and a client made per request), the Knowledge tab's search,
questions and translations, and imports. Imports use `cuttlefish::ingest`
(the same code as the CLI) with a `Sink` that writes the job's log; one runs
at a time, and the index is written every fifty documents and at the end (it
may live in a synced folder, where each write uploads it whole). The
API key is read only from `ANTHROPIC_API_KEY`; the page learns only whether it
is set.

The knowledge folder defaults to `Knowledge` next to the Inkspector's root and
may be synced: `cuttlefish::store` writes every file through a temporary file
and a rename, takes no locks, skips unreadable or conflict-copy files, drops
index entries of documents that are gone, rebuilds an index that does not read
and embeds documents it lacks (`Store::catch_up`, when the store loads). The
model, thumbnails and unpacked archives live in `cuttlefish::store::cache_dir`
(`~/.cache/procon-cuttlefish`; `~/.cache/cuttlefish` belongs to another
program). The old default `~/.local/share/cuttlefish` (`$XDG_DATA_HOME`) is
that program's folder too: at startup `cuttlefish::store::migrate` touches only
our entries there (`DATA_DIRS`, `DATA_FILES`, `models`), copies their data into
the knowledge folder, checks the copy (every file with its size) and only then
moves them into `procon-migrated-<date>.safe-to-delete/` inside it; the
overview lists such folders. `Store::open` refuses that folder. The CLI resolves
the knowledge folder like the studio from `--config` (default `./config.toml`),
else `$CUTTLEFISH_DATA`, else fails. Structured files without names in several
languages become small text documents (`tables::as_text`, under 1 MB).

The inbox (`cuttlefish::inbox`) is `<knowledge>/inbox/`. `POST
knowledge/upload?path=` streams a file into it (a bounded channel to a blocking
writer, `.name.upload` then renamed; hidden or `..` paths refused, 4 GB at
most); `routes` in `src/knowledge.rs` serves that and `GET thumb` before the JSON
routes of `src/cuttlefish.rs`. The import walks the inbox (no hidden folders,
no `node_modules`/build output), classifies each file (`inbox::classify`),
unpacks archives with `bsdtar` into the cache, and routes prose to documents
(key `inbox/<path>`, `Document::path`), structured files by family
(`tables::path_language`: `locales/*/x.json`) to `terms/<id>.json`
(`tables::build`), images to `assets.json` (`assets::link` to glossary terms by
file name). `inbox.json` remembers size, time and FNV hash per path for
dedup; `reports/` keeps the last thirty reports. `Store::load_glossary` merges
the tables into `glossary.toml` (or the seed) with `Glossary::merge`.

### Vision

`src/vision.rs` runs `gameplay-vision` on a thread: frames from ffmpeg, the
detector (kept loaded per model and device), the tracker, whose ids are copied
onto the detections they matched, and per-frame timings. The last results of a
segment are two files in `[vision] results`; "Send to labels" renames classes,
drops those not in `classes.json` and merges with the prelabel rule
(`gameplay_vision::labels::merge_model_boxes`) under the labeling mode's write
lock. The results shown go through the same renaming and keep only our classes
(the stock COCO ones behind an "experimental" toggle), and the dataset overview
counts the labeled boxes per class (by people and by models) and the frames
people labeled, against the 200 the Salmon Run detector waits for.

### Predictor

`src/predictor.rs` runs AgentZero's `agentzero-predict` (`uv run`, in
`[predictor] agentzero`) as a child process, one at a time: sessions in its
session mode, other videos with `--video`, a range with `--start-s/--end-s`
and `--cpu`, each only when the command's `--help` lists it. Its `progress
d/t` lines drive the progress bar, its last output lines stay with the job, and
CUDA running out of memory is told apart. The predictions go to
`[predictor] results/<video>/<checkpoint>/pred.jsonl` (renamed from `.part`
on success) with `run.json`. The page asks for windows of predictions (and, for
sessions, the truth through the Inkspector's alignment) and for the agreement
over a range; videos play through Cuttlefish's video endpoint.

### gameplay-data

One definition of a recording for the recorder (Rust) and the training code
(Python), so the two never read a session differently:

- `frame`: the 80-byte record; `controller`: `controller.bin` as columns
  (buttons, sticks, gyro in deg/s, accelerometer in g);
- `session`: the `session.json` model (older sessions lack some fields);
- `align`: on-screen times of video frames and controller actions per frame,
  given `video_delay_ms`;
- `labels`: per-frame labels as JSON lines, shared by ground truth and model
  predictions;
- `calibration`: `calibration.json` entries and the rule for the delay to
  apply: set by hand, else the session's own when its confidence is high or
  medium, else its setup era's.
