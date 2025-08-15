# Splabot - Nintendo Switch Pro Controller HID Proxy

A Rust program that acts as a proxy between a Nintendo Switch Pro Controller and a Nintendo Switch, forwarding HID input data in real-time.

## Features

- Real-time HID data forwarding from Pro Controller to Nintendo Switch
- Captures and forwards all button inputs (A, B, X, Y, L, R, ZL, ZR, +, -, Home, Capture, etc.)
- Forwards analog stick positions (left and right sticks)
- Forwards gyroscope and accelerometer data (3 samples per frame)
- Low-latency proxy functionality for competitive gaming
- Works with Raspberry Pi 4 HID gadget functionality

## Requirements

- Rust (latest stable version)
- Nintendo Switch Pro Controller connected via USB
- Raspberry Pi 4 (or compatible device) with HID gadget functionality
- `/dev/hidg0` device configured for HID gadget mode
- Linux system with HID permissions

## Setup

1. Make sure your user has permission to access HID devices:
   ```bash
   sudo usermod -a -G input $USER
   # or create a udev rule for the Pro Controller
   echo 'SUBSYSTEM=="hidraw", ATTRS{idVendor}=="057e", ATTRS{idProduct}=="2009", MODE="0666"' | sudo tee /etc/udev/rules.d/99-nintendo-pro-controller.rules
   sudo udevadm control --reload-rules
   ```

2. Connect your Nintendo Switch Pro Controller via USB

3. Build and run the proxy:
   ```bash
   cargo build --release
   cargo run --bin procon
   ```

## How It Works

The proxy program:

1. Connects to the Nintendo Switch Pro Controller via USB HID
2. Continuously reads HID input reports from the controller
3. Forwards the raw HID data to `/dev/hidg0` (HID gadget device)
4. The Nintendo Switch receives the data as if it's coming directly from a Pro Controller

This creates a transparent proxy that allows the Nintendo Switch to see the Pi as a Pro Controller while the Pi forwards all data from the real controller.

## Project Structure

The codebase is organized into the following modules:

- `src/lib.rs` - Main library entry point that exports all modules
- `src/device.rs` - Nintendo Switch Pro Controller device connection and communication
  - `ProController` struct that wraps HidDevice for controller communication
  - Device discovery and connection functionality
  - Data capture loop for reading input reports
- `src/keystate.rs` - Data structures for controller state representation
  - `ButtonState` - All button states (A, B, X, Y, triggers, etc.)
  - `StickData` - Analog stick position data
  - `GyroData` - Gyroscope and accelerometer sensor data
  - `ControllerState` - Complete controller state with timestamp
- `src/parser.rs` - Input report parsing functionality
  - Parses raw HID input reports into structured controller state
  - Handles button bit mapping, stick coordinate extraction, and gyro data
- `src/bin/proconproxy.rs` - Main executable for proxy functionality
  - Implements real-time HID data forwarding from Pro Controller to HID gadget device

## Notes

- The proxy runs until interrupted with Ctrl+C
- All HID data is forwarded as raw bytes with minimal latency
- The program requires write access to `/dev/hidg0`
- For optimal performance, run with real-time priority if needed
- The controller sends data at approximately 60Hz (16.67ms intervals) 