use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};

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