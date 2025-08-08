use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use hidapi::{HidApi, HidDevice};
use log::{error, info, warn};
use serde::{Deserialize, Serialize};

// Nintendo Switch Pro Controller Vendor ID and Product ID
const NINTENDO_VENDOR_ID: u16 = 0x057e;
const PRO_CONTROLLER_PRODUCT_ID: u16 = 0x2009;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ButtonState {
    pub y: bool,
    pub x: bool,
    pub b: bool,
    pub a: bool,
    pub sr_right: bool,
    pub sl_right: bool,
    pub r: bool,
    pub zr: bool,
    pub minus: bool,
    pub plus: bool,
    pub r_stick: bool,
    pub l_stick: bool,
    pub home: bool,
    pub capture: bool,
    pub down: bool,
    pub up: bool,
    pub right: bool,
    pub left: bool,
    pub sr_left: bool,
    pub sl_left: bool,
    pub l: bool,
    pub zl: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StickData {
    pub x: u16,
    pub y: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GyroData {
    pub accel_x: i16,
    pub accel_y: i16,
    pub accel_z: i16,
    pub gyro_x: i16,
    pub gyro_y: i16,
    pub gyro_z: i16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControllerState {
    pub timestamp: DateTime<Utc>,
    pub buttons: ButtonState,
    pub left_stick: StickData,
    pub right_stick: StickData,
    pub gyro: Vec<GyroData>, // NS Pro Controller sends 3 gyro samples per report
    pub battery_level: u8,
    pub connection_info: u8,
}

pub struct ProControllerParser {
    device: HidDevice,
}

impl ProControllerParser {
    pub fn connect() -> Result<Self> {
        let hid_api = HidApi::new()?;
        info!("Searching for Nintendo Switch Pro Controller...");

        // List all HID devices to find the Pro Controller
        for device_info in hid_api.device_list() {
            if device_info.vendor_id() == NINTENDO_VENDOR_ID
                && device_info.product_id() == PRO_CONTROLLER_PRODUCT_ID
            {
                info!(
                    "Found Pro Controller: {}",
                    device_info.product_string().unwrap_or("Unknown")
                );

                let device = device_info.open_device(&hid_api)?;

                return Ok(Self { device });
            }
        }
        Err(anyhow!("Nintendo Switch Pro Controller not found"))
    }

    pub fn parse_input_report(&self, data: &[u8]) -> Result<ControllerState> {
        if data.len() < 64 {
            return Err(anyhow!("Input report too short: {} bytes", data.len()));
        }

        // Parse button data (bytes 3-5)
        let button_data = [data[3], data[4], data[5]];
        let buttons = self.parse_buttons(&button_data);

        // Parse stick data
        let left_stick = StickData {
            x: ((data[6] as u16) | ((data[7] as u16 & 0x0F) << 8)),
            y: (((data[7] as u16 & 0xF0) >> 4) | ((data[8] as u16) << 4)),
        };

        let right_stick = StickData {
            x: ((data[9] as u16) | ((data[10] as u16 & 0x0F) << 8)),
            y: (((data[10] as u16 & 0xF0) >> 4) | ((data[11] as u16) << 4)),
        };

        // Parse gyro data (3 samples, each 12 bytes starting at offset 13)
        let mut gyro_samples = Vec::new();
        for i in 0..3 {
            let offset = 13 + i * 12;
            if offset + 11 < data.len() {
                let gyro = GyroData {
                    accel_x: i16::from_le_bytes([data[offset], data[offset + 1]]),
                    accel_y: i16::from_le_bytes([data[offset + 2], data[offset + 3]]),
                    accel_z: i16::from_le_bytes([data[offset + 4], data[offset + 5]]),
                    gyro_x: i16::from_le_bytes([data[offset + 6], data[offset + 7]]),
                    gyro_y: i16::from_le_bytes([data[offset + 8], data[offset + 9]]),
                    gyro_z: i16::from_le_bytes([data[offset + 10], data[offset + 11]]),
                };
                gyro_samples.push(gyro);
            }
        }

        let battery_level = data[2] >> 4; // Upper 4 bits of byte 2
        let connection_info = data[2] & 0x0F; // Lower 4 bits of byte 2

        Ok(ControllerState {
            timestamp: Utc::now(),
            buttons,
            left_stick,
            right_stick,
            gyro: gyro_samples,
            battery_level,
            connection_info,
        })
    }

    fn parse_buttons(&self, button_data: &[u8; 3]) -> ButtonState {
        ButtonState {
            // First byte (right buttons)
            y: (button_data[0] & 0x01) != 0,
            x: (button_data[0] & 0x02) != 0,
            b: (button_data[0] & 0x04) != 0,
            a: (button_data[0] & 0x08) != 0,
            sr_right: (button_data[0] & 0x10) != 0,
            sl_right: (button_data[0] & 0x20) != 0,
            r: (button_data[0] & 0x40) != 0,
            zr: (button_data[0] & 0x80) != 0,

            // Second byte (shared buttons)
            minus: (button_data[1] & 0x01) != 0,
            plus: (button_data[1] & 0x02) != 0,
            r_stick: (button_data[1] & 0x04) != 0,
            l_stick: (button_data[1] & 0x08) != 0,
            home: (button_data[1] & 0x10) != 0,
            capture: (button_data[1] & 0x20) != 0,

            // Third byte (left buttons and dpad)
            down: (button_data[2] & 0x01) != 0,
            up: (button_data[2] & 0x02) != 0,
            right: (button_data[2] & 0x04) != 0,
            left: (button_data[2] & 0x08) != 0,
            sr_left: (button_data[2] & 0x10) != 0,
            sl_left: (button_data[2] & 0x20) != 0,
            l: (button_data[2] & 0x40) != 0,
            zl: (button_data[2] & 0x80) != 0,
        }
    }

    pub fn start_capture(&mut self) -> Result<()> {
        info!("Starting Pro Controller data capture...");
        info!("Press Ctrl+C to stop");

        let mut buffer = [0u8; 64];
        let mut frame_count = 0u64;

        loop {
            match self.device.read_timeout(&mut buffer, 1000) {
                Ok(size) => {
                    if size > 0 {
                        match self.parse_input_report(&buffer[..size]) {
                            Ok(state) => {
                                frame_count += 1;

                                // Convert to JSON and print
                                match serde_json::to_string_pretty(&state) {
                                    Ok(json) => {
                                        println!("Frame {}: {}", frame_count, json);
                                    }
                                    Err(e) => {
                                        error!("Failed to serialize to JSON: {}", e);
                                    }
                                }
                            }
                            Err(e) => {
                                warn!("Failed to parse input report: {}", e);
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to read from device: {}", e);
                    break;
                }
            }
        }

        Ok(())
    }
}

fn main() -> Result<()> {
    env_logger::init();

    info!("Nintendo Switch Pro Controller HID Dumper");
    info!("==========================================");

    // Try to connect to the controller
    let mut parser = ProControllerParser::connect()?;

    // Start capturing data
    parser.start_capture()?;

    Ok(())
}
