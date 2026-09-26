//! Objects in gameplay video: detection and tracking on Splatoon 3 Salmon
//! Run recordings, the first step toward placing enemies on the stage in 3D.
//!
//! - [`frames`]: a session segment's frames, decoded by ffmpeg at 640x360;
//! - [`detect`]: frame to boxes with a YOLOv8 network ([`yolo`]) in candle,
//!   pretrained COCO weights or our own;
//! - [`track`]: boxes over frames to tracks with ids (SORT-like);
//! - [`labels`]: the object label files shared with the labeling tool, and
//!   the rule for merging model boxes into them;
//! - [`render`]: frames with boxes drawn, as PNG.
//!
//! See the crate README for the plan toward Salmon Run-specific detection
//! and 3D placement.

extern crate alloc;

pub mod detect;
pub mod frames;
pub mod labels;
pub mod render;
pub mod track;
pub mod yolo;
