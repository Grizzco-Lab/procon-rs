//! Recording sessions controlled from the dashboard
//!
//! Each session is a new file `procon-YYYYMMDD-HHMMSS.bin` in the output
//! directory, holding the same 80-byte timestamped frames as [`FileDumper`].
//! Pausing keeps the file open and skips frames; the frame timestamps show the gap.

use crate::dump::{Dumper, FileDumper};
use alloc::sync::Arc;
use anyhow::{Context, Result, bail, ensure};
use core::time::Duration;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

/// What the recorder is doing
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RecorderState {
    /// No session file is open
    Idle,
    /// Frames are written to the session file
    Recording,
    /// The session file is open but frames are skipped
    Paused,
}

/// Snapshot of the recorder for the dashboard
#[derive(Debug, Serialize)]
pub struct RecorderStatus {
    pub state: RecorderState,
    /// Directory where the next session is created
    pub dir: String,
    /// Current session file, or the last one once stopped
    pub file: Option<String>,
    /// Bytes written to `file`
    pub bytes: u64,
    /// Frames written to `file`
    pub frames: u64,
    /// Recording time of `file`, excluding pauses
    pub elapsed_ms: u64,
    /// Write error that ended the last session
    pub error: Option<String>,
}

struct Inner {
    dir: PathBuf,
    /// Open session file, `None` when idle
    writer: Option<FileDumper>,
    /// Path of the current or last session file
    path: Option<PathBuf>,
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

/// File dumper with start/pause/resume/stop control
///
/// Clones share the same session, so one clone can sit in the dump thread
/// while another is driven by the web server.
#[derive(Clone)]
pub struct Recorder(Arc<Mutex<Inner>>);

impl Recorder {
    /// Create an idle recorder that puts sessions into `dir`
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self(Arc::new(Mutex::new(Inner {
            dir: dir.into(),
            writer: None,
            path: None,
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

    /// Open a new session file and start writing frames
    pub fn start(&self) -> Result<()> {
        let mut inner = self.lock();
        ensure!(inner.writer.is_none(), "already recording");

        let name = chrono::Local::now().format("procon-%Y%m%d-%H%M%S.bin");
        let path = inner.dir.join(name.to_string());
        let writer =
            FileDumper::new(&path).with_context(|| format!("cannot create {}", path.display()))?;
        log::info!("Recording to {}", path.display());

        inner.writer = Some(writer);
        inner.path = Some(path);
        inner.frames = 0;
        inner.active = Duration::ZERO;
        inner.resumed_at = Some(Instant::now());
        inner.error = None;
        Ok(())
    }

    /// Stop writing frames but keep the session file open
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

    /// Close the session file, making sure it reached the disk
    pub fn stop(&self) -> Result<()> {
        let mut inner = self.lock();
        let Some(mut writer) = inner.writer.take() else {
            bail!("not recording");
        };
        inner.stop_clock();
        writer.sync()?;
        log::info!("Recording stopped after {} frames", inner.frames);
        Ok(())
    }

    /// Change the directory for the next session; it must already exist
    pub fn set_dir(&self, dir: &str) -> Result<()> {
        let dir = std::fs::canonicalize(dir.trim())
            .with_context(|| format!("cannot use {}", dir.trim()))?;
        ensure!(dir.is_dir(), "not a directory: {}", dir.display());

        let mut inner = self.lock();
        ensure!(
            inner.writer.is_none(),
            "stop recording before changing the directory"
        );
        log::info!("Recording directory set to {}", dir.display());
        inner.dir = dir;
        Ok(())
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
            dir: inner.dir.display().to_string(),
            file: inner.path.as_ref().map(|p| p.display().to_string()),
            bytes: inner.frames * FileDumper::get_frame_size() as u64,
            frames: inner.frames,
            elapsed_ms: elapsed.as_millis() as u64,
            error: inner.error.clone(),
        }
    }
}

impl Dumper for Recorder {
    fn dump(&mut self, data: &[u8]) -> Result<()> {
        let mut inner = self.lock();
        if inner.resumed_at.is_none() {
            return Ok(());
        }
        let Some(writer) = inner.writer.as_mut() else {
            return Ok(());
        };

        match writer.dump(data) {
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
