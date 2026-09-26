# ProCon Proxy

A Rust program that proxies Nintendo Switch Pro Controller HID data and records
it together with the console's video, for building training datasets.

> [!TIP]
> **[See the setup guide and dashboard tour →](https://htmlpreview.github.io/?https://github.com/Grizzco-Lab/procon-rs/blob/main/doc/index.html)**
>
> The hardware you need, how it is wired, and what the studio does, with
> screenshots. Source: [doc/index.html](doc/index.html).

![The studio dashboard](doc/demo.png)

## Features

- Forwards HID data between Pro Controller and Nintendo Switch
- Automatic USB gadget setup
- Bidirectional communication (input/output reports)
- Streams timestamped controller frames to a studio host over the network
- Studio host: web dashboard, video capture (capture card or screen) and
  recording sessions that keep controller data and video in sync
- Configurable via TOML files

## Architecture

```
Pro Controller ──USB──> Raspberry Pi (procon-proxy) ──USB gadget──> Nintendo Switch
                              │ TCP :7331, 80-byte timestamped frames
                              v
Switch HDMI ──capture card──> Linux host (procon) ──> dashboard :8090
                                         └──> <prefix>YYYY-MM-DD_HH-MM-SS/
```

- **USB proxy (`procon-proxy`, on the Pi)**: proxies the controller, stamps every report with the time
  and a sequence number, and streams it to whoever connects on `[stream] port`.
  It sends a heartbeat each second when the controller is quiet.
- **Studio (`procon`, the main binary, on the host)**: connects to the proxy, shows the dashboard, captures
  video with ffmpeg and records sessions.

## Requirements

- Raspberry Pi 4 (or compatible device with USB device controller)
- Linux with USB gadget support
- Nintendo Switch Pro Controller (USB connection)
- A Linux host with `ffmpeg` for the studio (NVENC is used by default)
- Rust

## Usage

Everything runs from the host. Deploy the proxy to the Pi (cross-compiles
`procon-proxy`, copies it with `proxy.toml` to `~/procon` on the Pi, and restarts it):

```bash
./scripts/deploy.sh [ssh-host]   # default host: pi4
```

Then start the studio, with the proxy's address in `config.toml`:

```bash
./scripts/run.sh
```

and open `http://<host>:8090`. To build on the Pi itself instead of deploying,
run `./scripts/run-proxy.sh` there.

### Replaying actions

To check what a model predicts, play its actions to the Switch from the
dashboard's Replay panel: load a session folder, a `controller.bin` or a
`.jsonl` of actions, then Play. The studio sends them to the proxy's
`[replay]` port (7332, `replay_address` in `config.toml`); while it plays, the
Switch gets the replayed input instead of the controller's (and that is what
gets recorded), and the controller takes over again on Stop or at the end.
"Mix with the controller" combines the two instead: buttons pressed on either
count, and each stick and the gyro take whichever moves more.

A `.jsonl` file has one action per line:

```json
{"t_ms": 40, "buttons": ["zr"], "left_stick": [2048, 3500], "right_stick": [1200, 2048], "gyro": [0, -300, 12]}
```

Fields left out keep the controller's own values. Sticks are raw 12-bit
(center ≈ 2048), `gyro`/`accel` raw IMU units; see `src/replay.rs`. A model
can also connect to the replay port itself and stream lines (without `t_ms`)
as it predicts them.

## Dashboard

One page with two apps, switched without reloading (the preview, capture and
recording keep running): **Studio** (`#studio`, below) and **Inspector**
(`#inspect`). The app links sit in a left rail that widens on hover, or in the
top bar (**View → Nav: Side / Top**; phones always use the top bar). The
status chips stay in the top bar in both apps.

- **View**: Studio, Joy or Telemetry style, plus a Phone layout (single column,
  also used automatically on narrow screens), and the Nav placement. Remembered
  per browser.
- **Record / Pause / Stop**: a session is a folder named from the path prefix
  and the start time, e.g. prefix `/data/procon/mk8-` records into
  `/data/procon/mk8-2026-09-24_21-40-05/`. The prefix's folder must exist.
- **Video input**: the screen or any V4L2 device, such as the Elgato 4K X. A
  capture card can only be opened by one program, so close OBS first.
- **Video quality**: recorded size (1080p, 720p, 540p, 360p) and frame rate (60
  to 10 fps), to keep files small. The live preview is H.264 at 1080p60 by
  default (a few Mbit/s, played by a `<video>` element with about 0.2 s delay);
  tick "Preview at recording quality" to see exactly what gets recorded.
- **Data**: controller and video write rates per second and per hour, this
  session's size, all sessions in the save folder, dropped frames, free disk
  space (with the time left at the current rate) and free memory. Updates twice
  a second.
- **Controller**: a 3D model (three.js, loaded from a CDN; the flat drawing is
  the fallback). **Splatoon mode** tracks the controller's real pose from the
  gyro and accelerometer, Y recenters it, and a sensitivity slider (-5 to +5)
  scales the motion from 1/4x to 4x.

The path prefix and video input are saved in `config.state.json` next to the
config, so they survive restarts.

### Inspector

Checks recorded sessions frame by frame: whether the controller labels line up
with the picture, and later a model's predictions against them.

- **Sessions**: every session under `[inspect] root` (by default the recording
  prefix's folder) with its start, duration, segments, video size and rate,
  sound, game settings and calibrated delay.
- **A segment**: the frame at 360p with its labels drawn by the same input
  overlay as the live video, a scrubber, play/pause at 0.25x to 1x, the three
  frames on each side, and a table of their labels (buttons, sticks, gyro
  degrees over the frame). Keys: Space play/pause, ←/→ one frame (Shift: ten),
  R a random frame where a button changes or the gyro turns (Shift+R: any), G go
  to a frame number or time.
- **Delay**: the `video_delay_ms` box starts at the session's calibrated delay
  from AgentZero's `calibration.json` (`[inspect] calibration`), when its
  confidence is high or medium; change it to check the alignment by eye.
- **Predictions**: a labels `.jsonl` path on the host shows a model's labels
  under the truth, differences in red.
- The URL keeps the view (`#inspect/s=<session>&seg=<file>&n=<frame>&delay=<ms>`).

Labels come from `crates/gameplay-data`, the same code AgentZero trains with.
Frames are decoded by ffmpeg on request (a seek, then a short window), so only
the part of a video being looked at is read.

### Session folder

| File | Contents |
|---|---|
| `controller.bin` | 80-byte frames: Unix ms (u64 LE), report size (u8), sequence number (u32 LE), µs from the proxy reading the report to the Switch taking it (u16 LE, 0 if unknown), 1 padding byte, 64 report bytes |
| `video-01.mkv`, `video-02.mkv`, … | One file per stretch between pauses: H.264 video, plus an Opus sound track (48 kHz stereo) when "Record sound" is on |
| `session.json` | Start/stop times, each video file's first-frame Unix ms (and first sound sample's, `audio_start_unix_ms`), the proxy's clock offset and dropped frames, and `game_settings` (Splatoon 3 motion/stick sensitivity, motion controls, invert), set in the Recording panel |

To line up the data: frames in a video file come at a constant rate, so frame
`n` was captured at its segment's `start_unix_ms` plus `n / video.fps` seconds.
`start_unix_ms` is when the capture card delivered that first frame to the kernel
(sessions before this change used its arrival at the studio, which could be a few
hundred ms later); the game's own delay from input to picture still comes on top; a controller frame's
host time is its proxy timestamp plus `proxy.clock_offset_ms`. The sound track starts at
the sample that arrived with the first frame (shifted by `[video] audio_offset_ms`), so in
the file both tracks start at 0; `audio_start_unix_ms` says when that sample arrived.

To work on the studio without a Pi, stream a synthetic controller and point
`[proxy] address` at `localhost:7331`:

```bash
cargo run --example fake_proxy
```

## Configuration

`proxy.toml` on the Pi:

```toml
[proxy]
# Retry delay when HID gadget device fails to open (milliseconds)
hidg_retry_delay_ms = 1000

[dump]
# Local backup: record one session from launch until exit, besides what the studio records
autostart = false
# Path prefix of that session folder (<prefix>YYYY-MM-DD_HH-MM-SS/controller.bin)
prefix = "/tmp/procon-"

[stream]
# TCP port the studio host connects to for live frames
port = 7331

[replay]
# TCP port for JSON-line actions that replace the controller's while a client is connected
port = 7332

[performance]
# Enable CPU affinity pinning to random core
enable_cpu_affinity = false

[logging]
# Log level: error, warn, info, debug, trace
level = "info"
```

`config.toml` on the host sets the proxy's address, dashboard port, default path
prefix and the ffmpeg capture and encoder options; see the comments in the file.
An optional `[inspect]` section sets the Inspector's `root` (the folder of
session folders) and `calibration` (AgentZero's `calibration.json`, by default
`../AgentZero/calibration.json` next to the config).

## How It Works

The proxy program:

1. **Automatic Setup**: Creates and configures USB gadget with Nintendo Pro Controller device IDs
2. **Controller Connection**: Connects to the physical Pro Controller via USB HID
3. **Bidirectional Forwarding**: 
   - **Input Reports**: Controller → Pi → Nintendo Switch (button presses, stick positions, gyro data)
   - **Output Reports**: Nintendo Switch → Pi → Controller (rumble commands, LED control)
4. **Streaming**: Every input report goes to the studio host with its timestamp, and optionally to the console
5. **Error Recovery**: Automatic reconnection if controller or gadget device disconnects

The result is a transparent proxy where the Nintendo Switch sees the Pi as a genuine Pro Controller while the Pi forwards all communication from the real controller.

## Project Structure

The codebase is organized into the following modules:

- **`src/bin/main.rs`** - Main executable (`procon`): studio with dashboard, video and recording
- **`src/bin/procon-proxy.rs`** - USB proxy executable (`procon-proxy`): proxy and frame streaming
- **`src/replay.rs`** - Action format, loading replay files, and the proxy's replay port
- **`src/player.rs`** - Replay panel: plays loaded actions to the proxy
- **`src/wake.rs`** - USB remote wakeup, so Home wakes a sleeping Switch
- **`src/motion.rs`** - Controller orientation from the IMU for Splatoon mode
- **`src/config.rs`** - Configuration management using TOML format
- **`src/gadget.rs`** - USB gadget management for automatic device setup
- **`src/proxy.rs`** - Core proxy functionality for bidirectional HID forwarding
- **`src/device.rs`** - Nintendo Switch Pro Controller device connection and communication
- **`src/dump.rs`** - Dumpers (file, async, fan-out to several); the frame format comes from `gameplay-data`
- **`src/stream.rs`** - Frame link: proxy-side TCP streamer and studio-side receiver
- **`src/recorder.rs`** - Session folders and the controller file, with start/pause/resume/stop
- **`src/video.rs`** - ffmpeg capture: input list, live preview and recorded segments
- **`src/studio.rs`** - Host coordinator: sessions, `session.json`, saved dashboard settings
- **`src/audio.rs`** - Capture card sound: always read from PulseAudio, the last 2 s kept, streamed into recordings from their first frame
- **`src/web.rs`** - Dashboard server: static page, WebSocket live feed and preview, command API
- **`src/inspect.rs`** - Inspector app backend: session list, frames decoded by ffmpeg, labels via `gameplay-data`
- **`crates/gameplay-data`** - Recording format (80-byte frames, `session.json`), per-frame alignment of controller input to video, labels and calibration; shared with AgentZero's Python training code
- **`web/`** - Dashboard page (`index.html`, `style.css`, `app.js`, `controller3d.js`, `inspect.js`), embedded into the binary
- **`examples/fake_proxy.rs`** - Streams a synthetic controller like the proxy, no hardware needed
- **`doc/`** - Setup and dashboard write-up with screenshots
- **`src/parser.rs`** - HID input report parsing into structured controller state
- **`src/keystate.rs`** - Data structures for controller state representation
- **`src/priority.rs`** - Process priority and CPU affinity management for real-time performance

### Core Components

- **`ProController`** - Manages connection and communication with the physical controller
- **`ProConGadget`** - Handles automatic USB gadget configuration and cleanup
- **`Proxy`** - Orchestrates bidirectional data forwarding between controller and Nintendo Switch
- **`AsyncDumper`** - Provides high-performance, non-blocking data logging to prevent proxy latency
- **`FrameStreamer`** - Proxy side of the frame link; a dumper that sends frames to studio hosts
- **`Recorder`** - Controller file of a session, controlled from the dashboard
- **`Video`** - ffmpeg process owning the video input
- **`Studio`** - Starts and stops controller and video recording together

## Performance Features

- **Real-time Priority**: Optional high process priority for minimal latency
- **CPU Affinity**: Optional CPU core pinning for consistent performance  
- **Asynchronous Dumping**: Data logging runs in separate thread to avoid blocking proxy
- **Non-blocking I/O**: All device operations use timeouts to prevent hanging
- **Backpressure Handling**: Intelligent packet dropping when logging falls behind

## Troubleshooting

- **"Failed to setup USB gadget"**: Ensure you're running with root privileges (`sudo`)
- **"No USB device controller found"**: Verify your device supports USB gadget mode
- **"Pro Controller not found"**: Check USB connection and device permissions
- **High CPU usage**: Try enabling CPU affinity in the configuration
- **Switch asleep**: the proxy logs "Switch stopped taking input" and drops
  reports until it wakes. Home signals USB remote wakeup (the gadget advertises
  it, and `src/wake.rs` drives the Pi 4's DWC2 controller directly, since its
  Linux driver cannot); the log says whether the bus was suspended 