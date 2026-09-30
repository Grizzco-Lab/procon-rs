//! Studio host: ties controller recording and video capture into sessions
//!
//! A session folder holds `controller.bin` (frames streamed from the proxy),
//! `video-01.mkv`, `video-02.mkv`, … (one per stretch between pauses) and
//! `session.json` describing how to line them up.
//!
//! It also holds the replay [`Player`], which plays loaded actions to the
//! Switch, and the [`Bot`], through which the Predictor's AgentZero plays it:
//! the two clients of the proxy's replay port.
//!
//! Settings changed from the dashboard (path prefix, video input, replay file,
//! techniques added to the list, what the bot may press) are saved to a small
//! JSON state file so they survive restarts.
//!
//! While a session is open, the Techniques panel marks spans of it as a
//! technique practised: a span started and stopped by hand, or the last few
//! seconds. They go to `session.json` as `markers` (see
//! [`gameplay_data::session::Marker`]), which the Inkspector can edit too, so
//! the file holds the list and each change here reads it first.
//!
//! A run of the bot is recorded too (see [`crate::predictor::online`]): a
//! session of its own, `<prefix>bot-<stamp>/`, from the play to its end, or
//! the session being recorded, which takes it. Either gets `bot` in
//! `session.json` ([`BotRecord`]: each play, and where a person took over)
//! and the policy's actions in `agentzero.jsonl` ([`BOT_LOG_FILE`]).

use crate::audio;
use crate::dump::unix_ms;
use crate::player::Player;
use crate::predictor::online::limits::Limits;
use crate::predictor::online::{Bot, Ended, Takeover};
use crate::recorder::{CONTROLLER_FILE, Recorder, RecorderState};
use crate::stream::LinkStats;
use crate::video::Video;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use anyhow::{Context, Result, bail, ensure};
use core::sync::atomic::{AtomicBool, Ordering};
use gameplay_data::session::{MARKER_TECHNIQUE, Marker, SessionInfo, write_atomic, write_markers};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

/// The policy's actions of a bot run, one JSON line each, in the session
/// folder (see [`BotRecord`])
pub const BOT_LOG_FILE: &str = "agentzero.jsonl";

/// Between the prefix and the stamp of a session the bot's run starts:
/// `<prefix>bot-2026-09-28_21-00-00/`, easy to tell from one's own
pub const BOT_INFIX: &str = "bot-";

/// The game's controller settings, as set in Splatoon 3's options (TV mode)
///
/// They sit between the controller and the camera: the same turn of the view
/// takes a different gyro or stick input at another sensitivity, so every
/// session records them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameSettings {
    /// Motion (gyro) aiming is on
    pub motion_controls: bool,
    /// Motion sensitivity, -5 to +5 in steps of 0.5
    pub motion_sensitivity: f32,
    /// Right stick sensitivity, -5 to +5 in steps of 0.5
    pub stick_sensitivity: f32,
    pub invert_y: bool,
    pub invert_x: bool,
}

impl Default for GameSettings {
    fn default() -> Self {
        Self {
            motion_controls: true,
            motion_sensitivity: 0.0,
            stick_sensitivity: 0.0,
            invert_y: false,
            invert_x: false,
        }
    }
}

impl GameSettings {
    fn validate(&self) -> Result<()> {
        for value in [self.motion_sensitivity, self.stick_sensitivity] {
            ensure!(
                (-5.0..=5.0).contains(&value) && (value * 2.0).fract() == 0.0,
                "sensitivity goes from -5 to +5 in steps of 0.5, not {value}"
            );
        }
        Ok(())
    }
}

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
    /// Recordings get the sound track
    pub record_audio: Option<bool>,
    /// Last loaded replay file
    pub replay_path: Option<String>,
    /// Replay mixes with the controller
    pub replay_mix: Option<bool>,
    /// The game's controller settings, for the next session
    pub game_settings: Option<GameSettings>,
    /// Techniques added to the Techniques panel's list (the dashboard has
    /// the usual ones)
    pub techniques: Option<Vec<Technique>>,
    /// What the Predictor's bot may press
    pub bot_limits: Option<Limits>,
    /// Record the bot's runs as sessions (on unless turned off)
    pub record_bot_runs: Option<bool>,
}

/// A play of the Predictor's bot about to start, for its record
#[derive(Debug, Clone)]
pub struct BotStart {
    pub checkpoint: String,
    pub cpu: bool,
    /// Seconds confirmed on the page
    pub seconds: f64,
    pub limits: Limits,
}

/// One time the bot played the Switch during the session
#[derive(Debug, Clone, Serialize)]
pub struct Play {
    /// The policy checkpoint, `runs/policy/<checkpoint>` in AgentZero
    pub checkpoint: String,
    /// The policy ran on the CPU
    pub cpu: bool,
    /// What it might press: the masks and the press-rate cap
    pub limits: Limits,
    /// Seconds confirmed on the page
    pub seconds: f64,
    /// Host Unix ms, like the session's markers
    pub start_ms: u64,
    pub end_ms: Option<u64>,
    /// Why it ended
    pub ended: Option<Ended>,
    /// Actions sent to the proxy
    pub sent: u64,
}

/// The Predictor's bot in a session, `bot` in `session.json`: the frames of
/// `controller.bin` and the video hold what reached the Switch (the bot's
/// actions mixed with the person's), [`BOT_LOG_FILE`] what the policy
/// wanted, action by action, and this what played when and where a person
/// took over
#[derive(Debug, Clone, Serialize)]
pub struct BotRecord {
    /// The session was started for the bot's run and stops with it; else
    /// the bot played during a session recorded from the Studio
    pub own_session: bool,
    /// The policy's actions, one JSON line each: [`BOT_LOG_FILE`]
    pub log: String,
    pub plays: Vec<Play>,
    /// Where a person's input showed over the bot's
    pub takeovers: Vec<Takeover>,
}

/// A technique added to the Techniques panel's list
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Technique {
    /// Its name, as markers get it
    pub label: String,
    /// Its name in Chinese
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zh: Option<String>,
    /// Its Pedia term id
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub term: Option<String>,
}

/// Longest "mark the last N seconds", in seconds
const MAX_MARK_LAST_S: f64 = 600.0;

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
    SetPrefix {
        prefix: String,
    },
    SetVideoInput {
        input: String,
    },
    SetVideoQuality {
        height: u32,
        fps: u32,
    },
    SetPreviewMatchesRecording {
        enabled: bool,
    },
    SetRecordAudio {
        enabled: bool,
    },
    SetGameSettings {
        settings: GameSettings,
    },
    LoadReplay {
        path: String,
    },
    PlayReplay,
    PauseReplay,
    ResumeReplay,
    StopReplay,
    SetReplayMix {
        enabled: bool,
    },
    /// Start a span of a technique now (ending the open one)
    MarkStart {
        label: String,
        term: Option<String>,
    },
    /// End the open span
    MarkStop,
    /// Mark the last `seconds` as a technique
    MarkLast {
        label: String,
        term: Option<String>,
        seconds: f64,
    },
    /// Remove the last marker of the session (a mistaken key)
    MarkUndo,
    /// The techniques added to the list
    SetTechniques {
        techniques: Vec<Technique>,
    },
}

/// A technique span started but not ended yet
#[derive(Clone)]
struct OpenSpan {
    label: String,
    term: Option<String>,
    start_ms: u64,
}

/// One recorded video file of the session
#[derive(Serialize)]
struct Segment {
    file: String,
    /// Unix ms of the first frame; frame `n` came `n / fps` seconds later
    start_unix_ms: Option<u64>,
    /// Unix ms of the first sample of the file's sound track, if it has one
    #[serde(skip_serializing_if = "Option::is_none")]
    audio_start_unix_ms: Option<u64>,
}

/// The session being recorded, or the last one
struct Session {
    dir: PathBuf,
    started_at_ms: u64,
    stopped_at_ms: Option<u64>,
    video_input: Option<String>,
    segments: Vec<Segment>,
    /// The game's controller settings during the session
    game_settings: GameSettings,
    /// Link drop counter when the session started
    dropped_before: u64,
    /// The markers as last read from or written to `session.json`
    markers: Vec<Marker>,
    /// The technique span being marked
    open_span: Option<OpenSpan>,
    /// The bot's plays, once it played during the session
    bot: Option<BotRecord>,
}

impl Session {
    /// A session recording into `dir` from now
    fn new(studio: &Studio, dir: PathBuf, bot: Option<BotRecord>) -> Self {
        Self {
            dir,
            started_at_ms: unix_ms(),
            stopped_at_ms: None,
            video_input: studio.video.input(),
            segments: Vec::new(),
            game_settings: studio.game_settings(),
            dropped_before: studio.link.dropped.load(Ordering::Relaxed),
            markers: Vec::new(),
            open_span: None,
            bot,
        }
    }

    /// The folder's name, as the Inkspector lists it
    fn name(&self) -> String {
        self.dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// The bot's play under way, if any
    fn open_play(&mut self) -> Option<&mut Play> {
        self.bot
            .as_mut()?
            .plays
            .last_mut()
            .filter(|play| play.end_ms.is_none())
    }

    /// Take the markers from `session.json`, where the Inkspector may have
    /// changed them; keep the known ones when it cannot be read
    fn reload_markers(&mut self) {
        match SessionInfo::read(&self.dir) {
            Ok(info) => self.markers = info.markers,
            Err(e) => log::warn!("Keeping the markers known: {:#}", e),
        }
    }

    /// End the open span at `now`, adding it to the markers
    fn close_span(&mut self, now: u64) -> bool {
        let Some(span) = self.open_span.take() else {
            return false;
        };
        self.markers.push(Marker {
            kind: MARKER_TECHNIQUE.into(),
            label: span.label,
            term: span.term,
            t_start_ms: span.start_ms,
            t_end_ms: now.max(span.start_ms),
            created_ms: now,
        });
        true
    }
}

/// A technique's name and term as the dashboard sends them: trimmed, and
/// never empty
fn technique(label: &str, term: Option<String>) -> Result<(String, Option<String>)> {
    let label = label.trim();
    ensure!(!label.is_empty(), "pick a technique first");
    let term = term.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    Ok((label.to_string(), term))
}

/// Session coordinator shared by the web server
pub struct Studio {
    pub recorder: Recorder,
    pub video: Video,
    pub player: Player,
    /// AgentZero's hold on the replay port, when it plays
    pub bot: Bot,
    pub link: Arc<LinkStats>,
    pub proxy_address: String,
    state_path: PathBuf,
    session: Mutex<Option<Session>>,
    game_settings: Mutex<GameSettings>,
    /// Techniques added to the Techniques panel's list
    techniques: Mutex<Vec<Technique>>,
    /// The bot's runs are recorded
    record_bot_runs: AtomicBool,
    /// [`BOT_LOG_FILE`] of the session recording the bot, open from its
    /// first play to the session's end. Behind a lock of its own: the
    /// policy's actions are logged on their way to the Switch, and must not
    /// wait while the session lock is held for file work (a session started
    /// on a network mount takes most of a second, longer than the bot may
    /// go quiet, [`crate::predictor::online::STALL`])
    bot_log: Mutex<Option<BufWriter<File>>>,
}

impl Studio {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        recorder: Recorder,
        video: Video,
        player: Player,
        bot: Bot,
        link: Arc<LinkStats>,
        proxy_address: String,
        state_path: PathBuf,
        game_settings: GameSettings,
        techniques: Vec<Technique>,
        record_bot_runs: bool,
    ) -> Self {
        Self {
            recorder,
            video,
            player,
            bot,
            link,
            proxy_address,
            state_path,
            session: Mutex::new(None),
            game_settings: Mutex::new(game_settings),
            techniques: Mutex::new(techniques),
            record_bot_runs: AtomicBool::new(record_bot_runs),
            bot_log: Mutex::new(None),
        }
    }

    fn lock_session(&self) -> MutexGuard<'_, Option<Session>> {
        self.session.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn lock_bot_log(&self) -> MutexGuard<'_, Option<BufWriter<File>>> {
        self.bot_log.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Start recording a session (`infix` names it, see
    /// [`Recorder::start`]) with its first video file
    fn start_session(
        &self,
        session: &mut Option<Session>,
        infix: &str,
        bot: Option<BotRecord>,
    ) -> Result<()> {
        let dir = self.recorder.start(infix)?;
        // The bot's log of a session that ended without a stop here
        drop(self.lock_bot_log().take());
        let mut new = Session::new(self, dir, bot);
        self.start_segment(&mut new);
        self.write_session(&new)?;
        *session = Some(new);
        Ok(())
    }

    /// Stop the recording: the video file, the controller data, and
    /// `session.json` with the end time
    fn stop_session(&self, session: &mut Option<Session>) -> Result<()> {
        let stopped_at = unix_ms();
        self.recorder.stop()?;
        if let Some(current) = session.as_mut() {
            // Stamped before ffmpeg spends a moment finishing the file
            current.stopped_at_ms = Some(stopped_at);
            current.reload_markers();
            current.close_span(stopped_at);
            self.finish_segment(current);
            let log = self.lock_bot_log().take();
            if let Some(mut log) = log
                && let Err(e) = log.flush()
            {
                log::warn!("Cannot finish {BOT_LOG_FILE}: {e}");
            }
            self.write_session(current)?;
        }
        Ok(())
    }

    /// Apply a dashboard command
    pub fn run(&self, command: Command) -> Result<()> {
        let mut session = self.lock_session();
        match command {
            Command::Start => self.start_session(&mut session, "", None)?,
            Command::Pause => {
                let paused_at = unix_ms();
                self.recorder.pause()?;
                if let Some(current) = session.as_mut() {
                    // A span ends with the frames
                    current.reload_markers();
                    current.close_span(paused_at);
                    self.finish_segment(current);
                    self.write_session(current)?;
                }
            }
            Command::Resume => {
                self.recorder.resume()?;
                if let Some(current) = session.as_mut() {
                    current.reload_markers();
                    self.start_segment(current);
                    self.write_session(current)?;
                }
            }
            Command::Stop => self.stop_session(&mut session)?,
            Command::MarkStart { label, term } => {
                let (label, term) = technique(&label, term)?;
                ensure!(
                    self.recorder.status().state == RecorderState::Recording,
                    "start recording to mark a technique"
                );
                let current = session.as_mut().context("no session is open")?;
                let now = unix_ms();
                current.reload_markers();
                // Another technique ends the one being marked
                if current.close_span(now) {
                    write_markers(&current.dir, &current.markers)?;
                }
                current.open_span = Some(OpenSpan {
                    label,
                    term,
                    start_ms: now,
                });
            }
            Command::MarkStop => {
                let current = session.as_mut().context("no session is open")?;
                ensure!(current.open_span.is_some(), "no technique is being marked");
                current.reload_markers();
                current.close_span(unix_ms());
                write_markers(&current.dir, &current.markers)?;
            }
            Command::MarkLast {
                label,
                term,
                seconds,
            } => {
                let (label, term) = technique(&label, term)?;
                ensure!(
                    seconds > 0.0 && seconds <= MAX_MARK_LAST_S,
                    "mark between 0 and {MAX_MARK_LAST_S} seconds, not {seconds}"
                );
                ensure!(
                    self.recorder.status().state != RecorderState::Idle,
                    "start recording to mark a technique"
                );
                let current = session.as_mut().context("no session is open")?;
                let now = unix_ms();
                current.reload_markers();
                current.markers.push(Marker {
                    kind: MARKER_TECHNIQUE.into(),
                    label,
                    term,
                    t_start_ms: now
                        .saturating_sub((seconds * 1000.0) as u64)
                        .max(current.started_at_ms),
                    t_end_ms: now,
                    created_ms: now,
                });
                write_markers(&current.dir, &current.markers)?;
            }
            Command::MarkUndo => {
                let current = session.as_mut().context("no session is open")?;
                // The span being marked goes first, else the last marker made
                if current.open_span.take().is_none() {
                    current.reload_markers();
                    let last = current
                        .markers
                        .iter()
                        .enumerate()
                        .max_by_key(|(_, m)| m.created_ms)
                        .map(|(i, _)| i);
                    let Some(last) = last else {
                        bail!("no marker to undo");
                    };
                    current.markers.remove(last);
                    write_markers(&current.dir, &current.markers)?;
                }
            }
            Command::SetTechniques { techniques } => {
                let mut kept = Vec::new();
                for t in techniques {
                    let (label, term) = technique(&t.label, t.term)?;
                    let zh = t.zh.map(|z| z.trim().to_string()).filter(|z| !z.is_empty());
                    kept.push(Technique { label, zh, term });
                }
                *self.techniques.lock().unwrap() = kept;
                self.save_state()?;
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
            Command::SetGameSettings { settings } => {
                ensure!(
                    self.recorder.status().state == RecorderState::Idle,
                    "stop recording before changing the game settings"
                );
                settings.validate()?;
                *self.game_settings.lock().unwrap() = settings;
                self.save_state()?;
            }
            Command::SetRecordAudio { enabled } => {
                self.video.set_record_audio(enabled);
                self.save_state()?;
            }
            Command::LoadReplay { path } => {
                self.player.load(&path)?;
                self.save_state()?;
            }
            Command::PlayReplay => self.player.play()?,
            Command::PauseReplay => self.player.set_paused(true)?,
            Command::ResumeReplay => self.player.set_paused(false)?,
            Command::StopReplay => self.player.stop(),
            Command::SetReplayMix { enabled } => {
                self.player.set_mix(enabled);
                self.save_state()?;
            }
        }
        Ok(())
    }

    /// The game's controller settings for the next session
    pub fn game_settings(&self) -> GameSettings {
        self.game_settings.lock().unwrap().clone()
    }

    /// What the bot may press, from its next action on; saved
    pub fn set_bot_limits(&self, limits: Limits) -> Result<()> {
        limits.validate()?;
        // One save at a time, as the dashboard's commands do
        let _session = self.lock_session();
        self.bot.set_limits(limits);
        self.save_state()
    }

    /// The bot's runs are recorded (see the module docs)
    pub fn record_bot_runs(&self) -> bool {
        self.record_bot_runs.load(Ordering::Relaxed)
    }

    /// Record the bot's runs from the next play on; saved
    pub fn set_record_bot_runs(&self, enabled: bool) -> Result<()> {
        let _session = self.lock_session();
        self.record_bot_runs.store(enabled, Ordering::Relaxed);
        self.save_state()
    }

    /// The bot starts to play: record it in the session being recorded, or
    /// in a session of its own (`<prefix>bot-<stamp>/`), and open the log
    /// of its actions; answers with the session's folder
    pub fn bot_play_started(&self, start: BotStart) -> Result<PathBuf> {
        let mut session = self.lock_session();
        let recording = self.recorder.status().state != RecorderState::Idle;
        if !recording {
            let record = BotRecord {
                own_session: true,
                log: BOT_LOG_FILE.to_string(),
                plays: Vec::new(),
                takeovers: Vec::new(),
            };
            self.start_session(&mut session, BOT_INFIX, Some(record))?;
        }
        let current = session.as_mut().context("no session is open")?;
        let record = current.bot.get_or_insert_with(|| BotRecord {
            own_session: false,
            log: BOT_LOG_FILE.to_string(),
            plays: Vec::new(),
            takeovers: Vec::new(),
        });
        record.plays.push(Play {
            checkpoint: start.checkpoint,
            cpu: start.cpu,
            limits: start.limits,
            seconds: start.seconds,
            start_ms: unix_ms(),
            end_ms: None,
            ended: None,
            sent: 0,
        });
        // Opened outside the log's lock: the actions go on meanwhile
        if self.lock_bot_log().is_none() {
            let path = current.dir.join(BOT_LOG_FILE);
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .with_context(|| format!("cannot open {}", path.display()))?;
            *self.lock_bot_log() = Some(BufWriter::new(file));
        }
        self.write_session(current)?;
        log::info!("Recording the bot's run in {}", current.dir.display());
        Ok(current.dir.clone())
    }

    /// The bot's play ended: its end, why, what it sent and the takeovers
    /// seen go to `session.json`; a session of its own stops
    pub fn bot_play_ended(
        &self,
        ended: Option<Ended>,
        sent: u64,
        takeovers: Vec<Takeover>,
    ) -> Result<()> {
        let mut session = self.lock_session();
        let own = {
            let Some(current) = session.as_mut().filter(|s| s.bot.is_some()) else {
                return Ok(());
            };
            let now = unix_ms();
            if let Some(play) = current.open_play() {
                play.end_ms = Some(now);
                play.ended = ended;
                play.sent = sent;
            }
            let record = current.bot.as_mut().context("no bot record")?;
            record.takeovers.extend(takeovers);
            let own = record.own_session
                && current.stopped_at_ms.is_none()
                && self.recorder.status().state != RecorderState::Idle;
            if let Some(log) = self.lock_bot_log().as_mut() {
                log.flush()?;
            }
            if !own {
                self.write_session(current)?;
            }
            own
        };
        if own {
            self.stop_session(&mut session)?;
        }
        Ok(())
    }

    /// A play of the bot is being recorded
    pub fn bot_play_open(&self) -> bool {
        self.lock_session()
            .as_mut()
            .is_some_and(|s| s.open_play().is_some())
    }

    /// The session being recorded is the bot's own
    pub fn recording_bot_run(&self) -> bool {
        self.lock_session().as_ref().is_some_and(|s| {
            s.stopped_at_ms.is_none() && s.bot.as_ref().is_some_and(|bot| bot.own_session)
        })
    }

    /// One of the policy's actions, for the log of the session recording
    /// the bot (`line` is made only then); a failed write ends the log.
    /// Never waits for the session lock (see `bot_log`)
    pub fn bot_action(&self, line: impl FnOnce() -> Value) {
        let mut guard = self.lock_bot_log();
        let Some(log) = guard.as_mut() else {
            return;
        };
        let written = serde_json::to_writer(&mut *log, &line())
            .map_err(std::io::Error::other)
            .and_then(|()| log.write_all(b"\n"));
        if let Err(e) = written {
            log::warn!("Stopped writing {BOT_LOG_FILE}: {e}");
            *guard = None;
        }
    }

    /// For the page: whether runs are recorded, the session recording the
    /// bot (the current or last one with a record) and whether a play is
    /// being recorded in it
    pub fn bot_record_status(&self) -> Value {
        let mut session = self.lock_session();
        let with_bot = session.as_mut().filter(|s| s.bot.is_some());
        json!({
            "enabled": self.record_bot_runs(),
            "session": with_bot.as_ref().map(|s| s.name()),
            "open": with_bot.is_some_and(|s| s.open_play().is_some()),
        })
    }

    /// The Techniques panel's state: the techniques added to the list, the
    /// span being marked (`label`, `term`, `elapsed_ms`) and the markers of
    /// the current or last session (`counts` by label, `total`)
    pub fn techniques_status(&self) -> Value {
        let added = self.techniques.lock().unwrap().clone();
        let session = self.session.lock().unwrap_or_else(|e| e.into_inner());
        let mut counts = BTreeMap::<&str, usize>::new();
        let (mut open, mut total) = (Value::Null, 0);
        if let Some(session) = session.as_ref() {
            for marker in &session.markers {
                *counts.entry(&marker.label).or_default() += 1;
            }
            total = session.markers.len();
            if let Some(span) = &session.open_span {
                open = json!({
                    "label": span.label,
                    "term": span.term,
                    "elapsed_ms": unix_ms().saturating_sub(span.start_ms),
                });
            }
        }
        json!({ "added": added, "open": open, "counts": counts, "total": total })
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
                audio_start_unix_ms: None,
            });
        }
    }

    fn finish_segment(&self, session: &mut Session) {
        let times = self.video.stop_recording();
        if let Some(segment) = session.segments.last_mut()
            && segment.start_unix_ms.is_none()
            && let Some(times) = times
        {
            segment.start_unix_ms = times.first_frame_ms;
            segment.audio_start_unix_ms = times.audio_start_ms;
        }
    }

    /// Describe the session in `session.json`
    fn write_session(&self, session: &Session) -> Result<()> {
        let recorder = self.recorder.status();
        let (height, fps) = self.video.quality();
        let mut description = json!({
            "started_at_unix_ms": session.started_at_ms,
            "stopped_at_unix_ms": session.stopped_at_ms,
            // Splatoon 3's controller settings: they scale gyro and stick into camera turns
            "game_settings": session.game_settings,
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
                // Sound track in the video files, where a segment has audio_start_unix_ms
                "audio": { "codec": "opus", "sample_rate": audio::SAMPLE_RATE, "channels": audio::CHANNELS },
                "segments": session.segments,
            },
        });
        if !session.markers.is_empty() {
            // Technique spans marked by hand, in host Unix ms like the frames
            description["markers"] = json!(session.markers);
        }
        if let Some(bot) = &session.bot {
            // The Predictor's bot played: its plays and the takeovers
            description["bot"] = json!(bot);
        }
        write_atomic(
            &session.dir.join("session.json"),
            &serde_json::to_string_pretty(&description)?,
        )
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
            record_audio: Some(self.video.record_audio()),
            game_settings: Some(self.game_settings()),
            replay_path: self.player.path(),
            replay_mix: Some(self.player.mix()),
            techniques: Some(self.techniques.lock().unwrap().clone()),
            bot_limits: Some(self.bot.limits()),
            record_bot_runs: Some(self.record_bot_runs()),
        };
        // Write then rename, so a crash never leaves a half-written file
        let temp = self.state_path.with_extension("tmp");
        std::fs::write(&temp, serde_json::to_string_pretty(&state)?)?;
        std::fs::rename(&temp, &self.state_path)
            .with_context(|| format!("cannot save {}", self.state_path.display()))
    }
}
