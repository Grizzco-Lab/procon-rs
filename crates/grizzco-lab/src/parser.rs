use anyhow::{Result, anyhow};

use crate::keystate::{ButtonState, ControllerState, GyroData, StickData};

/// Static parser functions for Pro Controller data
pub struct ProConParser;

impl ProConParser {
    pub fn parse_input_report(data: &[u8]) -> Result<ControllerState> {
        if data.len() < 12 {
            return Err(anyhow!("Input report too short: {} bytes", data.len()));
        }

        // Parse button data (bytes 3-5)
        let button_data = [data[3], data[4], data[5]];
        let buttons = Self::parse_buttons(&button_data);

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
            timestamp: chrono::Utc::now(),
            buttons,
            left_stick,
            right_stick,
            gyro: gyro_samples,
            battery_level,
            connection_info,
        })
    }

    pub fn parse_buttons(button_data: &[u8; 3]) -> ButtonState {
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
}
