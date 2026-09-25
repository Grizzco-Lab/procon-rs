//! Web dashboard: live controller view, recording controls and system stats
//!
//! - `GET /`, `/style.css`, `/app.js`: the page, embedded from `web/`
//! - `GET /ws`: WebSocket pushing `{"type":"state"}` for every input report
//!   and `{"type":"status"}` once per second
//! - `POST /api/recorder`: `{"action":"start"|"pause"|"resume"|"stop"}` or
//!   `{"action":"set_dir","dir":"..."}`, answered with the recorder status

use crate::dump::Dumper;
use crate::parser::ProConParser;
use crate::recorder::Recorder;
use alloc::ffi::CString;
use alloc::sync::Arc;
use anyhow::Result;
use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::Instant;
use tokio::sync::watch;
use warp::Filter;
use warp::http::StatusCode;
use warp::ws::{Message, WebSocket};

/// Dumper that publishes parsed input reports to dashboard clients
#[derive(Clone, Default)]
pub struct LiveFeed {
    /// Latest controller state as JSON
    state: watch::Sender<String>,
    /// Reports seen, used for the input rate and connection status
    frames: Arc<AtomicU64>,
}

impl LiveFeed {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Dumper for LiveFeed {
    fn dump(&mut self, data: &[u8]) -> Result<()> {
        self.frames.fetch_add(1, Ordering::Relaxed);

        // Skip parsing while no browser is watching
        if self.state.receiver_count() > 0
            && data.first() == Some(&0x30)
            && let Ok(state) = ProConParser::parse_input_report(data)
        {
            self.state
                .send_replace(json!({ "type": "state", "state": state }).to_string());
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Dashboard web server
pub struct WebServer {
    feed: LiveFeed,
    recorder: Recorder,
    /// Packets dropped by the async dumper
    dropped: Arc<AtomicU64>,
}

/// Recorder command sent by the dashboard
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum Command {
    Start,
    Pause,
    Resume,
    Stop,
    SetDir { dir: String },
}

impl WebServer {
    pub fn new(feed: LiveFeed, recorder: Recorder, dropped: Arc<AtomicU64>) -> Self {
        Self {
            feed,
            recorder,
            dropped,
        }
    }

    /// Serve the dashboard on all interfaces
    pub async fn run(self, port: u16) {
        let status = watch::Sender::new(String::new());
        tokio::spawn(publish_status(
            Arc::clone(&self.feed.frames),
            self.recorder.clone(),
            self.dropped,
            status.clone(),
        ));

        let index = warp::path::end().map(|| warp::reply::html(include_str!("../web/index.html")));
        let style = warp::path!("style.css")
            .map(|| asset(include_str!("../web/style.css"), "text/css; charset=utf-8"));
        let script = warp::path!("app.js").map(|| {
            asset(
                include_str!("../web/app.js"),
                "text/javascript; charset=utf-8",
            )
        });

        let feed = self.feed.state;
        let websocket = warp::path!("ws")
            .and(warp::ws())
            .map(move |ws: warp::ws::Ws| {
                let state = feed.subscribe();
                let status = status.subscribe();
                ws.on_upgrade(move |socket| stream_to_client(socket, state, status))
            });

        let recorder = self.recorder;
        let api = warp::path!("api" / "recorder")
            .and(warp::post())
            .and(warp::body::content_length_limit(4096))
            .and(warp::body::json())
            .map(move |command: Command| run_command(&recorder, command));

        let routes = warp::get()
            .and(index.or(style).or(script))
            .or(websocket)
            .or(api);

        log::info!("Dashboard on http://0.0.0.0:{}", port);
        warp::serve(routes).run(([0, 0, 0, 0], port)).await;
    }
}

/// Reply with an embedded static file
fn asset(body: &'static str, content_type: &'static str) -> impl warp::Reply {
    warp::reply::with_header(body, "content-type", content_type)
}

/// Apply a dashboard command and reply with the new recorder status
fn run_command(
    recorder: &Recorder,
    command: Command,
) -> warp::reply::WithStatus<warp::reply::Json> {
    let result = match command {
        Command::Start => recorder.start(),
        Command::Pause => recorder.pause(),
        Command::Resume => recorder.resume(),
        Command::Stop => recorder.stop(),
        Command::SetDir { dir } => recorder.set_dir(&dir),
    };

    match result {
        Ok(()) => warp::reply::with_status(warp::reply::json(&recorder.status()), StatusCode::OK),
        Err(e) => {
            log::warn!("Recorder command failed: {:#}", e);
            let error = json!({ "error": format!("{e:#}") });
            warp::reply::with_status(warp::reply::json(&error), StatusCode::BAD_REQUEST)
        }
    }
}

/// Forward controller state and status updates to one browser
async fn stream_to_client(
    socket: WebSocket,
    mut state: watch::Receiver<String>,
    mut status: watch::Receiver<String>,
) {
    let (mut tx, mut rx) = socket.split();
    // Send the current status right away instead of waiting for the next tick
    status.mark_changed();
    let mut ping = tokio::time::interval(Duration::from_secs(30));

    loop {
        // A slow client simply skips intermediate states
        let message = tokio::select! {
            changed = state.changed() => match changed {
                Ok(()) => Message::text(state.borrow_and_update().clone()),
                Err(_) => break,
            },
            changed = status.changed() => match changed {
                Ok(()) => Message::text(status.borrow_and_update().clone()),
                Err(_) => break,
            },
            _ = ping.tick() => Message::ping(Vec::new()),
            incoming = rx.next() => match incoming {
                Some(Ok(message)) if !message.is_close() => continue,
                _ => break,
            },
        };

        if tx.send(message).await.is_err() {
            break;
        }
    }
}

/// Publish a status snapshot every second
async fn publish_status(
    frames: Arc<AtomicU64>,
    recorder: Recorder,
    dropped: Arc<AtomicU64>,
    status: watch::Sender<String>,
) {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    // (time, reports seen, bytes recorded) at the previous tick
    let mut last: Option<(Instant, u64, u64)> = None;

    loop {
        tick.tick().await;

        let now = Instant::now();
        let frame_count = frames.load(Ordering::Relaxed);
        let recorder = recorder.status();
        let (input_rate, write_rate) = match last {
            Some((then, then_frames, then_bytes)) => {
                let secs = now.duration_since(then).as_secs_f64();
                (
                    frame_count.saturating_sub(then_frames) as f64 / secs,
                    recorder.bytes.saturating_sub(then_bytes) as f64 / secs,
                )
            }
            None => (0.0, 0.0),
        };
        last = Some((now, frame_count, recorder.bytes));

        let (disk_free, disk_total) = disk_space(Path::new(&recorder.dir)).unwrap_or_default();
        let (mem_available, mem_total) = memory().unwrap_or_default();

        status.send_replace(
            json!({
                "type": "status",
                "controller": { "connected": input_rate > 0.0, "rate": input_rate },
                "recorder": recorder,
                "write_rate": write_rate,
                "dropped": dropped.load(Ordering::Relaxed),
                "disk": { "free": disk_free, "total": disk_total },
                "memory": { "available": mem_available, "total": mem_total },
            })
            .to_string(),
        );
    }
}

/// Free and total bytes of the filesystem holding `path`
fn disk_space(path: &Path) -> Option<(u64, u64)> {
    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs only writes into the zeroed struct we own
    let mut stat: libc::statvfs = unsafe { core::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    let block = stat.f_frsize;
    Some((stat.f_bavail * block, stat.f_blocks * block))
}

/// Available and total system memory in bytes, from /proc/meminfo
fn memory() -> Option<(u64, u64)> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let field = |name: &str| {
        meminfo.lines().find_map(|line| {
            let kib = line.strip_prefix(name)?.trim().strip_suffix("kB")?;
            Some(kib.trim().parse::<u64>().ok()? * 1024)
        })
    };
    Some((field("MemAvailable:")?, field("MemTotal:")?))
}
