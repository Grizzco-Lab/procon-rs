# CLAUDE.md

- Do not add any unused packages
- Do not manually edit Cargo.toml or Cargo.lock. Only edit by calling `cargo add` or `cargo rm`. Use -F for features. Try not to include unused features.
- Call `cargo fmt` to format the code after finish editing. Use `prettier` for html.
- All the comments and docs must be in English. Chinese should never appear in the code.
- Keep it simple.
- rust has great inline doc feature (/// to struct, member, functions, and //! to the top of the file). You should leverage it as if you're really writing rust.
- follow what's there in the code; do not hallucinate.
- use core/alloc instead of std whenever possible. (I think this practice is good)

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

This is a Nintendo Switch Pro Controller HID proxy written in Rust. It forwards HID data bidirectionally between a physical Pro Controller and Nintendo Switch via USB gadget functionality on Raspberry Pi 4.

It has two binaries: `procon` (the main one, `src/bin/main.rs`, config `config.toml`) runs on a Linux host with the capture card (dashboard, ffmpeg video capture, session recording); `procon-proxy` (`src/bin/procon-proxy.rs`, config `proxy.toml`) is the USB proxy, run on the Pi (proxy + frame streaming over TCP). `cargo run --example fake_proxy` stands in for it.

## Common Commands

### Build and Run
```bash
# Build release version
cargo build --release

# Run the studio on the host
./scripts/run.sh

# Cross-compile procon-proxy, copy it to the Pi and restart it there
./scripts/deploy.sh [ssh-host]

# Build and run procon-proxy on the Pi itself
./scripts/run-proxy.sh
```

### Development
```bash
# Build for development
cargo build

# Run tests (if any exist)
cargo test

# Check code
cargo check

# Format code
cargo fmt

# Lint code
cargo clippy
```

### Configuration
- Config files: `config.toml` (studio), `proxy.toml` (USB proxy)
- Command line args: `--config <path>` to specify alternative config file
- Log levels: error, warn, info, debug, trace (set in config or RUST_LOG env var)

## Architecture

### Core Components

**Proxy (`src/proxy.rs`)**: The main orchestrator that handles bidirectional HID forwarding:
- Controller → Nintendo Switch (input reports: buttons, sticks, gyro)
- Nintendo Switch → Controller (output reports: rumble, LEDs)
- Runs at high priority for minimal latency
- Auto-reconnection on device failures

**ProController (`src/device.rs`)**: The physical controller through hidapi; reconnects as long as it takes. Do not change its polling interval (`usbhid.jspoll`): on the Pi 4 it breaks the output endpoint and the Switch's handshake

**ProConGadget (`src/gadget.rs`)**: Handles automatic USB gadget setup and cleanup with Nintendo Pro Controller device IDs

**AsyncDumper (`src/dump.rs`)**: High-performance data logging system:
- Runs in separate thread to avoid blocking proxy
- Supports file output and fan-out to several dumpers
- Backpressure handling with intelligent packet dropping

**Parser/KeyState (`src/parser.rs`, `src/keystate.rs`)**: HID input report parsing into structured controller state

**Replay (`src/replay.rs`, `src/player.rs`)**: JSON-line `Action`s, loaded from a session, `controller.bin` or `.jsonl`; the studio's Replay panel (`Player`) sends them to the proxy's replay port, and while it is connected the latest action overwrites (or, with `mix`, combines with) input reports before they are recorded and forwarded

**Remote wakeup (`src/wake.rs`)**: when the sleeping Switch stops reading reports, Home sets the DWC2 `DCTL.RmtWkUpSig` bit through `/dev/mem` (the dwc2 driver has no wakeup op)

**Proxy (`src/proxy.rs`)** forwards input and output on separate threads: a write to the controller blocks ~9 ms, and the Switch sends rumble constantly. Each frame's `forward_us` is the time from reading the report to the Switch taking it (poll POLLOUT on the gadget); the dashboard shows it as the "Proxy +x ms" chip

**Frame link (`src/stream.rs`)**: proxy-side `FrameStreamer` (TCP, header then 80-byte frames, heartbeats) and host-side `receive_frames` (sequence gaps, clock offset)

**Recorder (`src/recorder.rs`)**: Session folders `<prefix>YYYY-MM-DD_HH-MM-SS/` with `controller.bin`; start/pause/resume/stop

**Video (`src/video.rs`)**: One ffmpeg process per input: MJPEG preview on stdout, plus an encoded file while recording; stopped with SIGINT

**Audio (`src/audio.rs`)**: ffmpeg reads the `[video] audio_input` PulseAudio source continuously in 10 ms chunks; a recording's sound starts at the sample that arrived with its first frame and goes to the encoder on fd 3, as an Opus track in the same file

**Studio (`src/studio.rs`)**: Host coordinator; starts/stops recorder and video together, writes `session.json`, saves dashboard settings to `config.state.json`; each session records the game's controller settings (`GameSettings`: sensitivities scale gyro/stick into camera turns)

**Motion (`src/motion.rs`)**: Gyro + accelerometer orientation for the dashboard's Splatoon mode; Y recenters

**3D view (`web/controller3d.js`)**: three.js from jsdelivr; extrudes the SVG view's outline and reuses its theme colors. The SVG is the fallback without WebGL or the CDN

**Web dashboard (`src/web.rs`, `web/`)**: warp server with the embedded page, a WebSocket (`state` per input report, `status` once per second, preview JPEGs as binary) and `POST /api/command`

### Data Flow
1. **Initialization**: USB gadget auto-setup → Frame streamer and dumper setup → Controller connection
2. **Main Loop**: Bidirectional forwarding with timeout-based non-blocking I/O
3. **Error Recovery**: Automatic device reconnection and cleanup

### Performance Features
- Real-time process priority and optional CPU affinity (`src/priority.rs`)
- Asynchronous dumping prevents proxy latency
- Non-blocking I/O with configurable timeouts
- Intelligent backpressure handling

## Key Configuration Options

```toml
[proxy]
hidg_retry_delay_ms = 1000          # HID gadget retry delay

[dump]
autostart = false                   # Local backup session on the proxy
prefix = "/tmp/procon-"

[stream]
port = 7331                         # Studio host connects here

[replay]
port = 7332                         # JSON-line actions replace the controller's

[performance]
enable_cpu_affinity = false         # Pin to random CPU core

[logging]
level = "info"                      # Log verbosity
```

## Requirements
- Raspberry Pi 4 (or device with USB device controller)
- Linux with USB gadget support
- Root privileges for USB gadget operations
- Nintendo Switch Pro Controller (USB connection)

## Target Platform
- Primary target: `aarch64-unknown-linux-musl` (static ARM64 Linux binary, linked by rust-lld; see `scripts/deploy.sh`)
- Rust stable toolchain