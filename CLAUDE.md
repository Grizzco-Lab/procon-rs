# CLAUDE.md

- Do not add any unused packages
- Add and remove dependencies with `cargo add` / `cargo rm` (-F for features; no unused features). Editing Cargo.toml by hand is fine for what cargo cannot do (feature groups, workspace settings, cleanup). Never edit Cargo.lock by hand.
- Call `cargo fmt` to format the code after finish editing. Use `prettier` for html.
- All the comments and docs must be in English. Chinese should never appear in the code.
- Keep it simple.
- rust has great inline doc feature (/// to struct, member, functions, and //! to the top of the file). You should leverage it as if you're really writing rust.
- follow what's there in the code; do not hallucinate.
- use core/alloc instead of std whenever possible. (I think this practice is good)

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Records Nintendo Switch gameplay for training datasets: Pro Controller input, timestamped, next to the console's video and sound. The README is the user guide; CONTRIBUTING.md has the same architecture in more words; `doc/index.html` is the illustrated write-up (screenshots in `doc/images/`, hero image `doc/demo.png`).

- `procon-proxy` (`src/bin/procon-proxy.rs`, config `proxy.toml`): USB proxy on a Raspberry Pi 4 between the Pro Controller and the Switch (USB gadget); streams timestamped frames over TCP (`[stream] port`, 7331) and takes replayed actions (`[replay] port`, 7332); resets the controller at start.
- `procon` (`src/bin/main.rs`, config `config.toml`): the studio on the Linux host with the capture card. Web dashboard with two apps: Studio (live preview, controller view incl. 3D, recording with sound and game settings, replay, data, motion) and Inkspector (recorded sessions frame by frame: labels, delays, sound, overlays, predictions).
- `crates/gameplay-data`: recording format, per-frame alignment, labels, calibration; Python bindings (`python` feature, maturin) used by the AgentZero training project (`../AgentZero`).
- `crates/gameplay-vision`: object detection (YOLOv8 in candle, CPU; `cuda` feature) and SORT-like tracking on session video, the object label files shared with the labeling tool (`<annotations>/<session>/<segment>.objects.jsonl`, `classes.json`) and prelabeling; CLI `gameplay-vision` (`detect`, `track`, `prelabel`, `render`). Its README has the plan toward Salmon Run detection and 3D placement.
- `crates/cuttlefish`: the AI reviewer's backend and CLI: source importers (polite crawler, MediaWiki, yt-dlp, Discord export or bot), chunks with multilingual-e5-small embeddings (candle), a flat vector index, a glossary, and `Reviewer` (retrieval + Anthropic Messages API with frames). Data folder outside the repo; keys only from `ANTHROPIC_API_KEY` / `DISCORD_BOT_TOKEN`. See its README.
- `cargo run --example fake_proxy [port]` stands in for the proxy.

## Common Commands

```bash
cargo build --release                 # both binaries
cargo test --workspace                # procon, gameplay-data, gameplay-vision unit tests
cargo clippy --workspace
cargo fmt                             # Rust; `prettier --write` for web/*.html and doc/index.html
./scripts/run.sh                      # studio with config.toml
./scripts/deploy.sh [ssh-host]        # cross-compile procon-proxy (aarch64-unknown-linux-musl, rust-lld), copy to the Pi, restart
./scripts/run-proxy.sh                # build and run procon-proxy on the Pi itself
cargo run --example fake_proxy 7397   # synthetic controller for a test studio
```

Both binaries take `--config <path>`; the studio saves dashboard settings to `<config>.state.json` next to it. Log level: `[logging] level` (error, warn, info, debug, trace).

Testing the studio: never touch a studio the user is running (port 8090 by default). Run a copy of the config with its own `[web] port`, `[proxy] address` pointing at `fake_proxy`, and `[video] input = "screen"` or `""`.

## Architecture

**Proxy side**
- `src/device.rs`: the physical controller through hidapi; waits for it as long as it takes. `device::reset()` replugs it through sysfs (`authorized` 0/1) at proxy start: a controller the console knows over Bluetooth connects wirelessly when idle, which stalls the USB handshake (Switch stops after `subcommand 03`). Do not change its polling interval (`usbhid.jspoll`): on the Pi 4 it breaks the output endpoint and the Switch's handshake.
- `src/gadget.rs`: USB gadget (usb-gadget crate) with the Pro Controller's IDs; returns the `/dev/hidg*` path.
- `src/proxy.rs`: input (controller → Switch) and output (Switch → controller: rumble, LEDs, subcommands) on separate threads; a write to the controller blocks ~9 ms and the Switch sends rumble constantly. Each frame's `forward_us` is the time from reading the report to the Switch taking it (poll POLLOUT on the gadget); the dashboard shows it as the "Proxy +x ms" chip.
- `src/wake.rs`: while the Switch sleeps, Home restarts the DWC2 clock (`PCGCTL.StopPclk`, which also freezes `DSTS`) and sets `DCTL.RmtWkUpSig` through `/dev/mem`, as the driver's `dwc2_gadget_exit_clock_gating` does. The Switch 2 ignores it; the original Switch is untested.
- `src/priority.rs`: nice -10, `SCHED_FIFO` 50 as root, optional CPU affinity (`[performance]`).
- `src/dump.rs`: `Dumper` trait, `AsyncDumper` (own thread, drops on backpressure), `FileDumper`, `MultiDumper`; frames are `gameplay_data::frame::Frame`.
- `src/replay.rs`: JSON-line `Action`s, loaded from a session, `controller.bin` or `.jsonl`; the proxy's replay port applies the latest action to input reports (or, with `mix`, combines them) before they are recorded and forwarded.

**Link**: `src/stream.rs`: proxy-side `FrameStreamer` (TCP, 8-byte header then 80-byte frames, heartbeats after 1 s without reports) and studio-side `receive_frames` (sequence gaps as dropped frames, clock offset as the minimum `host_now - proxy_ts` over 10 s).

**Studio side**
- `src/recorder.rs`: session folders `<prefix>YYYY-MM-DD_HH-MM-SS/` with `controller.bin`; start/pause/resume/stop.
- `src/video.rs`: ffmpeg grabber (owns the input, raw 1080p frames; restarted only by choosing another input), preview encoder (low-latency H.264 as fragmented MP4, one fragment per frame, played by a `<video>`), one recording encoder per file (constant rate, keyframe every second). The grabber logs each frame's kernel capture time (`-ts mono2abs -copyts` + `showinfo`); recordings start at the first frame's capture time; a queue of late frames (>120 ms for 3 s while idle) restarts the grabber.
- `src/audio.rs`: ffmpeg reads the `[video] audio_input` PulseAudio source continuously in 10 ms chunks (last 2 s kept); a recording's sound starts at the sample that arrived with its first frame and goes to the encoder on fd 3, as an Opus track in the same file.
- `src/studio.rs`: coordinator; `Command`s from the dashboard, starts/stops recorder and video together, writes `session.json` (with `GameSettings`: Splatoon 3 sensitivities, which scale gyro/stick into camera turns), saves settings to the state file.
- `src/player.rs`: the Replay panel; sends loaded actions to the proxy's replay port at their `t_ms`.
- `src/motion.rs`: gyro + accelerometer orientation for Splatoon mode; Y recenters.
- `src/web.rs`: warp server with the page embedded from `web/`, WebSocket `/ws` (`state` per input report, `status` twice a second, preview fMP4 as binary), `POST /api/command`, `/api/inspect/...`.
- `src/inspect.rs` + `web/inspect.js`: Inkspector. Sessions under `[inspect] root`, frames decoded by ffmpeg in short cached windows at 360p, labels via `gameplay-data`, sound as WebM with ranges, delays from `[inspect] calibration` (AgentZero's `calibration.json`); `POST /api/inspect/delay` sets/removes a delay by hand. State in the hash (`#inspect/s=&seg=&n=&delay=&pred=`).
- `src/objects.rs` + `web/label.js`: Inkspector labeling mode (Label toggle, L): object boxes per frame in `[inspect] annotations` (default `Annotations` next to the root): `classes.json` and `<session>/<segment stem>.objects.jsonl` (`{"frame", "boxes": [{"class", "x", "y", "w", "h", "id"?, "by": "user"|"model", "score"?}]}`, top-left and size as 0–1 fractions); `POST /api/inspect/objects` saves a frame atomically and keeps model boxes the page never loaded.
- `src/cuttlefish.rs` + `web/cuttlefish.js`: Cuttlefish, the VOD reviewer. Videos: a session segment, a local file or a YouTube range (yt-dlp into `[cuttlefish] cache`); comments at times/ranges with drawings; reviews as JSON in `[cuttlefish] reviews`; `POST /api/cuttlefish/ai` sends the range's frames (ffmpeg, 2 fps, 720p) and nearby comments to `cuttlefish::review::Reviewer` (knowledge from `[cuttlefish] knowledge`) and returns its comments; 501 while it cannot start (key from `ANTHROPIC_API_KEY` only). `web/sketch.js` is the drawing layer both use (shapes in 0–1 coordinates).

**Dashboard (`web/`)**: one page, two apps switched by hash (`#studio`, `#inspect/...`) without reloading; app links as a left rail or a top-bar switch (`data-nav`); the View popover sets `data-theme` (studio, joy, telemetry), `data-layout` (phone) and `data-nav`, stored in `localStorage` (`procon-*`). The Studio's preview pauses and its views stop drawing while another app is shown or the tab is hidden. `web/controller3d.js`: three.js from jsdelivr, extrudes the SVG view's outline and reuses its theme colors; the SVG is the fallback without WebGL or the CDN. The input overlay (`drawInputHud` in `app.js`) draws sticks, buttons and turn rates over the Studio preview (held back by the preview delay) and the Inkspector frame.

**gameplay-data (`crates/gameplay-data`)**: `frame` (80-byte record), `controller` (`controller.bin` as columns), `session` (`session.json` model; old sessions lack fields), `align` (frame `n` at `start_unix_ms + n * 1000 / fps - video_delay_ms`; actions per frame), `labels` (JSON lines for truth and predictions), `calibration` (delay applied: manual > session with high/medium confidence > setup era), `python` (bindings).

## Target Platform
- Proxy: `aarch64-unknown-linux-musl` (static ARM64 binary, linked by rust-lld; see `.cargo/config.toml`, `scripts/deploy.sh`), run as root on the Pi.
- Studio: a Linux host with ffmpeg (NVENC by default), PulseAudio and a V4L2 capture card.
