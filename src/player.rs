//! Studio side of replay: load actions and play them to the proxy
//!
//! The dashboard loads a session folder, a `controller.bin` or a `.jsonl` of
//! [`Action`]s (such as a model's predictions), then plays it: each action is
//! sent to the proxy's replay port at its `t_ms`, and the Switch sees it
//! instead of, or mixed with, the controller (see [`crate::replay`]). Stopping,
//! or reaching the end, closes the connection and gives the controller back.
//! Pausing does too, until Resume reconnects and carries on where it paused.

use crate::recorder::expand_home;
use crate::replay::{self, Action};
use alloc::sync::Arc;
use anyhow::{Context, Result, ensure};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use core::time::Duration;
use serde::Serialize;
use std::io::Write;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::Mutex;
use std::thread;
use std::time::Instant;

/// Longest sleep between checks for Stop
const STOP_CHECK: Duration = Duration::from_millis(50);

/// A loaded file of actions
struct Track {
    path: String,
    actions: Arc<Vec<Action>>,
}

/// State shared with the playing thread
#[derive(Default)]
struct Shared {
    playing: AtomicBool,
    paused: AtomicBool,
    stop: AtomicBool,
    mix: AtomicBool,
    position_ms: AtomicU64,
    /// Why the last playback ended early
    error: Mutex<Option<String>>,
}

/// What the dashboard shows
#[derive(Serialize)]
pub struct PlayerStatus {
    /// Loaded file, if any
    pub path: Option<String>,
    pub actions: usize,
    pub duration_ms: u64,
    pub position_ms: u64,
    pub playing: bool,
    /// Playing but paused, with the controller back
    pub paused: bool,
    /// Combine with the controller instead of replacing it
    pub mix: bool,
    pub error: Option<String>,
}

/// Plays loaded actions to the proxy's replay port
pub struct Player {
    /// `host:port` of the proxy's `[replay]` port
    address: String,
    track: Mutex<Option<Track>>,
    shared: Arc<Shared>,
}

impl Player {
    pub fn new(address: String, mix: bool) -> Self {
        let shared = Arc::new(Shared::default());
        shared.mix.store(mix, Ordering::Relaxed);
        Self {
            address,
            track: Mutex::new(None),
            shared,
        }
    }

    /// Load a session folder, `controller.bin` or `.jsonl`; `~` is the home folder
    pub fn load(&self, path: &str) -> Result<()> {
        ensure!(!self.is_playing(), "stop the replay before loading another");
        let path = expand_home(path);
        let actions = replay::load(Path::new(&path))?;
        log::info!("Loaded {} actions from {}", actions.len(), path);
        *self.track.lock().unwrap() = Some(Track {
            path,
            actions: Arc::new(actions),
        });
        self.shared.position_ms.store(0, Ordering::Relaxed);
        *self.shared.error.lock().unwrap() = None;
        Ok(())
    }

    /// Path of the loaded file
    pub fn path(&self) -> Option<String> {
        self.track.lock().unwrap().as_ref().map(|t| t.path.clone())
    }

    /// Start playing the loaded actions from the beginning
    pub fn play(&self) -> Result<()> {
        ensure!(!self.is_playing(), "already playing");
        let actions = match self.track.lock().unwrap().as_ref() {
            Some(track) => Arc::clone(&track.actions),
            None => anyhow::bail!("load a file first"),
        };
        // Connect here so a missing proxy is reported right away
        let stream = connect(&self.address)?;

        let shared = Arc::clone(&self.shared);
        let address = self.address.clone();
        shared.stop.store(false, Ordering::Relaxed);
        shared.paused.store(false, Ordering::Relaxed);
        shared.position_ms.store(0, Ordering::Relaxed);
        shared.playing.store(true, Ordering::Relaxed);
        *shared.error.lock().unwrap() = None;
        log::info!("Replaying {} actions to {}", actions.len(), self.address);
        thread::spawn(move || {
            if let Err(e) = send_actions(stream, &address, &actions, &shared) {
                log::warn!("Replay stopped: {:#}", e);
                *shared.error.lock().unwrap() = Some(format!("{e:#}"));
            }
            shared.playing.store(false, Ordering::Relaxed);
            log::info!("Replay finished; the controller is back");
        });
        Ok(())
    }

    /// Stop playing; the controller takes over
    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }

    /// Pause, handing the Switch back to the controller, or resume
    pub fn set_paused(&self, paused: bool) -> Result<()> {
        ensure!(self.is_playing(), "nothing is playing");
        self.shared.paused.store(paused, Ordering::Relaxed);
        Ok(())
    }

    /// Mix with the controller from the next action on
    pub fn set_mix(&self, enabled: bool) {
        self.shared.mix.store(enabled, Ordering::Relaxed);
    }

    pub fn mix(&self) -> bool {
        self.shared.mix.load(Ordering::Relaxed)
    }

    fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Relaxed)
    }

    pub fn status(&self) -> PlayerStatus {
        let track = self.track.lock().unwrap();
        let actions = track.as_ref().map(|t| t.actions.as_slice()).unwrap_or(&[]);
        PlayerStatus {
            path: track.as_ref().map(|t| t.path.clone()),
            actions: actions.len(),
            duration_ms: actions.last().and_then(|a| a.t_ms).unwrap_or(0),
            position_ms: self.shared.position_ms.load(Ordering::Relaxed),
            playing: self.is_playing(),
            paused: self.is_playing() && self.shared.paused.load(Ordering::Relaxed),
            mix: self.mix(),
            error: self.shared.error.lock().unwrap().clone(),
        }
    }
}

/// Connect to the proxy's replay port
fn connect(address: &str) -> Result<TcpStream> {
    let socket = address
        .to_socket_addrs()?
        .next()
        .context("replay address did not resolve")?;
    let stream = TcpStream::connect_timeout(&socket, Duration::from_secs(3))
        .with_context(|| format!("cannot reach the proxy's replay port {address}"))?;
    stream.set_nodelay(true)?;
    Ok(stream)
}

/// Send each action at its `t_ms`, until the end or Stop; while paused, the
/// connection is closed and the timeline waits
fn send_actions(
    stream: TcpStream,
    address: &str,
    actions: &[Action],
    shared: &Shared,
) -> Result<()> {
    let mut stream = Some(stream);
    // Where t_ms = 0 falls, moved later by every pause
    let mut start = Instant::now();
    for action in actions {
        let due = Duration::from_millis(action.t_ms.unwrap_or(0));
        loop {
            if shared.stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            if shared.paused.load(Ordering::Relaxed) {
                if stream.take().is_some() {
                    log::info!("Replay paused; the controller is back");
                }
                let paused_at = Instant::now();
                while shared.paused.load(Ordering::Relaxed) && !shared.stop.load(Ordering::Relaxed)
                {
                    thread::sleep(STOP_CHECK);
                }
                start += paused_at.elapsed();
                continue;
            }
            if stream.is_none() {
                stream = Some(connect(address)?);
                log::info!("Replay resumed");
            }
            let Some(wait) = due.checked_sub(start.elapsed()) else {
                break;
            };
            thread::sleep(wait.min(STOP_CHECK));
        }
        let action = Action {
            mix: shared.mix.load(Ordering::Relaxed),
            ..action.clone()
        };
        let mut line = serde_json::to_vec(&action)?;
        line.push(b'\n');
        stream
            .as_mut()
            .expect("connected above")
            .write_all(&line)
            .context("the proxy closed the replay connection")?;
        shared
            .position_ms
            .store(due.as_millis() as u64, Ordering::Relaxed);
    }
    Ok(())
}
