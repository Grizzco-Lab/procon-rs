# Splabot - Nintendo Switch Pro Controller HID Dumper

A Rust program that captures input data from Nintendo Switch Pro Controller via USB HID and outputs it as JSON with timestamps.

## Features

- Captures all button inputs (A, B, X, Y, L, R, ZL, ZR, +, -, Home, Capture, etc.)
- Reads analog stick positions (left and right sticks)
- Captures gyroscope and accelerometer data (3 samples per frame)
- Outputs data in JSON format with precise timestamps
- Real-time data streaming

## Requirements

- Rust (latest stable version)
- Nintendo Switch Pro Controller connected via USB
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

3. Build and run the program:
   ```bash
   cargo build --release
   cargo run
   ```

## Output Format

The program outputs JSON data for each frame with the following structure:

```json
{
  "timestamp": "2024-01-15T10:30:45.123456Z",
  "buttons": {
    "a": false,
    "b": false,
    "x": false,
    "y": false,
    "l": false,
    "r": false,
    "zl": false,
    "zr": false,
    "plus": false,
    "minus": false,
    "home": false,
    "capture": false,
    "l_stick": false,
    "r_stick": false,
    "up": false,
    "down": false,
    "left": false,
    "right": false,
    "sl_left": false,
    "sr_left": false,
    "sl_right": false,
    "sr_right": false
  },
  "left_stick": {
    "x": 2048,
    "y": 2048
  },
  "right_stick": {
    "x": 2048,
    "y": 2048
  },
  "gyro": [
    {
      "accel_x": 0,
      "accel_y": 0,
      "accel_z": 4096,
      "gyro_x": 0,
      "gyro_y": 0,
      "gyro_z": 0
    },
    {
      "accel_x": 0,
      "accel_y": 0,
      "accel_z": 4096,
      "gyro_x": 0,
      "gyro_y": 0,
      "gyro_z": 0
    },
    {
      "accel_x": 0,
      "accel_y": 0,
      "accel_z": 4096,
      "gyro_x": 0,
      "gyro_y": 0,
      "gyro_z": 0
    }
  ],
  "battery_level": 8,
  "connection_info": 1
}
```

## Notes

- The program runs until interrupted with Ctrl+C
- Stick values range from 0 to 4095 (center is around 2048)
- Gyroscope and accelerometer values are raw sensor data
- Battery level is reported as a value from 0-15
- The controller sends 3 gyro/accel samples per input report for higher precision 