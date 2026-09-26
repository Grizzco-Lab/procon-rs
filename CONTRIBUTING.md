# Contributing

How the code is laid out, how the pieces fit, and how to work on it. For what
the project does and how to run it, see the [README](README.md).

## Development

```bash
cargo build --release                  # both binaries for this machine
cargo test --workspace                 # procon and gameplay-data unit tests
cargo clippy --workspace
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
| `src/objects.rs` | Object labels of the Inkspector's labeling mode: `classes.json`, `<session>/<segment>.objects.jsonl`, atomic writes |
| `src/cuttlefish.rs` | Cuttlefish app backend: review files, video bytes with ranges, yt-dlp downloads, AI endpoint (501 until connected) |
| `crates/gameplay-data` | Recording format, alignment, labels, calibration; Python bindings |
| `web/` | Dashboard page (`index.html`, `style.css`, `app.js`, `controller3d.js`, `inspect.js`, `sketch.js` drawing layer, `label.js`, `cuttlefish.js`), embedded into the binary |
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
`{"action":"start"}`) and `/api/inspect/...` (see `src/inspect.rs`).

The page holds two apps switched by the hash (`#studio`, `#inspect/...`)
without reloading; the app links are a left rail or a top-bar switch
(`data-nav`), and the View menu sets `data-theme`, `data-layout` and `data-nav`,
remembered in `localStorage`. The Studio's preview pauses and its views stop
drawing while another app is shown. `web/controller3d.js` loads three.js from
jsdelivr and extrudes the SVG view's outline; the SVG stays as the fallback. The
input overlay (`drawInputHud` in `app.js`) is shared by the Studio's video and
the Inkspector.

### Inkspector

`src/inspect.rs` reads sessions under `[inspect] root` with `gameplay-data`.
Frames are decoded by ffmpeg on request (a seek, then a short window at 360p)
and cached; labels come from `gameplay_data::align` at the requested delay; a
segment's sound is served as WebM with byte ranges. `POST
/api/inspect/delay` sets or removes a delay by hand in the calibration file.
`web/inspect.js` keeps its state in the hash.

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
