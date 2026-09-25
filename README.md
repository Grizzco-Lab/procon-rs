# ProCon Proxy

A Rust program that proxies Nintendo Switch Pro Controller HID data and records
it together with the console's video, for building training datasets.

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
Pro Controller ──USB──> Raspberry Pi (procon-pi) ──USB gadget──> Nintendo Switch
                              │ TCP :7331, 80-byte timestamped frames
                              v
Switch HDMI ──capture card──> Linux host (procon) ──> dashboard :8090
                                         └──> <prefix>YYYY-MM-DD_HH-MM-SS/
```

- **Pi (`procon-pi`)**: proxies the controller, stamps every report with the time
  and a sequence number, and streams it to whoever connects on `[stream] port`.
  It sends a heartbeat each second when the controller is quiet.
- **Host (`procon`, the main binary)**: connects to the Pi, shows the dashboard, captures
  video with ffmpeg and records sessions.

## Requirements

- Raspberry Pi 4 (or compatible device with USB device controller)
- Linux with USB gadget support
- Nintendo Switch Pro Controller (USB connection)
- A Linux host with `ffmpeg` for the studio (NVENC is used by default)
- Rust

## Usage

Everything runs from the host. Deploy the proxy to the Pi (cross-compiles
`procon-pi`, copies it with `pi.toml` to `~/procon` on the Pi, and restarts it):

```bash
./scripts/deploy.sh [ssh-host]   # default host: pi4
```

Then start the studio, with the Pi's address in `config.toml`:

```bash
./scripts/run.sh
```

and open `http://<host>:8090`. To build on the Pi itself instead of deploying,
run `./scripts/run-pi.sh` there.

## Dashboard

- **View**: Studio, Joy or Telemetry style, plus a Phone layout (single column,
  also used automatically on narrow screens). Remembered per browser.
- **Record / Pause / Stop**: a session is a folder named from the path prefix
  and the start time, e.g. prefix `/data/procon/mk8-` records into
  `/data/procon/mk8-2026-09-24_21-40-05/`. The prefix's folder must exist.
- **Video input**: the screen or any V4L2 device, such as the Elgato 4K X. A
  capture card can only be opened by one program, so close OBS first.
- **Data**: controller and video sizes, write rate, dropped frames, free disk
  space (with the time left at the current rate) and free memory.

The path prefix and video input are saved in `config.state.json` next to the
config, so they survive restarts.

### Session folder

| File | Contents |
|---|---|
| `controller.bin` | 80-byte frames: Unix ms (u64 LE), report size (u8), sequence number (u32 LE), 3 padding bytes, 64 report bytes |
| `video-01.mkv`, `video-02.mkv`, … | One file per stretch between pauses |
| `session.json` | Start/stop times, each video file's first-frame Unix ms, the Pi clock offset and dropped frames |

To line up the data: a video frame's wall-clock time is its segment's
`start_unix_ms` plus the frame's timestamp in the file; a controller frame's
host time is its Pi timestamp plus `pi.clock_offset_ms`.

To work on the studio without a Pi, stream a synthetic controller and point
`[pi] address` at `localhost:7331`:

```bash
cargo run --example fake_pi
```

## Configuration

`pi.toml` on the Pi:

```toml
[proxy]
# Timeout for reading from Pro Controller (milliseconds)
controller_read_timeout_ms = 20
# Interval for logging frame count progress
frame_count_log_interval = 100
# Retry delay when HID gadget device fails to open (milliseconds)
hidg_retry_delay_ms = 1000

[dump]
# Local backup: record one session from launch until exit, besides what the studio records
autostart = false
# Path prefix of that session folder (<prefix>YYYY-MM-DD_HH-MM-SS/controller.bin)
prefix = "/tmp/procon-"

[console]
# Enable console output to terminal
enable = false

[stream]
# TCP port the studio host connects to for live frames
port = 7331

[performance]
# Enable CPU affinity pinning to random core
enable_cpu_affinity = false

[logging]
# Log level: error, warn, info, debug, trace
level = "info"
```

`config.toml` on the host sets the Pi address, dashboard port, default path
prefix and the ffmpeg capture and encoder options; see the comments in the file.

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
- **`src/bin/procon-pi.rs`** - Pi executable (`procon-pi`): proxy and frame streaming
- **`src/motion.rs`** - Controller orientation from the IMU for Splatoon mode
- **`src/config.rs`** - Configuration management using TOML format
- **`src/gadget.rs`** - USB gadget management for automatic device setup
- **`src/proxy.rs`** - Core proxy functionality for bidirectional HID forwarding
- **`src/device.rs`** - Nintendo Switch Pro Controller device connection and communication
- **`src/dump.rs`** - Data dumping functionality (console output, file logging, async processing)
- **`src/stream.rs`** - Frame link: Pi-side TCP streamer and host-side receiver
- **`src/recorder.rs`** - Session folders and the controller file, with start/pause/resume/stop
- **`src/video.rs`** - ffmpeg capture: input list, live preview and recorded segments
- **`src/studio.rs`** - Host coordinator: sessions, `session.json`, saved dashboard settings
- **`src/web.rs`** - Dashboard server: static page, WebSocket live feed and preview, command API
- **`web/`** - Dashboard page (`index.html`, `style.css`, `app.js`), embedded into the binary
- **`examples/fake_pi.rs`** - Streams a synthetic controller like the Pi, no hardware needed
- **`src/parser.rs`** - HID input report parsing into structured controller state
- **`src/keystate.rs`** - Data structures for controller state representation
- **`src/priority.rs`** - Process priority and CPU affinity management for real-time performance

### Core Components

- **`ProController`** - Manages connection and communication with the physical controller
- **`ProConGadget`** - Handles automatic USB gadget configuration and cleanup
- **`Proxy`** - Orchestrates bidirectional data forwarding between controller and Nintendo Switch
- **`AsyncDumper`** - Provides high-performance, non-blocking data logging to prevent proxy latency
- **`FrameStreamer`** - Pi side of the frame link; a dumper that sends frames to studio hosts
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