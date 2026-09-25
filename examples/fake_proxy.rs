//! Stand-in for `procon-proxy`: streams a synthetic controller like it does
//!
//! ```sh
//! cargo run --example fake_proxy [port]
//! ```
//!
//! Then point `config.toml`'s `[proxy] address` at `localhost:7331` (or the given
//! port) and run `procon`. No Pro Controller or USB gadget needed.

use core::f64::consts::TAU;
use core::time::Duration;
use procon::dump::{Dumper, Frame};
use procon::stream::FrameStreamer;

/// (byte offset, bit mask) of every Pro Controller button in an input report
const BUTTONS: [(usize, u8); 18] = [
    (3, 0x08), // A
    (3, 0x04), // B
    (3, 0x02), // X
    (3, 0x01), // Y
    (5, 0x40), // L
    (3, 0x40), // R
    (5, 0x80), // ZL
    (3, 0x80), // ZR
    (5, 0x02), // Up
    (5, 0x04), // Right
    (5, 0x01), // Down
    (5, 0x08), // Left
    (4, 0x01), // Minus
    (4, 0x02), // Plus
    (4, 0x20), // Capture
    (4, 0x10), // Home
    (4, 0x08), // Left stick
    (4, 0x04), // Right stick
];

fn main() -> anyhow::Result<()> {
    env_logger::builder()
        .filter_level(log::LevelFilter::Info)
        .init();
    let port = match std::env::args().nth(1) {
        Some(port) => port.parse()?,
        None => 7331,
    };

    let mut streamer = FrameStreamer::listen(port)?;

    // A wired Pro Controller reports every 8 ms
    for tick in 0u64.. {
        streamer.dump(&Frame::new(tick as u32, &fake_report(tick)))?;
        std::thread::sleep(Duration::from_millis(8));
    }
    Ok(())
}

/// Build a 0x30 input report: sticks circle, buttons take turns, gyro wobbles
fn fake_report(tick: u64) -> [u8; 64] {
    let t = tick as f64 * 0.008;
    let mut report = [0u8; 64];
    report[0] = 0x30;
    report[1] = tick as u8;
    report[2] = 0x91; // battery full and charging

    let (byte, mask) = BUTTONS[(tick / 60) as usize % BUTTONS.len()];
    report[byte] |= mask;

    let stick = |angle: f64, radius: f64| {
        let x = (2048.0 + radius * angle.cos()) as u16;
        let y = (2048.0 + radius * angle.sin()) as u16;
        [x as u8, (x >> 8) as u8 | (y << 4) as u8, (y >> 4) as u8]
    };
    report[6..9].copy_from_slice(&stick(t * TAU / 3.0, 1300.0));
    report[9..12].copy_from_slice(&stick(-t * TAU / 5.0, 900.0 * (t * 0.7).sin()));

    // Three IMU samples of accel xyz then gyro xyz, little-endian i16
    for sample in 0..3 {
        let t = t + sample as f64 * 0.005;
        let imu: [f64; 6] = [
            300.0 * (t * 1.1).sin(),
            -200.0,
            4096.0,
            450.0 * (t * 1.9).sin(),
            350.0 * (t * 1.3).cos(),
            200.0 * (t * 0.7).sin(),
        ];
        for (i, value) in imu.iter().enumerate() {
            let offset = 13 + sample * 12 + i * 2;
            report[offset..offset + 2].copy_from_slice(&(*value as i16).to_le_bytes());
        }
    }
    report
}
