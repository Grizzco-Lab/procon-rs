//! Recorded gameplay: the session format written by the studio and the
//! per-frame alignment of controller input to video.
//!
//! One definition shared by the recorder (Rust) and the training code
//! (Python, through the `python` feature), so the two never read
//! a session differently.
//!
//! A session folder holds `session.json` ([`session`]), `controller.bin`
//! ([`frame`], [`controller`]) and one video file per segment. [`align`]
//! turns the controller log into actions per video frame, [`labels`] writes
//! and reads them as JSON lines and [`calibration`] holds each session's
//! measured video delay.

extern crate alloc;

pub mod align;
pub mod calibration;
pub mod controller;
pub mod frame;
pub mod labels;
pub mod session;

#[cfg(feature = "python")]
mod python;
