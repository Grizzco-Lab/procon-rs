# Contributing

How the code is laid out, how the pieces fit, and how to work on it. For what
the project does and how to run it, see the [README](README.md).

## Development

```bash
cargo build --release                  # every crate for this machine
cargo test --workspace                 # every crate's unit tests
cargo clippy --workspace --all-targets
cargo fmt

cargo run -p procon-proxy --example fake_proxy [port] [--still]  # synthetic controller, no Pi needed (default port 7331, replay on the next)
./scripts/run.sh                       # Grizzco Lab with config.toml (builds -p grizzco-lab)
./scripts/deploy.sh [ssh-host]         # cross-compile, copy and restart the proxy on the Pi
./scripts/run-proxy.sh                 # build and run the proxy on the Pi itself
```

The proxy is cross-compiled for `aarch64-unknown-linux-musl`: a static binary
linked by Rust's bundled `rust-lld` (`.cargo/config.toml`), so no C cross
toolchain or Pi sysroot is needed. `rust-toolchain.toml` adds the target.
The root `Cargo.toml` is a virtual workspace, every package a crate under
`crates/`, and the proxy is a package of its own (`cargo build -p
procon-proxy`): of ours it builds only `procon-core` and `gameplay-data`, never the
lab's crates (tokio, warp, the model and knowledge crates), which it has no
use for and which need a C compiler for the target.

To work on the lab without hardware, run `fake_proxy` on a free port, point
a copy of `config.toml` at it (`[proxy] address`, and `replay_address` at the
next port, a different `[web] port`, `[video] input = "screen"`, `""` or a
recorded video file, which plays in a loop as if live) and run
`target/release/grizzco-lab --config <copy>`. Dashboard settings are saved next to
that copy (`<name>.state.json`). The fake proxy applies replayed actions like
the proxy; `--still` keeps its synthetic controller at rest, so AgentZero's
actions can be sent to it without a "person" taking over.

After changing the page's layout or styles, run the layout check against
such a test lab (Node 22 or later and Chrome; no packages):

```bash
node scripts/layout-check.mjs http://127.0.0.1:<port>   # --only joy,salmon for some themes
```

It opens every app in headless Chrome, in each theme, in English and Chinese,
at 1440 px (apps in the top bar and in the rail) and at phone width (390 px),
and measures the boxes that broke before: the Studio's 3D stage (the SVG view
without WebGL) fills its panel, the top bar is one row at 1440 px, the
Cuttlefish chat bar sits at the bottom of the window, and no app widens the
page sideways. Every view is listed; failures are repeated at the end and the
exit code is 1. `CHROME` names another Chrome binary. The profile lives in a
temporary folder and is removed afterwards.

`scripts/gadget_procon.sh` and `scripts/cleanup_gadget.sh` set up and remove
the USB gadget by hand; the proxy does this itself
(`crates/procon-proxy/src/gadget.rs`).

### Python bindings

`crates/gameplay-data` builds a Python module, `gameplay_data`, with maturin
(`pyproject.toml`, the `python` feature). AgentZero depends on it as an
editable path dependency (`../procon-rs/crates/gameplay-data`, so the crate
stays there), and `uv` rebuilds it when the Rust sources change.

## Conventions

- Comments and docs in English; use `///` and `//!` doc comments.
- Add and remove dependencies with `cargo add` / `cargo rm`, with as few
  features as needed, and no unused packages. Edit `Cargo.toml` by hand only for
  what cargo cannot do; never edit `Cargo.lock` by hand.
- Prefer `core` and `alloc` over `std` where possible.
- Run `cargo fmt` after editing Rust, and `prettier --write` for HTML (the
  dashboard in `crates/grizzco-lab/web/` and the write-up in `doc/`).
- Keep it simple.

## Layout

| Path | What |
|---|---|
| `Cargo.toml` | The workspace (virtual): every package is a crate under `crates/` |
| **`crates/procon-proxy/`** | **The USB proxy on the Pi** |
| `src/main.rs` | Binary `procon-proxy` (`proxy.toml`, read by `src/config.rs`): reset the controller, set up the gadget, stream frames, forward |
| `src/device.rs` | The physical controller through hidapi; `reset()` replugs it through sysfs |
| `src/gadget.rs` | USB gadget with the Pro Controller's IDs (usb-gadget crate) |
| `src/proxy.rs` | Forwarding between controller and Switch, on two threads |
| `src/wake.rs` | USB remote wakeup on Home, through the DWC2 registers |
| `src/priority.rs` | Real-time priority and optional CPU affinity |
| `examples/fake_proxy.rs` | Streams a synthetic controller like the proxy (or one at rest, `--still`) and applies replayed actions |
| **`crates/procon-core/`** | **What the proxy and the lab share** |
| `src/dump.rs` | `Dumper` trait, `AsyncDumper` (own thread), `FileDumper`, `MultiDumper` |
| `src/stream.rs` | Frame link: `FrameStreamer` on the proxy, `receive_frames` in the lab |
| `src/replay.rs` | Replay `Action`s, loading them, and the proxy's replay port |
| `src/recorder.rs` | Session folders and `controller.bin`: start/pause/resume/stop |
| `src/config.rs` | The TOML loader and the `[logging]` section both config files have |
| **`crates/grizzco-lab/`** | **Grizzco Lab on the PC, one module per app** |
| `src/main.rs` | Binary `grizzco-lab` (`config.toml`, read by `src/config.rs`): frame receiver, video, recorder, replay player, dashboard, and the exit on Ctrl-C |
| `src/exit.rs` | The exit's step under way and the blocking work in flight, named (`exit::blocking` runs every request's file, ffmpeg or model work), so a slow exit says what it waits for |
| `src/web.rs` | Dashboard server (warp): page, WebSocket, command API, and every app's routes under its prefix |
| `src/studio.rs` | The Studio app's coordinator: sessions, `session.json`, dashboard commands, saved settings, technique markers (open span, mark last N s, undo; each with its kind and item id, a technique or Lean's key of a weapon or special; `web/techniques.js` is their panel, the weapons and specials from `GET /api/cuttlefish/game-items`) |
| `src/studio/player.rs` | The Studio's Replay panel: plays actions to the replay port |
| `src/studio/parser.rs`, `src/studio/keystate.rs` | Input reports parsed into buttons, sticks and IMU samples |
| `src/studio/motion.rs` | Controller orientation from the IMU for Splatoon mode |
| `src/studio/video.rs`, `src/studio/v4l2.rs` | ffmpeg capture: input list, grabber, preview and recording encoders; a YUYV capture card read directly (`v4l2.rs`) |
| `src/studio/audio.rs` | Capture card sound from PulseAudio, for recordings |
| `src/inspect.rs` | Inkspector backend: sessions, frames, labels, delays, technique markers (read, replace, every session's) |
| `src/inspect/objects.rs` | Object labels of the Inkspector's labeling mode: `classes.json`, `<session>/<segment>.objects.jsonl`, atomic writes, Follow's write rules |
| `src/inspect/follow.rs` | Follow: boxes carried over the next frames by AgentZero's SAM 2 tracker, proxied from a thread; starts the tracker |
| `src/cuttlefish.rs` | Cuttlefish app backend: review folders (`review.json` with the chat, and the video, optional), video bytes with ranges, yt-dlp downloads into new or existing reviews, migration of the older flat layout, the chat endpoint over the shared knowledge store; pictures of other sites fetched once into the local cache (Gungee's stage maps, Lean's weapon and special icons) and Lean's Salmon Run weapons and specials for the Techniques panel |
| `src/cuttlefish/knowledge.rs` | Cuttlefish's Knowledge view: the store and embedder loaded once (the chat's retrieval too), search, glossary lookups and `Knowledge::translate` for the Translate view, import jobs, inbox uploads, overview, assets and thumbnails, document deletion |
| `src/cuttlefish/pedia.rs` | Cuttlefish's Overfishing Pedia: the terms in scope with sections, games and facets (`cuttlefish::pedia`), their #vod-review mentions searched once and cached until the corpus or the names change, entries with quotes, fact cards, notes and deep questions; `GET source`, the context of a cited source or a quote for the page's source popover (`web/source.js`) |
| `src/vision.rs` | Vision app backend: detection runs on a thread, timings, stored results through our classes, dataset overview, send to labels |
| `src/vision/detector.rs` | The Salmon Run detector's client: AgentZero's `agentzero-detect-serve`, its health, runs streamed as JSON lines, and starting it |
| `src/predictor.rs` | Predictor app backend: `agentzero-predict` runs as a child process, stored predictions, windows of predictions and truth, agreement numbers |
| `src/predictor/online.rs` | The Predictor's online mode: `agentzero-play --json` on a paced video or the live capture's piped frames, the loop's latency, and the bot (`Bot`) that plays the Switch through the replay port with a person's input taking over |
| `src/pipeline.rs` | Pipeline app backend: the experiment queue file (read, reordered under its lock, what each runner takes next), both machines (this host, and the win11 VM's GPU, CPU and memory from its runner's file) sampled on a thread, each entry's processes (with its main one), CPU, progress (ended, stalled) and what its log says it runs, the timeline |
| `web/` | Dashboard page (`index.html`, `style.css`, `app.js`, `controller3d.js`, `player.js` the video player of the apps, `inspect.js`, `sketch.js` drawing layer, `label.js`, `cuttlefish.js`, `knowledge.js`, `translate.js`, `vision.js`, `predictor.js`, `pipeline.js`, `i18n.js` and `i18n-zh.js` for the language, `icons/` icon set and gallery), embedded into the binary |
| **Libraries and CLIs** | |
| `crates/gameplay-data` | Recording format, alignment, labels, calibration, the camera turn from AgentZero's fits; Python bindings |
| `crates/gameplay-vision` | Object detection (YOLOv8 in candle) and tracking on session video; object labels and prelabels; CLI `gameplay-vision` (see its README) |
| `crates/cuttlefish` | AI reviewer backend and CLI `cuttlefish`: knowledge store (importers, inbox, name tables, assets, embeddings, search, glossary) and `Reviewer` for the Anthropic API (see its README) |
| `doc/` | Setup and dashboard write-up with screenshots (`index.html`), and the project's story (`story.html`; its videos rendered from the page's canvas scenes by `story/render.mjs`), published to GitHub Pages |

## How it works

### Proxy (on the Pi)

`crates/procon-proxy`, with the dumpers, the recorder, the link and the replay
port of `crates/procon-core`:

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
   (or, with `mix`, combines with: buttons on either, each stick and the gyro
   the controller's while a person moves it past a deadzone) each input
   report before it is forwarded and recorded.
6. While the Switch sleeps, reports are dropped and Home signals remote wakeup
   (`wake.rs`).

The controller asks to be polled every 8 ms and only sends on that beat; do not
change its polling interval (`usbhid.jspoll`): on the Pi 4 that breaks the
output endpoint and the Switch's handshake.

### Frame link

`crates/procon-core/src/stream.rs`: the proxy sends an 8-byte header, then 80-byte
frames (the `controller.bin` record, `gameplay_data::frame`), and an empty
frame (a heartbeat) after a second without reports. The lab counts sequence
gaps as dropped frames and keeps the smallest `host_now - proxy_timestamp`
over 10 s as the clock offset.

### Grizzco Lab (on the PC)

Paths in this and the following sections are in `crates/grizzco-lab/`.

- Frames from the link go through a `MultiDumper` to the `Recorder` and the
  dashboard's live feed.
- `Video` runs three kinds of ffmpeg: a grabber that owns the input and turns it
  into raw 1080p frames; a preview encoder (low-latency H.264 as fragmented MP4,
  one fragment per frame, played by a `<video>` element); and one recording
  encoder per video file, fed from Record on. The grabber logs each frame's
  kernel capture time (`-ts mono2abs -copyts` + `showinfo`); a recording starts
  at its first frame's capture time, and frames reach it at a constant rate. A
  queue of late frames (over 120 ms for 3 s while idle) restarts the grabber.
  A capture card in YUYV is read by the lab itself instead
  (`src/studio/v4l2.rs`, `[video] v4l2_direct`): memory-mapped, four buffers, each
  frame taken as the kernel has it, with the kernel's timestamp; `pump` in
  `src/studio/video.rs` hands the newest frame to the live policy, then puts every
  frame on the constant rate itself (`ConstantRate`, the fps filter's rule
  without its wait for the next frame) and writes it into a converter
  ffmpeg whose 1080p frames go on as the grabber's. ffmpeg's v4l2 input asks
  for 256 buffers, so frames it falls behind on wait in the kernel, and each
  frame passes four of its threads before a pipe; on the Elgato 4K X at
  1080p60 the policy's hand-off fell from 26.9 ms to 17.8 (median), most of
  what is left being the card's own transfer of a frame over one frame
  period. `pump` also counts the card's frames (`CaptureCounts`): every
  frame, the corrupted or short ones it skips and the ones the driver
  dropped (gaps in its sequence numbers); the frame before stands in for
  either on the constant rate. The status has the counts since the reader
  started and since the file being recorded started (the Studio's chip),
  each file's own go into `session.json` (`capture`), and the log has the
  first loss at once and then, while losses go on, a line a minute with
  their rate (`LossLog`), so a burst at the start reads apart from a
  steady loss. When the device cannot be opened this way, ffmpeg reads it.
  Otherwise the grabber's second output (fd 3) is the live policy's frames
  (see the Predictor below). Every raw output runs `-threads 1`, since
  ffmpeg's rawvideo encoder is frame threaded and held a frame or two back,
  and every pipe of frames is grown to 1 MiB.
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
`src/cuttlefish.rs` and `src/cuttlefish/knowledge.rs`), `/api/vision/...` (see
`src/vision.rs`), `/api/predictor/...` (see `src/predictor.rs`) and
`/api/pipeline/...` (see `src/pipeline.rs`). Everything
that reads files, runs ffmpeg or a model is kept off the async workers
(`exit::blocking`, tokio's `spawn_blocking` with the work named, or a
thread of its own for jobs). The status never waits on the sessions'
folder, which may be a network mount where every folder not listed lately
is a round trip: a thread of its own counts the earlier sessions' sizes (16
folders at once, every 10 s) and reads the disk's free space, and each
status tells what it read last. Likewise the Studio's weapons and specials
(`GET /api/cuttlefish/game-items`) come at once from a copy kept in the
local cache, made again on a thread when their files change.

A click never freezes the page. The page shares six connections to the lab
(HTTP/1.1) between every app, so every `fetch` takes its turn in one queue
in `web/app.js`: at most four at a time, changes first, then the open
app's data, then its pictures (frames and thumbnails load through it too,
`imageUrl`, a list's thumbnails as they come into view, `lazyImages`); a
GET belongs to the app open when it was asked, and leaving that app
aborts it, so the next app never waits for the one left (a loader treats
`isAbort(error)` as nothing and runs again when shown). The lab
answers at once: slow data is made on a thread and kept (`Kept` in
`src/cuttlefish/kept.rs` for the glossary and the game items, the Knowledge
view's panels, the reviews list, the Pedia), `refreshing` while made again,
`202` while it never was, which the queue asks again every second; every
request answered in more than 300 ms is logged at debug level (`Slow
request:`). On the main thread, no long task of ours runs in an app switch:
long lists are built as one HTML string, laid out once they come into view
(`content-visibility: auto`), and the 3D controller builds after the
Studio's first paint, step by step, only while the Studio is open.
CLAUDE.md's Dashboard section has the same rules in short.

Ctrl-C stops the lab in steps, each logged (`exit::step`): AgentZero (the
controller first), the recording (ffmpeg finishes the file,
`session.json` gets its end), the tracker, detector and prediction the page
started (SIGTERM to each group, SIGKILL after 5 s: `end_group`), the
capture, then the requests' blocking work, which gets 2 s before the
runtime goes without waiting. A tokio runtime dropped waits for every
blocking task, as long as they take: a read on the Dropbox mount with a
cold cache or an answer from the model can hold the exit for minutes. Tokio's
SIGINT handler replaces the default for the rest of the process, so the
lab listens for a second Ctrl-C itself, which exits at once (code 130),
naming the step and the work it cut short (`exit::doing`).

Startup does nothing slow before the server listens: the last replay file
(`Player::restore`, which keeps naming the file until it is loaded, so saved
settings keep it) and the migration of old reviews run on threads, since both
may sit on a network mount; the knowledge store, detectors and the
`agentzero-predict` options load on first use. Browsers abort requests all the
time (a video's range request when seeking, a frame it no longer needs, a
reload during a slow request); warp reports each as a connection error
(`IncompleteMessage`, connection reset, broken pipe), which the lab's logger
lowers to debug (`is_client_abort` in `src/main.rs`). `scripts/run.sh`
builds first (15–45 s after a code update, over a minute after a dependency
change) and then runs the binary.

The page holds six apps, each at its own path (`/studio`, `/inspect/...`,
`/cuttlefish/...`, `/vision/...`, `/predictor/...`, `/pipeline`), switched without
reloading. `web/app.js` routes with the History API: `appUrl(app, state)`
builds a path from the keys an app reads (what is open goes in the path, a
frame, a time or an option in the query), `routeOf` reads it back and
`routeApp` shows that app and fires `app-route` with the state; `navigate`
pushes a history entry and `replaceRoute` replaces it for changes as frequent
as a frame while scrubbing. A document-level click handler takes plain left
clicks on links into the apps, so the links are real paths (middle-click and
copying work); back and forward (`popstate`) route again. Old `#app/...` links,
also those kept as an app's last view in `localStorage`, become paths
(`urlOfHash`, `storedView`). The server answers `/` and every app path with the
page (`APPS` in `src/web.rs`) after its own routes, so every asset, icon and
API URL in the page starts with `/`. The app
links are a dock-like left rail (apps centred in the height above the
tools, the language switch and View at its foot) or a segmented switch in the top bar (`data-nav`; other
apps' names become tooltips where the bar runs short), in the order the user
dragged them into (or moved with Alt+arrows; `procon-app-order`, new apps go
last). The rail is compact by default (icons on 50 px tiles, names as
tooltips beside them after a short delay, the open app on a tinted tile with
a mark on the rail's edge, icons lifting on hover, nothing that filters or
blurs over the live video) or expanded to icons with names (`data-rail`,
`procon-rail`; the View menu's Rail row or the chevron at the rail's foot);
each theme sets the rail's surface, tile and mark through `--rail-*` tokens. The top bar keeps three looks apart:
navigation (the switch), status (passive indicators, a dot and a word, the
details in their title; the open app's first, then the lab's) and actions
(buttons: language, View). It stays one row down to about 1060 px of page
width (healthy link indicators drop to their dot first when an app adds its
own status), then the status takes a row of its own. The View menu sets
`data-theme`, `data-layout` (Auto or Phone), `data-nav` and `data-rail`
(Compact or Expanded), remembered in `localStorage`, and the language. `web/i18n.js` is the language layer: a dictionary per language
(English in it, Simplified Chinese in `web/i18n-zh.js`), `t(key, values)`,
and `data-i18n*` attributes on the page's elements; a change fires
`lang-change` for what scripts draw; an entry may be a list (the chat's
example messages). The Cuttlefish app with its Translate and Knowledge views
uses it; other apps can adopt it key by key. The Studio's preview pauses and its views stop
drawing while another app is shown. `web/controller3d.js` loads three.js from
jsdelivr and extrudes the SVG view's outline; the SVG stays as the fallback. The
input overlay (`drawInputHud` in `app.js`) is shared by the Studio's video and
the player's Minimal overlay.

### The player

`web/player.js` is the one video player of the Inkspector, Cuttlefish, Vision
and Predictor, made from the Inkspector's: `new Player({screen, controls,
scrubber, strip, table, ...})` builds the picture, the overlays, the transport
and the scrubber into the app's elements, and `open(source, at)` shows a
source. Two kinds of source sit behind the same interface: exact frames
(`{frames, fps, frame(n)}`, the Inkspector's endpoint; drawn on a canvas,
prefetched, played by a paced loop or by the segment's audio clock) and a
`<video>` (`{video, fps}`; frames followed with `requestVideoFrameCallback`,
a seek lands in the middle of the frame). Either may add `thumb(n)` for the
neighbours strip, `labels(n)` (a promise of `[truth, prediction]`) for the
overlays and the table, `sound` and `title`. The player owns the keys (Space,
arrows, Shift for ten, Home/End, G; only while the app sets `enabled`, never
in inputs, and the events are consumed so the page never scrolls), the marks
on the scrubber (`setMarks`; ticks and ranges by kind, colored by
`--mark-<kind>`; a click snaps to the nearest tick) and the strip (every frame,
or every `seconds` on a grid). Apps draw their own layers by appending to the
screen (the labeling mode's Sketch, Vision's boxes, Cuttlefish's danmaku) and
follow the player through `onFrame`, `onSeek`, `onPlay` and `onMark`.

`web/icons/` is the icon set, embedded whole (include_dir) and served at
`/icons/`: `app-*`, `class-*` (named after `classes.json`) and `ui-*` SVGs,
drawn by hand on a 24×24 grid with 2 px round strokes in `currentColor` and
one ink accent filled with `var(--icon-accent, currentColor)`. Each file's
drawing is `<g id="i">`, so the page uses it as
`<svg class="app-icon" viewBox="0 0 24 24"><use href="/icons/app-studio.svg#i"/></svg>`
and it takes the theme's colours. `/icons/` is a gallery of every icon on each
theme. Our own doodles in the Salmon Run spirit; never Nintendo's artwork.

### Inkspector

`src/inspect.rs` reads sessions under `[inspect] root` with `gameplay-data`.
Frames are decoded by ffmpeg on request (a seek, then a short window at 360p)
and cached; labels come from `gameplay_data::align` at the requested delay,
the truth with its camera turn (`gameplay_data::turn`) when AgentZero's
`sessions.json` next to the calibration file has the session's fit; a
segment's sound is served as WebM with byte ranges. `POST
/api/inspect/delay` sets or removes a delay by hand in the calibration file.
`web/inspect.js` opens the segment in the player and keeps its state in the
URL (`/inspect/<session>?seg=&n=&delay=&pred=`).

The labeling mode (`src/inspect/objects.rs`, `web/label.js`) saves boxes frame by
frame; the scrubber marks labeled frames on one canvas (`drawMarks`). Follow
(`src/inspect/follow.rs`) sends a frame's boxes and the video path to the tracker
(`agentzero-track-serve` in AgentZero: SAM 2.1 tiny through transformers,
streaming, JSON lines per frame) from a thread and writes its boxes every ten
frames under the labeling lock. `follow_span` is the count up to the ends of
the segment; `apply_followed` decides per object: it replaces the model boxes
of the followed track ids and adds a followed box unless a person's box of
the same class overlaps it (`COVERED_IOU`), never touching people's boxes or a
line a person left empty; Accept up to here (Shift+A) saves several frames in
one write (`save_frames`, `frames` in `POST /api/inspect/objects`); `follow_ids` gives boxes
without an id a new one and writes it on the start frame. The page polls
`GET follow/job`; `POST follow/start` runs `[inspect] tracker_command` in its
own process group, stopped with the lab.

### Cuttlefish and its knowledge

`src/cuttlefish.rs` keeps each review as a folder, `<reviews>/<id>/review.json`
plus the video when it lives there (`video.file`): a YouTube range downloads
into a new review folder (or, with `review`, into a chat's review that has no
video yet), a local file can be copied in, and a session review points at its
recording. The file name is checked to be a plain name, so a path never leaves
its folder. `Cuttlefish::migrate` moves reviews of the older flat layout into
folders at startup, with their YouTube videos from the old download cache. A
review may hold `notes` on the whole video and `messages`, its chat with
Cuttlefish (role, text, the moment or range a user message was asked with,
the sources an answer cites, the ids of the comments it added, time); a review
started from the chat has no `video` until one is attached. A YouTube review
without its title gets it (with channel and upload date) from `yt-dlp
--skip-download` on a thread, once per run, when it is listed or opened; a
save keeps those fields when the page's copy lacks them. It also serves videos,
and single thumbnails (`GET thumb`, ffmpeg, cached in memory) for the player's
neighbours strip.

The chat: `POST chat` takes the message, the earlier turns (the page owns the
review and sends its `messages`), the video and the moment or range it is
about; `Cuttlefish::video_context` extracts the frames with ffmpeg and picks
the review's comments near them, and `cuttlefish::review::chat` retrieves
knowledge for the message plus the last user turns, sends the conversation
with the frames and a JSON schema, and answers text (citing `[S1]`, naming
moments as times), sources and timed comments. The page appends both turns to
the review and saves it (Cuttlefish's turn even after the review was left),
and adds his comments as its own. `src/cuttlefish/knowledge.rs` holds the `cuttlefish`
crate's `Store` and `E5Embedder`, loaded once on first use and shared by the
chat (`Knowledge::chat` over the borrowed store, embedder and a client made
per request), the Knowledge view's search, and imports; `GET knowledge/model`
tells the page whether the key is set without loading the store. The Translate
view (`web/translate.js`) posts `translate` with `{text, target}`:
`Knowledge::translate` reads the glossary (no store, no model) for the terms
the text uses, or the entry of a bare term with its name in the target
language, and with a client the crate's `review::translate` for a sentence
and `review::explain` for a term; `Cuttlefish::record_translation` appends
the answer, with an id and time, to `<reviews>/translations.jsonl` (the
last 500 kept, rewritten whole through `write_atomic`), which `GET
translations` lists and `DELETE translations` removes. The page shows a bare
term's entry from `GET knowledge/glossary` before the model answers. Slang
(`cuttlefish::slang`): a term has official names (`Term::forms`) and aliases
(`Alias`: text, language, note, source, status); the user's are in
`<knowledge>/glossary-user.toml` (`UserGlossary`, applied last by
`Store::load_glossary`, with its new terms, `UserTerm`), written by `POST
knowledge/slang/add|edit|delete|undo|term|move` under `Knowledge::slang`'s
lock; `GET knowledge/terms?q=` serves the page's term picker. `POST
knowledge/slang/suggest` plans batches of unread community text
(`slang::plan`; `dry_run` answers the counts; `all` reads everything) and
runs them as a job (`Knowledge::start_job`, shared with imports) through the
translator's client, three at once (`slang::run`), each batch's aliases and
new terms (`slang::parse_candidates`) saved as it ends, the sure ones
approved at once with auto-apply (`[cuttlefish] slang_auto_apply`,
`slang_threshold`). `POST knowledge/slang/term-edit` edits a term
(`UserGlossary::edit_term`: a new term in place, a glossary term as an
`[[override]]` of the user file applied on every load; `term-reset` drops
one). Expert notes (`cuttlefish::notes`, `<knowledge>/notes/<id>.md`, source
kind `expert-note`, weight 1.3): `GET knowledge/notes`, `POST
knowledge/notes/save` (the file written first, then indexed at once under the
store lock, or through the same loaded store while a job of the lab holds
it; a step failing after the file only warns; with `eval: {file, id}` the
eval answer it corrects is marked wrong with its id) and `notes/delete`; the
page's editor (`web/knowledge.js`, the dialog `#cf-note-dialog` and its form
`#cf-note-editor`, `window.cuttlefishNotes.edit`) is opened by the chat's
**Correct / add to memory** and by the deep eval's answers, and keeps what
is typed as a draft in localStorage until the note is saved. The deep
question bank (`cuttlefish::questions`, `GET knowledge/questions`) feeds the
chat's chips and the eval (`cuttlefish::deep_eval`: `POST knowledge/eval/deep`
runs it as a job when the player starts it, `GET knowledge/eval[?file=]`
lists and reads `<knowledge>/eval/deep-<date>.jsonl`, each answer with the
backend, model and effort that gave it, `POST knowledge/eval/mark` records a
verdict on the first answer or one asked again and the note made, `POST
knowledge/eval/ask` asks a question again over the store as it is now). Imports use `cuttlefish::ingest`
(the same code as the CLI) with a `Sink` that writes the job's log; one runs
at a time, and the index is written every fifty documents and at the end (it
may live in a synced folder, where each write uploads it whole). The
API key is read only from `ANTHROPIC_API_KEY`; with none, `auto` falls back
to the Claude Code CLI (`cuttlefish::claude_cli`, on the user's subscription);
the page learns only which backend answers.

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
the knowledge folder like the lab from `--config` (default `./config.toml`),
else `$CUTTLEFISH_DATA`, else fails. Structured files without names in several
languages become small text documents (`tables::as_text`, under 1 MB).

The inbox (`cuttlefish::inbox`) is `<knowledge>/inbox/`. `POST
knowledge/upload?path=` streams a file into it (a bounded channel to a blocking
writer, `.name.upload` then renamed; hidden or `..` paths refused, 4 GB at
most); `routes` in `src/cuttlefish/knowledge.rs` serves that and `GET thumb` before the JSON
routes of `src/cuttlefish.rs`. The import walks the inbox (no hidden folders,
no `node_modules`/build output), classifies each file (`inbox::classify`),
unpacks archives with `bsdtar` into the cache, and routes prose to documents
(key `inbox/<path>`, `Document::path`), structured files by family
(`tables::path_language`: `locales/*/x.json`) to `terms/<id>.json`
(`tables::build`), images to `assets.json` (`assets::link` to glossary terms by
file name). `inbox.json` remembers size, time and FNV hash per path for
dedup; `reports/` keeps the last thirty reports. `Store::load_glossary` merges
the tables into `glossary.toml` (or the seed) with `Glossary::merge`, then
adds the user's approved aliases (`glossary-user.toml`).

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

The Salmon Run detector (model `salmon`) runs outside the lab, in
AgentZero's `agentzero-detect-serve`; `src/vision/detector.rs` is its client, like
Follow's for the tracker: `GET /health` (checkpoint, training summary, device,
free GPU memory, busy) for `GET /api/vision/detector`, with the checkpoint's
modification time as `saved_ms`; `POST /api/vision/detector/start` runs
`[vision] detector_command` in its own process group, stopped with the lab.
A run posts `/detect` and reads its JSON lines (`start`, one `frame` per frame
with boxes in the label format and decode/network/total ms, `end` or `error`)
into the same job, tracker and results file as a candle run; Cancel drops the
connection, which stops the service; its 409 (one request at a time) becomes a
"busy" error. Before a run that may use the GPU, the sessions' root is scanned
for a `session.json` without `stopped_at_unix_ms` (a recording under way), as
AgentZero's own commands do.

### Predictor

`src/predictor.rs` runs AgentZero's `agentzero-predict` (`uv run`, in
`[predictor] agentzero`) as a child process, one at a time: sessions in its
session mode, other videos with `--video`, a range with `--start-s/--end-s`
and `--cpu`, each only when the command's `--help` lists it. Its `progress
d/t` lines drive the progress bar, its last output lines stay with the job, and
CUDA running out of memory is told apart. The predictions go to
`[predictor] results/<video>/<checkpoint>/pred.jsonl` (renamed from `.part`
on success) with `run.json`. The command runs in its own process group, since
`uv run` starts Python as its child and a signal to `uv` alone would leave
Python predicting: Cancel sends the group SIGTERM, then SIGKILL after five
seconds, and keeps nothing; a run under way stops with the lab. The page
asks for windows of predictions (and, for sessions, the truth through the
Inkspector's alignment, with the camera turn where AgentZero fitted the
session) and for the agreement over a range; videos play through
Cuttlefish's video endpoint.

The online mode (`src/predictor/online.rs`) runs AgentZero's policy
(`runs/policy/*/best.pt`) with `uv run agentzero-play --dry-run --json`, in
its own process group like a run: on a video with `--realtime` (paced at
30 fps, frames skipped while the model is busy, as live), or on the live
capture with `--shared-frames`. For the latter the lab hands over the
frames the capture card delivers, as it delivers them (YUYV): the newest,
30 a second, split off before the constant rate (which holds every frame
until the next one arrives), from its own V4L2 reader or from ffmpeg's
second output (`src/studio/video.rs`, `pipe:3`), each with its capture time. The
lab writes each into shared memory (`SharedFrames`: a memfd
`agentzero-play` inherits as fd 3, a ring of four slots) and announces it
with a 40-byte notice on its stdin (number, slot, times, size); AgentZero
takes the newest notice's frame whenever the model is free, with no ffmpeg
and no reader thread of its own, copies it into pinned memory and scales it
on its GPU to 640 x 360 RGB as training's frames were (4:2:0 chroma,
anti-aliased bilinear, BT.601 limited range; within 1.6 levels on average
of torchcodec's own frame of a recording). Every JSON line comes back with
the lab's number of the frame seen and the moments the policy took it,
had it on the model's device and had the action, on `CLOCK_MONOTONIC` like
the lab's, so each stage is timed on one clock: the grabber (capture to
the frame in hand), the pipe into the lab (ffmpeg's only), shared
memory, the wait for the model, the upload and scaling, the model and the
send. Where the time went before: ffmpeg's rawvideo encoder is frame
threaded, which held one or two frames back at every raw output (all now
`-threads 1`); pipes stayed at 64 KiB, since asking for more than
`/proc/sys/fs/pipe-max-size` (1 MiB) fails (all now grown to 1 MiB); the
`fps` filter held each frame for the next; and the policy's own ffmpeg
dropped the first piped frame (`-fflags nobuffer`), so every frame it
reported was one later than the one timed. Each action goes to the
dashboard's WebSocket as an `agent` message (the page draws it over the
Studio's live preview, lent to the Predictor by
`lendScreen` in `app.js`); a video's actions are kept as labels (for frame
seen + lead, the frame whose input they predict) and stored as a run
`policy-<checkpoint>` when it ends. `agentzero-play` never sends anything:
the `Bot` does (the `Studio` holds it, beside the Replay panel's `Player`),
only after `POST online/play` (a confirmation on the page each time, for a
set time), writing each action to the replay port with `mix`, so a person
corrects it live and it never pauses: the proxy ORs the buttons and takes
each stick and the gyro from the controller while it is pushed past
`STICK_DEADZONE` (300 raw units from 2048, past where a resting stick reads
its calibrated centre) or turned faster than `GYRO_DEADZONE_DPS` (10 °/s),
else from the line (`crates/procon-core/src/replay.rs`; a person's turn replaces the bot's
rather than adding to it, since both raw readings carry the controller's
rest bias and a policy aiming by the picture would double a shared turn).
Before a line goes out, a `Limiter` holds its buttons to the page's
`Limits` (`src/predictor/online/limits.rs`, kept in the state file): the
d-pad and the special blocked unless unticked, Home and Capture never, and
each button's presses at least `1 / max_hz` apart and `min_hold_ms` long
(7.7 a second and 40 ms by default: 1.1 times the 7 a second a person keeps
up; the module docs cite the tapping studies, the records and the owner's
sessions); the stop's neutral line (buttons only, so mixed it is the
controller alone) lets go at once. The bot is also a dumper of the proxy's
frames, which measures a person tapping ZR (`Tapping`: 10 s from the first
press, the fastest six in a row, 1.1 times that offered as the cap). A
watchdog thread ends sending at the time's end, when the policy stalls
(500 ms), when no dashboard page has been connected for 5 s, when the Replay
panel plays and when the proxy's frames stop; the lab's exit and Ctrl-C
stop it first. The bot's status goes out with every `status` message on
`/ws`, so every app shows Stop bot while it plays and Esc stops it anywhere.

### Pipeline

`src/pipeline.rs` follows the machine and the experiment queue, and writes
nothing but the queue's order. The queue is a JSON file the agents keep
(AgentZero's `runs/queue.json`, `[pipeline] queue`), written by AgentZero's
`agentzero-queue`: each entry one step of an experiment with its status,
`priority`, the entries it waits for (`after`), where it can run
(`device`: `cpu`, `gpu:linux`, `gpu:win11` or `gpu`, either), processes
(`pgid`, `pid` or `match`, a piece of its command line), run folder, log,
times, result and next step. It is read again when its size or time
changes, each entry on its own, so an entry that does not read is reported
and kept, never lost. Which entry each runner takes next follows the
helper's rules (`next_for`: the first queued one that fits its device and
waits for nothing; the VM's only with a `command`).

The second GPU is the win11 VM's: AgentZero's `agentzero-win11 run` marks
the entries it takes `host: win11`, copies their logs and metrics into the
same paths here every minute and writes the VM's GPU (with its power limit,
fan, clock and P-state), CPU (`cpu`: `percent`, `cores`) and memory
(`memory`: `used`, `total`) to `win11/gpu.json` beside the queue every 10 s;
a runner of before writes the GPU alone, and the page says its runner does
not read the rest yet. An entry of another machine is never looked for
among this host's processes; it runs while that file, fresh (a minute),
names it as the runner's job. A stale file makes the VM unknown, never 0,
and its entries "no word" rather than "no process".

The sampling runs in a process of its own, `grizzco-lab sample`
(`pipeline::run_sampler`), so the timeline has no hole while the lab is
stopped. One runs at a time: it holds an exclusive flock on
`pipeline-sampler.lock` in the local cache, with its pid in it. The lab
starts one whenever none holds the lock (at most every 30 s), detached in a
session of its own with `setsid`, so the terminal's Ctrl-C and the lab's
exit leave it running; its output goes to `pipeline-sampler.log`. It exits
when its binary is rebuilt (its mtime changes), and the lab starts the new
build. The lab samples nothing itself: it reads the sampler's snapshot,
`pipeline-now.json` (rewritten after every sample: the GPUs, their
processes, CPU, memory, each entry's processes, progress and what its
log says it runs, the disks),
and follows the history file's new lines (by inode and offset; all of it
again once the compaction replaced the file). A snapshot older than 30 s
says nothing of now: the state's GPU and processes are left out and its
`gpu_error` names the sampler's log.

The sampler samples every 5 s: the GPU with `nvidia-smi` (one query for the
GPU, one for its compute processes), the VM's GPU, CPU and memory from
its file, the CPU,
load, memory and every process from `/proc`, which processes belong to
which entry (running entries claim theirs first), each live entry's CPU,
RSS and GPU memory and its main process (the one holding the most GPU
memory, else the busiest, else the newest, by its short name), and when
each was seen running; the lab keeps 12 hours
of samples in memory for the timeline, averaged down to 720 points for a
window, each sample with the entries seen running and the cores each took.
The sampler also watches the disks (`Storage`): the Proxmox host's ZFS pools
(`[pipeline] storage_host`, default `pve`: every VM's disk is a thin zvol
on `rpool`, and a full pool hangs the host and both VMs), read with `ssh
<host> zpool list -Hp ...` once a minute by a `PoolProbe` the sampler looks
at without waiting (given up after 30 s, so a hung host never stalls the
samples), this host's `/` (statvfs) and the VM's `C:` from its runner's
file (`disk`). `pool_level` has AgentZero's storage guard's thresholds
(low under 300 GB free or at 85 % used, critical under 150 GB); a reading
older than 5 minutes, or failing, makes the level unknown. The `state`
answer has the disks and the level; the `/ws` status carries them while
`rpool` is not fine (`Pipeline::storage_alert`), for the top bar's chip in
every app, and the page shows a banner.

Each sample is appended to `pipeline-gpu.jsonl` in the local cache
(`~/.cache/procon-cuttlefish`), which the sampler compacts at its start and
every hour; the log keeps a week, older than 12 hours one row a minute.
Its lines keep the first version's layout (eight numbers, then the running
ids) and add the VM's GPU, the load, each entry's cores and each disk's
free space in GB, and the VM's CPU and memory after it, so every version
reads the others' lines. A live entry's log is read once a sample, for
its progress and for what it does (`log_activity`): the step its job
script started last (the agents' `== start <name> (step N of the job)
<date> <time>`, until its `== end <name>`), whether a counter came after
that start (else the counter is an earlier step's), and its last line.
Progress comes from the run
folder's `metrics.jsonl`, read as it grows (whole lines only, from the
start again when the file shrinks), with the total from `args.json`, else
from the last `N/M` in the log of a live entry; the ETA comes from the
steps the sampler saw over the last ten minutes, else from `s_per_step`. A
run whose trainer wrote its closing rows (the policy's `thresholds`, the
IDM's `val-tuned`) has ended, early when short of its `steps`; a live
entry whose step has not moved for five minutes does something else (an
evaluation after its training, say); neither gets an ETA, and the page
shows what runs instead of a bar. `POST order` rewrites the waiting entries'
priorities (their count down to 1, top first) under the lock the helper
takes (`<queue>.lock`, an exclusive `flock`) and replaces the file
atomically, with the fields in the helper's order, so the two write the same
bytes. `web/pipeline.js` polls `state` (with the samples since the last)
while the app is shown, redraws only what changed (a focused handle, an open
log and a chart keep their place), and reorders by pointer events, so a
touch drags as a mouse does. It draws both machines from one model
(`machinesOf`: title, GPU, CPU, memory and disk readings, their rows in the
samples, the runner of its GPU), so the two panels are alike; their head,
vitals and processes share rows (`grid-template-rows: subgrid`, which a
size container would break: the vitals box is the container instead), and
the running cards come last, where the columns may differ in length. Each
machine's disk tile reads `storage.root` (`/`) or the VM's `disk` (`C:`);
the Proxmox host's pools are a strip of their own under the machines
(`renderHost`, the watched pool first, its tile's edge in its level's
colour), beside main's banner over the page and chip in the top bar. A
card's phase (`phaseOf`) is counting, working (no counter of its own: a
step its script started after the counter's last line, its training over,
or none at all; named by the step or the main process), stalled or ended.
The timeline is a chart per GPU (`drawChart`) with the same lanes' height
on both, so their rows line up, and one crosshair time for both.

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
  medium, else its setup era's;
- `turn`: the camera turn per frame as AgentZero defines it (`x = a * yaw +
  b * (stick_x - 2100)`, `y = c * pitch`, pixels per frame at 640 x 360,
  the gyro's rest bias removed), with each session's `a`, `b`, `c` read
  from AgentZero's `sessions.json`, never fitted here.
