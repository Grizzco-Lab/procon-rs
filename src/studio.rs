//! Studio host: ties controller recording and video capture into sessions
//!
//! A session folder holds `controller.bin` (frames streamed from the proxy),
//! `video-01.mkv`, `video-02.mkv`, … (one per stretch between pauses) and
//! `session.json` describing how to line them up.
//!
//! It also holds the replay [`Player`], which plays loaded actions to the Switch.
//!
//! Settings changed from the dashboard (path prefix, video input, replay file)
//! are saved to a small JSON state file so they survive restarts.

use crate::dump::unix_ms;
use crate::player::Player;
use crate::recorder::{CONTROLLER_FILE, Recorder, RecorderState};
use crate::stream::LinkStats;
use crate::video::Video;
use alloc::sync::Arc;
use anyhow::{Context, Result, ensure};
use core::sync::atomic::Ordering;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Dashboard settings kept across restarts
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SavedState {
    pub prefix: Option<String>,
    /// Video input id; empty for none
    pub video_input: Option<String>,
    /// Recorded height, 0 for the source size
    pub video_height: Option<u32>,
    /// Recorded frame rate
    pub video_fps: Option<u32>,
    /// The preview follows the recording size and rate
    pub preview_matches_recording: Option<bool>,
    /// Last loaded replay file
    pub replay_path: Option<String>,
    /// Replay mixes with the controller
    pub replay_mix: Option<bool>,
}

impl SavedState {
    /// Read the state file; a missing or broken file means no saved settings
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }
}

/// Command sent by the dashboard
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Command {
    Start,
    Pause,
    Resume,
    Stop,
    SetPrefix { prefix: String },
    SetVideoInput { input: String },
    SetVideoQuality { height: u32, fps: u32 },
    SetPreviewMatchesRecording { enabled: bool },
    LoadReplay { path: String },
    PlayReplay,
    StopReplay,
    SetReplayMix { enabled: bool },
}

/// One recorded video file of the session
#[derive(Serialize)]
struct Segment {
    file: String,
    /// Unix ms of the first frame; frame `n` came `n / fps` seconds later
    start_unix_ms: Option<u64>,
}

/// The session being recorded, or the last one
struct Session {
    dir: PathBuf,
    started_at_ms: u64,
    stopped_at_ms: Option<u64>,
    video_input: Option<String>,
    segments: Vec<Segment>,
    /// Link drop counter when the session started
    dropped_before: u64,
}

/// Session coordinator shared by the web server
pub struct Studio {
    pub recorder: Recorder,
    pub video: Video,
    pub player: Player,
    pub link: Arc<LinkStats>,
    pub proxy_address: String,
    state_path: PathBuf,
    session: Mutex<Option<Session>>,
}

impl Studio {
    pub fn new(
        recorder: Recorder,
        video: Video,
        player: Player,
        link: Arc<LinkStats>,
        proxy_address: String,
        state_path: PathBuf,
    ) -> Self {
        Self {
            recorder,
            video,
            player,
            link,
            proxy_address,
            state_path,
            session: Mutex::new(None),
        }
    }

    /// Apply a dashboard command
    pub fn run(&self, command: Command) -> Result<()> {
        let mut session = self.session.lock().unwrap_or_else(|e| e.into_inner());
        match command {
            Command::Start => {
                let dir = self.recorder.start()?;
                let mut new = Session {
                    dir,
                    started_at_ms: unix_ms(),
                    stopped_at_ms: None,
                    video_input: self.video.input(),
                    segments: Vec::new(),
                    dropped_before: self.link.dropped.load(Ordering::Relaxed),
                };
                self.start_segment(&mut new);
                self.write_session(&new)?;
                *session = Some(new);
            }
            Command::Pause => {
                self.recorder.pause()?;
                if let Some(current) = session.as_mut() {
                    self.finish_segment(current);
                    self.write_session(current)?;
                }
            }
            Command::Resume => {
                self.recorder.resume()?;
                if let Some(current) = session.as_mut() {
                    self.start_segment(current);
                    self.write_session(current)?;
                }
            }
            Command::Stop => {
                let stopped_at = unix_ms();
                self.recorder.stop()?;
                if let Some(current) = session.as_mut() {
                    // Stamped before ffmpeg spends a moment finishing the file
                    current.stopped_at_ms = Some(stopped_at);
                    self.finish_segment(current);
                    self.write_session(current)?;
                }
            }
            Command::SetPrefix { prefix } => {
                self.recorder.set_prefix(&prefix)?;
                self.save_state()?;
            }
            Command::SetVideoInput { input } => {
                ensure!(
                    self.recorder.status().state == RecorderState::Idle,
                    "stop recording before changing the video input"
                );
                let input = Some(input).filter(|id| !id.is_empty());
                self.video.set_input(input)?;
                self.save_state()?;
            }
            Command::SetVideoQuality { height, fps } => {
                self.video.set_quality(height, fps)?;
                self.save_state()?;
            }
            Command::SetPreviewMatchesRecording { enabled } => {
                self.video.set_preview_matches_recording(enabled)?;
                self.save_state()?;
            }
            Command::LoadReplay { path } => {
                self.player.load(&path)?;
                self.save_state()?;
            }
            Command::PlayReplay => self.player.play()?,
            Command::StopReplay => self.player.stop(),
            Command::SetReplayMix { enabled } => {
                self.player.set_mix(enabled);
                self.save_state()?;
            }
        }
        Ok(())
    }

    /// Bytes of video in the current or last session
    pub fn video_bytes(&self) -> u64 {
        let session = self.session.lock().unwrap_or_else(|e| e.into_inner());
        let Some(session) = session.as_ref() else {
            return 0;
        };
        session
            .segments
            .iter()
            .filter_map(|segment| std::fs::metadata(session.dir.join(&segment.file)).ok())
            .map(|meta| meta.len())
            .sum()
    }

    /// Bytes of the other sessions under the path prefix (folders holding a
    /// `session.json`), leaving out the current or last one, which is counted live
    ///
    /// Walks the folder, so call it now and then rather than on every tick.
    pub fn other_sessions_bytes(&self) -> u64 {
        let current = self
            .session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|session| session.dir.clone());
        let prefix = self.recorder.prefix();
        let name = prefix.rsplit('/').next().unwrap_or_default().to_string();
        let Ok(entries) = std::fs::read_dir(self.recorder.prefix_dir()) else {
            return 0;
        };
        entries
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(&name))
            .map(|entry| entry.path())
            .filter(|dir| dir.join("session.json").is_file() && Some(dir) != current.as_ref())
            .filter_map(|dir| std::fs::read_dir(dir).ok())
            .flat_map(|files| files.flatten())
            .filter_map(|file| file.metadata().ok())
            .map(|meta| meta.len())
            .sum()
    }

    /// Record the next video file of the session, if there is a video input
    fn start_segment(&self, session: &mut Session) {
        let name = format!(
            "video-{:02}.{}",
            session.segments.len() + 1,
            self.video.extension()
        );
        if self.video.start_recording(&session.dir.join(&name)) {
            session.segments.push(Segment {
                file: name,
                start_unix_ms: None,
            });
        }
    }

    fn finish_segment(&self, session: &mut Session) {
        let started = self.video.stop_recording();
        if let Some(segment) = session.segments.last_mut()
            && segment.start_unix_ms.is_none()
        {
            segment.start_unix_ms = started;
        }
    }

    /// Describe the session in `session.json`
    fn write_session(&self, session: &Session) -> Result<()> {
        let recorder = self.recorder.status();
        let (height, fps) = self.video.quality();
        let description = json!({
            "started_at_unix_ms": session.started_at_ms,
            "stopped_at_unix_ms": session.stopped_at_ms,
            "proxy": {
                "address": self.proxy_address,
                // host clock = proxy clock + offset, estimated from the stream
                "clock_offset_ms": self.link.clock_offset_ms.load(Ordering::Relaxed),
            },
            "controller": {
                "file": CONTROLLER_FILE,
                "frame_size": crate::dump::FRAME_SIZE,
                "frames": recorder.frames,
                "dropped": self.link.dropped.load(Ordering::Relaxed) - session.dropped_before,
            },
            "video": {
                "input": session.video_input,
                "height": height,
                "fps": fps,
                "segments": session.segments,
            },
        });
        let path = session.dir.join("session.json");
        std::fs::write(&path, serde_json::to_string_pretty(&description)?)
            .with_context(|| format!("cannot write {}", path.display()))
    }

    /// Save dashboard settings next to the config file
    fn save_state(&self) -> Result<()> {
        let (height, fps) = self.video.quality();
        let state = SavedState {
            prefix: Some(self.recorder.prefix()),
            video_input: Some(self.video.input().unwrap_or_default()),
            video_height: Some(height),
            video_fps: Some(fps),
            preview_matches_recording: Some(self.video.preview_matches_recording()),
            replay_path: self.player.path(),
            replay_mix: Some(self.player.mix()),
        };
        // Write then rename, so a crash never leaves a half-written file
        let temp = self.state_path.with_extension("tmp");
        std::fs::write(&temp, serde_json::to_string_pretty(&state)?)?;
        std::fs::rename(&temp, &self.state_path)
            .with_context(|| format!("cannot save {}", self.state_path.display()))
    }
}
