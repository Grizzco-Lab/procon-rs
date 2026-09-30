//! Grizzco Lab, on the capture host: receives the controller frames
//! `procon-proxy` streams, captures the console's video and sound, and
//! serves the dashboard, whose apps are its modules:
//!
//! - [`studio`]: the Studio, live view, recording and replay;
//! - [`inspect`]: the Inkspector, recorded sessions frame by frame and their
//!   object labels;
//! - [`cuttlefish`]: Cuttlefish, video reviews with the AI reviewer, and its
//!   Translate, Knowledge and Pedia views;
//! - [`vision`]: Vision, detection and tracking on recorded sessions;
//! - [`predictor`]: the Predictor, the IDM's predictions on any video and
//!   AgentZero's policy online;
//! - [`pipeline`]: the Pipeline, the GPU and the experiment queue.
//!
//! [`web`] serves the page (embedded from `web/`) with every app's routes,
//! [`config`] reads `config.toml`, and [`exit`] names what the lab still
//! does as it exits.

extern crate alloc;

pub mod config;
pub mod cuttlefish;
pub mod exit;
pub mod inspect;
pub mod pipeline;
pub mod predictor;
pub mod studio;
pub mod vision;
pub mod web;
