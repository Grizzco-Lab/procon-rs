# CLAUDE.md

- Do not add any unused packages
- Do not manually edit Cargo.toml or Cargo.lock. Only edit by calling `cargo add` or `cargo rm`. Use -F for features. Try not to include unused features.
- Call `cargo fmt` to format the code after finish editing.
- All the comments and docs must be in English. Chinese should never appear in the code.
- Keep it simple.
- rust has great inline doc feature (/// to struct, member, functions, and //! to the top of the file). You should leverage it as if you're really writing rust.
- follow what's there in the code; do not hallucinate.
- use core/alloc instead of std whenever possible. (I think this practice is good)

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

This is a Nintendo Switch Pro Controller HID proxy written in Rust. It forwards HID data bidirectionally between a physical Pro Controller and Nintendo Switch via USB gadget functionality on Raspberry Pi 4.

## Common Commands

### Build and Run
```bash
# Build release version
cargo build --release

# Run with proper permissions (recommended)
./scripts/run.sh

# Manual run with logging
sudo RUST_LOG=info ./target/release/procon
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
- Primary config file: `config.toml`
- Command line args: `--config <path>` to specify alternative config file
- Log levels: error, warn, info, debug, trace (set in config or RUST_LOG env var)

## Architecture

### Core Components

**Proxy (`src/proxy.rs`)**: The main orchestrator that handles bidirectional HID forwarding:
- Controller → Nintendo Switch (input reports: buttons, sticks, gyro)
- Nintendo Switch → Controller (output reports: rumble, LEDs)
- Runs at high priority for minimal latency
- Auto-reconnection on device failures

**ProController (`src/device.rs`)**: Manages physical controller connection via USB HID

**ProConGadget (`src/gadget.rs`)**: Handles automatic USB gadget setup and cleanup with Nintendo Pro Controller device IDs

**AsyncDumper (`src/dump.rs`)**: High-performance data logging system:
- Runs in separate thread to avoid blocking proxy
- Supports file and console output
- Backpressure handling with intelligent packet dropping

**Parser/KeyState (`src/parser.rs`, `src/keystate.rs`)**: HID input report parsing into structured controller state

### Data Flow
1. **Initialization**: USB gadget auto-setup → Controller connection → Dumper setup
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
controller_read_timeout_ms = 20      # Controller read timeout
frame_count_log_interval = 100       # Frame counting log frequency
hidg_retry_delay_ms = 1000          # HID gadget retry delay

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
- Primary target: `aarch64-unknown-linux-gnu` (ARM64 Linux)
- Rust stable toolchain