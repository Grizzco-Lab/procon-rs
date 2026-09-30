//! Controller orientation from the IMU, for the dashboard's Splatoon mode
//!
//! Integrates the gyro into a quaternion and slowly pulls it toward gravity
//! from the accelerometer, so tilt stays right while yaw drifts slowly, just
//! like the game. Pressing Y recenters: the current pose becomes flat.
//!
//! Everything is in the dashboard's CSS frame, where the tilt view was tuned:
//! CSS x = IMU y, CSS y = IMU x, CSS z = -IMU z (a proper rotation).

use crate::keystate::{ControllerState, GyroData};

/// Uncalibrated gyro raw units to radians per second (0.07 °/s per unit)
const GYRO_RAD: f64 = 0.07 * core::f64::consts::PI / 180.0;

/// Accelerometer raw units per g
const ACCEL_G: f64 = 4096.0;

/// How hard gravity pulls the estimate back, in rad/s per unit of error
const GRAVITY_GAIN: f64 = 1.5;

/// Longest step integrated at once, so a pause in reports cannot jolt the pose
const MAX_STEP_S: f64 = 0.05;

/// Rotation as a unit quaternion (w, x, y, z)
type Quat = [f64; 4];

fn mul(a: Quat, b: Quat) -> Quat {
    [
        a[0] * b[0] - a[1] * b[1] - a[2] * b[2] - a[3] * b[3],
        a[0] * b[1] + a[1] * b[0] + a[2] * b[3] - a[3] * b[2],
        a[0] * b[2] - a[1] * b[3] + a[2] * b[0] + a[3] * b[1],
        a[0] * b[3] + a[1] * b[2] - a[2] * b[1] + a[3] * b[0],
    ]
}

/// Rotate vector `v` by `q`
fn rotate(q: Quat, v: [f64; 3]) -> [f64; 3] {
    let r = mul(mul(q, [0.0, v[0], v[1], v[2]]), [q[0], -q[1], -q[2], -q[3]]);
    [r[1], r[2], r[3]]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn norm(v: [f64; 3]) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

/// Mean of the report's IMU samples, mapped to the CSS frame
fn imu_average(samples: &[GyroData]) -> Option<([f64; 3], [f64; 3])> {
    if samples.is_empty() {
        return None;
    }
    let n = samples.len() as f64;
    let mean = |f: fn(&GyroData) -> i16| samples.iter().map(|s| f(s) as f64).sum::<f64>() / n;
    let gyro = [mean(|s| s.gyro_y), mean(|s| s.gyro_x), -mean(|s| s.gyro_z)];
    let accel = [
        mean(|s| s.accel_y),
        mean(|s| s.accel_x),
        -mean(|s| s.accel_z),
    ];
    Some((gyro, accel))
}

/// Tracked controller pose
#[derive(Clone)]
pub struct Orientation {
    pose: Quat,
    /// Where gravity pointed (as measured) when the pose was last recentered
    reference: Option<[f64; 3]>,
    /// Gyro offset learned while the controller lies still, in raw units
    bias: [f64; 3],
    last_ms: Option<u64>,
    y_held: bool,
}

impl Default for Orientation {
    fn default() -> Self {
        Self {
            pose: [1.0, 0.0, 0.0, 0.0],
            reference: None,
            bias: [0.0; 3],
            last_ms: None,
            y_held: false,
        }
    }
}

impl Orientation {
    /// Advance by one input report taken at `timestamp_ms` (proxy clock)
    pub fn update(&mut self, state: &ControllerState, timestamp_ms: u64) {
        let Some((gyro_raw, accel_raw)) = imu_average(&state.gyro) else {
            return;
        };
        let accel_len = norm(accel_raw);
        let steady = (accel_len / ACCEL_G - 1.0).abs() < 0.1;

        // Pressing Y makes the current pose the flat one, as in Splatoon
        let y = state.buttons.y;
        if (y && !self.y_held) || self.reference.is_none() {
            self.pose = [1.0, 0.0, 0.0, 0.0];
            if accel_len > 0.0 {
                self.reference = Some(accel_raw.map(|a| a / accel_len));
            }
        }
        self.y_held = y;

        // Learn the gyro offset whenever the controller is barely moving
        let rate = [
            gyro_raw[0] - self.bias[0],
            gyro_raw[1] - self.bias[1],
            gyro_raw[2] - self.bias[2],
        ];
        if steady && rate.iter().all(|r| r.abs() * 0.07 < 3.0) {
            for (bias, raw) in self.bias.iter_mut().zip(gyro_raw) {
                *bias += 0.02 * (raw - *bias);
            }
        }

        let dt = match self.last_ms.replace(timestamp_ms) {
            Some(last) => (timestamp_ms.saturating_sub(last) as f64 / 1000.0).min(MAX_STEP_S),
            None => 0.0,
        };
        let mut omega = [rate[0] * GYRO_RAD, rate[1] * GYRO_RAD, rate[2] * GYRO_RAD];

        // Nudge the measured gravity back onto the reference (in body coordinates)
        if steady && let Some(reference) = self.reference {
            let measured = rotate(self.pose, accel_raw.map(|a| a / accel_len));
            let error = cross(measured, reference);
            let conjugate = [self.pose[0], -self.pose[1], -self.pose[2], -self.pose[3]];
            let body = rotate(conjugate, error);
            for i in 0..3 {
                omega[i] += GRAVITY_GAIN * body[i];
            }
        }

        // pose <- pose * exp(omega * dt / 2)
        let angle = norm(omega) * dt;
        if angle > 0.0 {
            let axis = omega.map(|w| w / norm(omega));
            let (sin, cos) = (angle / 2.0).sin_cos();
            let step = [cos, axis[0] * sin, axis[1] * sin, axis[2] * sin];
            let pose = mul(self.pose, step);
            let length = pose.iter().map(|c| c * c).sum::<f64>().sqrt();
            self.pose = pose.map(|c| c / length);
        }
    }

    /// Pose as a quaternion (w, x, y, z) for CSS `rotate3d`
    pub fn quaternion(&self) -> Quat {
        self.pose
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::ProConParser;

    /// Input report with the given IMU sample repeated three times
    fn report(gyro: [i16; 3], accel: [i16; 3], y: bool) -> ControllerState {
        let mut data = [0u8; 49];
        data[0] = 0x30;
        data[3] = y as u8;
        for sample in 0..3 {
            for (i, value) in accel.iter().chain(&gyro).enumerate() {
                let offset = 13 + sample * 12 + i * 2;
                data[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
            }
        }
        ProConParser::parse_input_report(&data).unwrap()
    }

    fn angle_deg(q: Quat) -> f64 {
        2.0 * q[0].clamp(-1.0, 1.0).acos().to_degrees()
    }

    #[test]
    fn integrates_rotation() {
        let mut orientation = Orientation::default();
        // 90 °/s around the IMU y axis for one second; 2 g keeps gravity out of it
        for step in 0..=125 {
            orientation.update(&report([0, 1286, 0], [0, 0, 8192], false), step * 8);
        }
        let q = orientation.quaternion();
        assert!((angle_deg(q) - 90.0).abs() < 1.0, "angle {}", angle_deg(q));
        // IMU y is CSS x
        assert!(q[1] > 0.7 && q[2].abs() < 0.01 && q[3].abs() < 0.01);
    }

    /// Raw accelerometer reading when the controller has turned by `angle` (rad)
    /// about the CSS x axis, having been recentered while flat
    fn tilted_accel(angle: f64) -> [i16; 3] {
        // Flat reads +1 g on IMU z, which is -z in the CSS frame
        let flat = [0.0, 0.0, -4096.0];
        let (sin, cos) = angle.sin_cos();
        // Body sees gravity rotated the other way: R(angle)^-1 * flat
        let css = [
            flat[0],
            cos * flat[1] + sin * flat[2],
            -sin * flat[1] + cos * flat[2],
        ];
        // CSS (x, y, z) = IMU (y, x, -z)
        [css[1] as i16, css[0] as i16, -css[2] as i16]
    }

    #[test]
    fn gravity_agrees_with_gyro() {
        let mut orientation = Orientation::default();
        orientation.update(&report([0, 0, 0], tilted_accel(0.0), false), 0);
        // Turn 90 degrees at 90 °/s with matching accelerometer readings
        for step in 1..=125 {
            let angle = (step as f64 * 0.008).min(1.0) * core::f64::consts::FRAC_PI_2;
            orientation.update(&report([0, 1286, 0], tilted_accel(angle), false), step * 8);
        }
        // Then hold still for two seconds
        for step in 126..=375 {
            let accel = tilted_accel(core::f64::consts::FRAC_PI_2);
            orientation.update(&report([0, 0, 0], accel, false), step * 8);
        }
        let q = orientation.quaternion();
        assert!((angle_deg(q) - 90.0).abs() < 3.0, "angle {}", angle_deg(q));
        assert!(q[1] > 0.6, "axis {:?}", q);
    }

    #[test]
    fn holds_still_and_recenters_on_y() {
        let mut orientation = Orientation::default();
        for step in 0..=125 {
            orientation.update(&report([0, 0, 0], [0, 0, 4096], false), step * 8);
        }
        assert!(angle_deg(orientation.quaternion()) < 0.1);

        for step in 126..=250 {
            orientation.update(&report([1286, 0, 0], [0, 0, 8192], false), step * 8);
        }
        assert!(angle_deg(orientation.quaternion()) > 45.0);

        orientation.update(&report([0, 0, 0], [0, 0, 4096], true), 2008);
        assert!(angle_deg(orientation.quaternion()) < 0.1);
    }
}
