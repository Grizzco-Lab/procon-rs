//! Sound from the capture card, for the recordings
//!
//! A small ffmpeg reads the PulseAudio source (the capture card's sound
//! device) all the time as raw 48 kHz stereo, in 10 ms chunks stamped with
//! their arrival. The last two seconds are kept, so a recording can start its
//! sound at the sample that arrived with its first video frame, however late
//! that frame is; from then on chunks stream straight to its encoder. The
//! video file then holds both tracks, both starting at time 0.

use crate::dump::unix_ms;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use anyhow::{Context, Result};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use core::time::Duration;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::mpsc::{SyncSender, TrySendError};
use std::thread;

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u32 = 2;
/// Bytes per sample frame: 16-bit samples for each channel
const FRAME_BYTES: usize = 2 * CHANNELS as usize;
/// Chunk the grabber hands over at once
const CHUNK_MS: u64 = 10;
const CHUNK_BYTES: usize = (SAMPLE_RATE as u64 * CHUNK_MS / 1000) as usize * FRAME_BYTES;
/// Sound kept for a recording whose first frame is still on its way
const HISTORY_MS: u64 = 2000;

/// A chunk of samples and the Unix µs its last sample arrived
struct Chunk {
    arrived_us: u64,
    data: Arc<Vec<u8>>,
}

#[derive(Default)]
struct Inner {
    history: VecDeque<Chunk>,
    /// Encoder input of the recording being made, once its sound started
    sink: Option<SyncSender<Arc<Vec<u8>>>>,
}

/// The capture card's sound; clones share it
#[derive(Clone, Default)]
pub struct Audio {
    inner: Arc<Mutex<Inner>>,
    /// Unix ms of the latest chunk
    last_chunk_ms: Arc<AtomicU64>,
    /// Process id of the running ffmpeg, 0 if none
    pid: Arc<AtomicU32>,
    /// Set on exit: ffmpeg is stopped and not restarted
    stopped: Arc<AtomicBool>,
}

impl Audio {
    /// Read `source` (a PulseAudio source name) from now on, restarting ffmpeg if it exits
    pub fn start(source: String) -> Self {
        let audio = Audio::default();
        let reader = audio.clone();
        thread::spawn(move || {
            while !reader.stopped.load(Ordering::Relaxed) {
                if let Err(e) = reader.grab(&source)
                    && !reader.stopped.load(Ordering::Relaxed)
                {
                    log::warn!("Audio capture from {}: {:#}", source, e);
                }
                thread::sleep(Duration::from_secs(3));
            }
        });
        audio
    }

    /// Stop reading for good, before the studio exits: ffmpeg runs in its own
    /// process group, so a terminal's Ctrl+C does not reach it
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
        let pid = self.pid.swap(0, Ordering::Relaxed);
        if pid != 0 {
            // SAFETY: signals only the ffmpeg this handle started
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        }
    }

    /// Sound arrived within the last second
    pub fn live(&self) -> bool {
        unix_ms().saturating_sub(self.last_chunk_ms.load(Ordering::Relaxed)) < 1000
    }

    /// Stream sound to a recording encoder, starting with the sample that
    /// arrived at Unix ms `start_ms`; returns the Unix ms of that sample, or
    /// `None` when there is no sound to give
    pub fn begin(&self, sink: SyncSender<Arc<Vec<u8>>>, start_ms: u64) -> Option<u64> {
        if !self.live() {
            return None;
        }
        let start_us = start_ms * 1000;
        let chunk_us = CHUNK_MS * 1000;
        let mut inner = self.inner.lock().unwrap();
        // The chunk holding start_ms and all after it; the first is cut at that sample
        let mut started = None;
        for chunk in inner.history.iter().filter(|c| c.arrived_us > start_us) {
            let data = match started {
                Some(_) => Arc::clone(&chunk.data),
                None => {
                    let first_us = chunk.arrived_us.saturating_sub(chunk_us);
                    let skip_frames =
                        start_us.saturating_sub(first_us) * SAMPLE_RATE as u64 / 1_000_000;
                    let skip = (skip_frames as usize * FRAME_BYTES).min(chunk.data.len());
                    started = Some(first_us.max(start_us) / 1000);
                    Arc::new(chunk.data[skip..].to_vec())
                }
            };
            if sink.try_send(data).is_err() {
                return None;
            }
        }
        inner.sink = Some(sink);
        // Nothing arrived since start_ms yet: the sound starts with the next chunk
        Some(started.unwrap_or(start_ms))
    }

    /// Stop streaming to the recording, which ends its sound input
    pub fn end(&self) {
        self.inner.lock().unwrap().sink = None;
    }

    /// Run one ffmpeg reading `source`, until it exits
    fn grab(&self, source: &str) -> Result<()> {
        let mut child = Command::new("ffmpeg")
            .args(["-hide_banner", "-nostdin", "-nostats", "-loglevel", "error"])
            // Small fragments, so chunks arrive as the sound does
            .args(["-f", "pulse", "-fragment_size", &CHUNK_BYTES.to_string()])
            .args(["-i", source])
            .args(["-f", "s16le", "-ar", &SAMPLE_RATE.to_string()])
            .args(["-ac", &CHANNELS.to_string(), "pipe:1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .context("cannot run ffmpeg")?;
        self.pid.store(child.id(), Ordering::Relaxed);
        let mut stdout = child.stdout.take().context("no ffmpeg stdout")?;
        if let Some(stderr) = child.stderr.take() {
            let stopped = Arc::clone(&self.stopped);
            thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    // Complaints about the closed pipe while exiting are expected
                    if !stopped.load(Ordering::Relaxed) {
                        log::warn!("ffmpeg (audio): {}", line);
                    }
                }
            });
        }
        log::info!("Capturing audio from {}", source);
        let mut chunk = vec![0u8; CHUNK_BYTES];
        while stdout.read_exact(&mut chunk).is_ok() {
            let now_ms = unix_ms();
            self.last_chunk_ms.store(now_ms, Ordering::Relaxed);
            let data = Arc::new(chunk.clone());
            let mut inner = self.inner.lock().unwrap();
            if let Some(sink) = &inner.sink
                && let Err(TrySendError::Disconnected(_)) = sink.try_send(Arc::clone(&data))
            {
                inner.sink = None;
            }
            inner.history.push_back(Chunk {
                arrived_us: now_ms * 1000,
                data,
            });
            while inner
                .history
                .front()
                .is_some_and(|c| c.arrived_us + HISTORY_MS * 1000 < now_ms * 1000)
            {
                inner.history.pop_front();
            }
        }
        self.pid.store(0, Ordering::Relaxed);
        let _ = child.kill();
        let status = child.wait()?;
        anyhow::bail!("ffmpeg exited ({status})")
    }
}
