//! Recording sessions controlled from the dashboard
//!
//! Each session is a new folder named after a path prefix and the start time,
//! e.g. prefix `/data/procon/mk8-` gives `/data/procon/mk8-2026-09-24_21-40-05/`.
//! Controller frames go to `controller.bin` inside it, as 80-byte [`Frame`]s.
//! Pausing keeps the file open and skips frames; the frame timestamps show the gap.

use crate::dump::{Dumper, FRAME_SIZE, FileDumper, Frame};
use alloc::sync::Arc;
use anyhow::{Context, Result, bail, ensure};
use core::time::Duration;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

/// File name of the controller data inside a session folder
pub const CONTROLLER_FILE: &str = "controller.bin";

/// What the recorder is doing
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RecorderState {
    /// No session is open
    Idle,
    /// Frames are written to the session
    Recording,
    /// The session is open but frames are skipped
    Paused,
}

/// Snapshot of the recorder for the dashboard
#[derive(Debug, Serialize)]
pub struct RecorderStatus {
    pub state: RecorderState,
    /// Path prefix for new sessions
    pub prefix: String,
    /// Folder the next session would get
    pub next: String,
    /// Current session folder, or the last one once stopped
    pub session: Option<String>,
    /// Bytes of controller data in `session`
    pub bytes: u64,
    /// Frames written to `session`
    pub frames: u64,
    /// Recording time of `session`, excluding pauses
    pub elapsed_ms: u64,
    /// Write error that ended the last session
    pub error: Option<String>,
}

struct Inner {
    prefix: String,
    /// Open controller file, `None` when idle
    writer: Option<FileDumper>,
    /// Current or last session folder
    session: Option<PathBuf>,
    frames: u64,
    /// Recording time accumulated before the last resume
    active: Duration,
    /// Set while recording, cleared while paused or idle
    resumed_at: Option<Instant>,
    error: Option<String>,
}

impl Inner {
    /// Move the running time into `active` and stop the clock
    fn stop_clock(&mut self) {
        if let Some(since) = self.resumed_at.take() {
            self.active += since.elapsed();
        }
    }
}

/// Folder a session starting now would get with `prefix`
fn session_dir(prefix: &str) -> PathBuf {
    let stamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
    PathBuf::from(format!("{prefix}{stamp}"))
}

/// Clean up a typed prefix: expand `~` to the home folder, and treat an
/// existing folder as a folder even without a trailing `/`
fn normalize_prefix(prefix: &str) -> String {
    let prefix = prefix.trim();
    let mut prefix = match (prefix.strip_prefix('~'), std::env::var("HOME")) {
        (Some(rest), Ok(home)) if rest.is_empty() || rest.starts_with('/') => {
            format!("{home}{rest}")
        }
        _ => prefix.to_string(),
    };
    if !prefix.ends_with('/') && Path::new(&prefix).is_dir() {
        prefix.push('/');
    }
    prefix
}

/// Directory that must exist for `prefix` to be usable
fn prefix_parent(prefix: &str) -> PathBuf {
    // "a/b-" lives in "a", "a/b/" lives in "a/b"
    match Path::new(&format!("{prefix}x")).parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// File dumper with start/pause/resume/stop control
///
/// Clones share the same session, so one clone can sit in the frame pipeline
/// while another is driven by the web server.
#[derive(Clone)]
pub struct Recorder(Arc<Mutex<Inner>>);

impl Recorder {
    /// Create an idle recorder that names sessions with `prefix`
    pub fn new(prefix: &str) -> Self {
        Self(Arc::new(Mutex::new(Inner {
            prefix: normalize_prefix(prefix),
            writer: None,
            session: None,
            frames: 0,
            active: Duration::ZERO,
            resumed_at: None,
            error: None,
        })))
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // The state stays consistent even if a holder panicked
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Create a session folder and start writing frames; returns the folder
    pub fn start(&self) -> Result<PathBuf> {
        let prefix = {
            let inner = self.lock();
            ensure!(inner.writer.is_none(), "already recording");
            inner.prefix.clone()
        };

        // Creating files on a network mount can take a second; do it without
        // the lock so incoming frames and the live view keep flowing
        let parent = prefix_parent(&prefix);
        ensure!(
            parent.is_dir(),
            "folder does not exist: {}",
            parent.display()
        );
        let dir = session_dir(&prefix);
        std::fs::create_dir(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
        let writer = FileDumper::new(dir.join(CONTROLLER_FILE))?;
        log::info!("Recording to {}", dir.display());

        let mut inner = self.lock();
        ensure!(inner.writer.is_none(), "already recording");
        inner.writer = Some(writer);
        inner.session = Some(dir.clone());
        inner.frames = 0;
        inner.active = Duration::ZERO;
        inner.resumed_at = Some(Instant::now());
        inner.error = None;
        Ok(dir)
    }

    /// Stop writing frames but keep the session open
    pub fn pause(&self) -> Result<()> {
        let mut inner = self.lock();
        ensure!(inner.resumed_at.is_some(), "not recording");
        inner.stop_clock();
        if let Some(writer) = inner.writer.as_mut() {
            writer.flush()?;
        }
        log::info!("Recording paused");
        Ok(())
    }

    /// Continue writing frames into the paused session
    pub fn resume(&self) -> Result<()> {
        let mut inner = self.lock();
        ensure!(
            inner.writer.is_some() && inner.resumed_at.is_none(),
            "not paused"
        );
        inner.resumed_at = Some(Instant::now());
        log::info!("Recording resumed");
        Ok(())
    }

    /// Close the session, making sure the controller data reached the disk
    pub fn stop(&self) -> Result<()> {
        let (mut writer, frames) = {
            let mut inner = self.lock();
            let Some(writer) = inner.writer.take() else {
                bail!("not recording");
            };
            inner.stop_clock();
            (writer, inner.frames)
        };
        // Syncing to disk can be slow; frames arriving meanwhile are simply not recorded
        writer.sync()?;
        log::info!("Recording stopped after {} frames", frames);
        Ok(())
    }

    /// Change the path prefix for the next session; its folder must already exist
    ///
    /// `~` means the home folder, and an existing folder gets sessions inside it.
    pub fn set_prefix(&self, prefix: &str) -> Result<()> {
        let prefix = normalize_prefix(prefix);
        ensure!(!prefix.is_empty(), "the path prefix is empty");
        let parent = prefix_parent(&prefix);
        ensure!(
            parent.is_dir(),
            "folder does not exist: {}",
            parent.display()
        );

        let mut inner = self.lock();
        ensure!(
            inner.writer.is_none(),
            "stop recording before changing the path"
        );
        log::info!("Recording prefix set to {}", prefix);
        inner.prefix = prefix;
        Ok(())
    }

    /// Path prefix for new sessions
    pub fn prefix(&self) -> String {
        self.lock().prefix.clone()
    }

    /// Directory holding new sessions, for disk space checks
    pub fn prefix_dir(&self) -> PathBuf {
        prefix_parent(&self.lock().prefix)
    }

    /// Take a snapshot for the dashboard
    pub fn status(&self) -> RecorderStatus {
        let inner = self.lock();
        let state = match (&inner.writer, inner.resumed_at) {
            (None, _) => RecorderState::Idle,
            (Some(_), Some(_)) => RecorderState::Recording,
            (Some(_), None) => RecorderState::Paused,
        };
        let elapsed = inner.active + inner.resumed_at.map_or(Duration::ZERO, |t| t.elapsed());

        RecorderStatus {
            state,
            prefix: inner.prefix.clone(),
            next: session_dir(&inner.prefix).display().to_string(),
            session: inner.session.as_ref().map(|p| p.display().to_string()),
            bytes: inner.frames * FRAME_SIZE as u64,
            frames: inner.frames,
            elapsed_ms: elapsed.as_millis() as u64,
            error: inner.error.clone(),
        }
    }
}

impl Dumper for Recorder {
    fn dump(&mut self, frame: &Frame) -> Result<()> {
        let mut inner = self.lock();
        if inner.resumed_at.is_none() {
            return Ok(());
        }
        let Some(writer) = inner.writer.as_mut() else {
            return Ok(());
        };

        match writer.dump(frame) {
            Ok(()) => inner.frames += 1,
            Err(e) => {
                // A full or vanished disk ends the session instead of failing every frame
                log::error!("Recording stopped, write failed: {:#}", e);
                inner.error = Some(format!("write failed: {e:#}"));
                inner.writer = None;
                inner.stop_clock();
            }
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        if let Some(writer) = self.lock().writer.as_mut() {
            writer.flush()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_prefixes() {
        let home = std::env::var("HOME").unwrap();
        let temp = std::env::temp_dir().display().to_string();
        // An existing folder gets sessions inside it
        assert_eq!(normalize_prefix(&temp), format!("{temp}/"));
        // A name prefix is kept as typed
        assert_eq!(
            normalize_prefix(&format!("{temp}/mk8-")),
            format!("{temp}/mk8-")
        );
        assert_eq!(normalize_prefix("~/procon-"), format!("{home}/procon-"));
        assert_eq!(normalize_prefix(" ~ "), format!("{home}/"));
        // Only a leading ~ that means home is expanded
        assert_eq!(normalize_prefix("~other/x-"), "~other/x-");
    }
}
