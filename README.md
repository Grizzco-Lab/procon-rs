# ProCon Proxy

A Rust program that proxies Nintendo Switch Pro Controller HID data.

## Features

- Forwards HID data between Pro Controller and Nintendo Switch
- Automatic USB gadget setup
- Bidirectional communication (input/output reports)
- Optional data dumping to file or console
- Configurable via TOML file

## Requirements

- Raspberry Pi 4 (or compatible device with USB device controller)
- Linux with USB gadget support
- Nintendo Switch Pro Controller (USB connection)
- Rust

## Usage

```bash
./scripts/run.sh
```

That's it. The script builds and runs with proper permissions.

## Configuration

The proxy uses a TOML configuration file (`config.toml`) for customization:

```toml
[proxy]
# Timeout for reading from Pro Controller (milliseconds)  
controller_read_timeout_ms = 20
# Interval for logging frame count progress
frame_count_log_interval = 100
# Retry delay when HID gadget device fails to open (milliseconds)
hidg_retry_delay_ms = 1000

[dump]
# File path for binary dump output
file_path = "/tmp/procon_hid_dump.bin"

[console]
# Enable console output to terminal
enable = false

[performance]
# Enable CPU affinity pinning to random core
enable_cpu_affinity = false

[logging]
# Log level: error, warn, info, debug, trace
level = "info"
```

## How It Works

The proxy program:

1. **Automatic Setup**: Creates and configures USB gadget with Nintendo Pro Controller device IDs
2. **Controller Connection**: Connects to the physical Pro Controller via USB HID
3. **Bidirectional Forwarding**: 
   - **Input Reports**: Controller → Pi → Nintendo Switch (button presses, stick positions, gyro data)
   - **Output Reports**: Nintendo Switch → Pi → Controller (rumble commands, LED control)
4. **Data Logging**: Optional console output and binary file dumping for analysis
5. **Error Recovery**: Automatic reconnection if controller or gadget device disconnects

The result is a transparent proxy where the Nintendo Switch sees the Pi as a genuine Pro Controller while the Pi forwards all communication from the real controller.

## Project Structure

The codebase is organized into the following modules:

- **`src/bin/main.rs`** - Main executable entry point with command-line argument parsing
- **`src/config.rs`** - Configuration management using TOML format
- **`src/gadget.rs`** - USB gadget management for automatic device setup
- **`src/proxy.rs`** - Core proxy functionality for bidirectional HID forwarding
- **`src/device.rs`** - Nintendo Switch Pro Controller device connection and communication
- **`src/dump.rs`** - Data dumping functionality (console output, file logging, async processing)
- **`src/parser.rs`** - HID input report parsing into structured controller state
- **`src/keystate.rs`** - Data structures for controller state representation
- **`src/priority.rs`** - Process priority and CPU affinity management for real-time performance

### Core Components

- **`ProController`** - Manages connection and communication with the physical controller
- **`ProConGadget`** - Handles automatic USB gadget configuration and cleanup
- **`Proxy`** - Orchestrates bidirectional data forwarding between controller and Nintendo Switch
- **`AsyncDumper`** - Provides high-performance, non-blocking data logging to prevent proxy latency

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