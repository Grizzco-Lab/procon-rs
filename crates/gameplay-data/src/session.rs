//! `session.json`: what a session folder holds and how to line it up.
//!
//! Frame `n` of a segment was captured at `start_unix_ms + n * 1000 / fps`
//! on the host clock. Older sessions lack `video.fps` and `video.height`
//! and have variable-rate video; newer ones may add `game_settings`,
//! `video.audio` and, per segment with sound, `audio_start_unix_ms`. Fields
//! that are absent stay absent when written back.

use alloc::string::String;
use alloc::vec::Vec;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Name of the description file in a session folder
pub const SESSION_FILE: &str = "session.json";

/// A session's description
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    /// Host Unix ms when recording started
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_unix_ms: Option<u64>,
    /// Host Unix ms when recording stopped
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_at_unix_ms: Option<u64>,
    /// The game's controller settings during the session
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game_settings: Option<GameSettings>,
    /// The controller proxy the reports came from
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxyInfo>,
    /// The controller log
    pub controller: ControllerInfo,
    /// The video files
    pub video: VideoInfo,
}

/// Splatoon 3's controller settings, which scale gyro and stick input into
/// camera rotation
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GameSettings {
    /// Motion (gyro) aiming is on
    pub motion_controls: bool,
    /// Motion sensitivity, -5 to +5 in steps of 0.5
    pub motion_sensitivity: f32,
    /// Right stick sensitivity, -5 to +5 in steps of 0.5
    pub stick_sensitivity: f32,
    /// Vertical aiming is inverted
    pub invert_y: bool,
    /// Horizontal aiming is inverted
    pub invert_x: bool,
}

/// The controller proxy
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProxyInfo {
    /// Address the studio connected to
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// Smallest `host_now - proxy_timestamp` seen: network latency plus the
    /// clock difference
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock_offset_ms: Option<i64>,
}

/// The controller log
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ControllerInfo {
    /// File name in the session folder
    pub file: String,
    /// Record size, 80
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_size: Option<u32>,
    /// Records written
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames: Option<u64>,
    /// Reports the studio saw missing from the sequence
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped: Option<u64>,
}

/// The video recording
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VideoInfo {
    /// Capture input, such as `/dev/video0` or `screen`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    /// Recorded height; the width follows 16:9
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// Constant frame rate; absent in older, variable-rate sessions
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<u32>,
    /// The sound track's format, for segments that have one
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<AudioInfo>,
    /// One video file per stretch between pauses
    pub segments: Vec<SegmentInfo>,
}

/// Format of the sound tracks
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioInfo {
    /// Codec name, such as `opus`
    pub codec: String,
    /// Samples per second
    pub sample_rate: u32,
    /// Channel count
    pub channels: u32,
}

/// One video file
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SegmentInfo {
    /// File name in the session folder
    pub file: String,
    /// Host Unix ms of the first frame; absent if no frame was recorded
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_unix_ms: Option<u64>,
    /// Host Unix ms of the sound track's first sample, if the file has one;
    /// the track starts with the first frame
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_start_unix_ms: Option<u64>,
}

impl SegmentInfo {
    /// Whether the file has a sound track
    pub fn has_audio(&self) -> bool {
        self.audio_start_unix_ms.is_some()
    }
}

impl SessionInfo {
    /// Read the `session.json` of a session folder
    pub fn read(dir: &Path) -> Result<Self> {
        let path = dir.join(SESSION_FILE);
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("cannot parse {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_format() {
        let text = r#"{"controller": {"dropped": 0, "file": "controller.bin", "frame_size": 80, "frames": 0},
            "game_settings": {"invert_x": false, "invert_y": true, "motion_controls": true,
                              "motion_sensitivity": 1.5, "stick_sensitivity": -0.5},
            "proxy": {"address": "127.0.0.1:7397", "clock_offset_ms": -2},
            "started_at_unix_ms": 1790369541466, "stopped_at_unix_ms": 1790369546477,
            "video": {"audio": {"channels": 2, "codec": "opus", "sample_rate": 48000},
                      "fps": 30, "height": 720, "input": "screen",
                      "segments": [{"audio_start_unix_ms": 1790369541479, "file": "video-01.mkv",
                                    "start_unix_ms": 1790369541479}]}}"#;
        let info: SessionInfo = serde_json::from_str(text).unwrap();
        assert_eq!(info.video.fps, Some(30));
        assert_eq!(info.proxy.as_ref().unwrap().clock_offset_ms, Some(-2));
        assert!(info.video.segments[0].has_audio());
        assert_eq!(info.game_settings.as_ref().unwrap().stick_sensitivity, -0.5);
        let again: SessionInfo =
            serde_json::from_str(&serde_json::to_string(&info).unwrap()).unwrap();
        assert_eq!(again, info);
    }

    #[test]
    fn older_format() {
        let text = r#"{"controller": {"dropped": 0, "file": "controller.bin", "frame_size": 80, "frames": 1486},
            "proxy": {"address": "192.168.4.94:7331", "clock_offset_ms": 14},
            "started_at_unix_ms": 1790318208330, "stopped_at_unix_ms": 1790318229741,
            "video": {"input": "/dev/video0",
                      "segments": [{"file": "video-01.mkv", "start_unix_ms": 1790318210540}]}}"#;
        let info: SessionInfo = serde_json::from_str(text).unwrap();
        assert_eq!(info.video.fps, None);
        assert!(info.game_settings.is_none());
        assert!(!info.video.segments[0].has_audio());
        let written = serde_json::to_string(&info).unwrap();
        assert!(!written.contains("fps") && !written.contains("audio"));
    }
}
