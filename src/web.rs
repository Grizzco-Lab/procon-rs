//! Studio dashboard server: live controller view, video preview, recording controls
//!
//! - `GET /`, `/style.css`, `/app.js`: the page, embedded from `web/`
//! - `GET /ws`: WebSocket pushing `{"type":"state"}` text for every input
//!   report, `{"type":"status"}` text once per second, and the video preview
//!   as binary JPEG messages
//! - `POST /api/command`: a [`Command`] such as `{"action":"start"}`, answered
//!   with `{"recorder": ...}` or `{"error": "..."}`

use crate::dump::{Dumper, Frame};
use crate::parser::ProConParser;
use crate::studio::{Command, Studio};
use alloc::collections::VecDeque;
use alloc::ffi::CString;
use alloc::sync::Arc;
use anyhow::Result;
use core::sync::atomic::Ordering;
use core::time::Duration;
use futures_util::{SinkExt, StreamExt};
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
}

impl LiveFeed {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Dumper for LiveFeed {
    fn dump(&mut self, frame: &Frame) -> Result<()> {
        let data = frame.payload();
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

/// Serve the dashboard on all interfaces
pub async fn serve(feed: LiveFeed, studio: Arc<Studio>, port: u16) {
    let status = watch::Sender::new(String::new());
    tokio::spawn(publish_status(Arc::clone(&studio), status.clone()));

    let index = warp::path::end().map(|| warp::reply::html(include_str!("../web/index.html")));
    let style = warp::path!("style.css")
        .map(|| asset(include_str!("../web/style.css"), "text/css; charset=utf-8"));
    let script = warp::path!("app.js").map(|| {
        asset(
            include_str!("../web/app.js"),
            "text/javascript; charset=utf-8",
        )
    });

    let video = studio.video.clone();
    let websocket = warp::path!("ws")
        .and(warp::ws())
        .map(move |ws: warp::ws::Ws| {
            let state = feed.state.subscribe();
            let status = status.subscribe();
            let preview = video.subscribe();
            ws.on_upgrade(move |socket| stream_to_client(socket, state, status, preview))
        });

    let api = warp::path!("api" / "command")
        .and(warp::post())
        .and(warp::body::content_length_limit(4096))
        .and(warp::body::json())
        .map(move |command: Command| {
            // Commands may wait for ffmpeg to restart; keep that off the async workers
            match tokio::task::block_in_place(|| studio.run(command)) {
                Ok(()) => {
                    let reply = json!({ "recorder": studio.recorder.status() });
                    warp::reply::with_status(warp::reply::json(&reply), StatusCode::OK)
                }
                Err(e) => {
                    log::warn!("Command failed: {:#}", e);
                    let error = json!({ "error": format!("{e:#}") });
                    warp::reply::with_status(warp::reply::json(&error), StatusCode::BAD_REQUEST)
                }
            }
        });

    let routes = warp::get()
        .and(index.or(style).or(script))
        .or(websocket)
        .or(api);

    log::info!("Dashboard on http://0.0.0.0:{}", port);
    warp::serve(routes).run(([0, 0, 0, 0], port)).await;
}

/// Reply with an embedded static file
fn asset(body: &'static str, content_type: &'static str) -> impl warp::Reply {
    warp::reply::with_header(body, "content-type", content_type)
}

/// Forward controller state, status and preview frames to one browser
async fn stream_to_client(
    socket: WebSocket,
    mut state: watch::Receiver<String>,
    mut status: watch::Receiver<String>,
    mut preview: watch::Receiver<Arc<Vec<u8>>>,
) {
    let (mut tx, mut rx) = socket.split();
    // Send the current status right away instead of waiting for the next tick
    status.mark_changed();
    let mut ping = tokio::time::interval(Duration::from_secs(30));

    loop {
        // A slow client simply skips intermediate states and frames
        let message = tokio::select! {
            changed = state.changed() => match changed {
                Ok(()) => Message::text(state.borrow_and_update().clone()),
                Err(_) => break,
            },
            changed = status.changed() => match changed {
                Ok(()) => Message::text(status.borrow_and_update().clone()),
                Err(_) => break,
            },
            changed = preview.changed() => match changed {
                Ok(()) => Message::binary(preview.borrow_and_update().to_vec()),
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

/// Status ticks the input and write rates are averaged over
const RATE_WINDOW: usize = 6;

/// Publish a status snapshot every second
async fn publish_status(studio: Arc<Studio>, status: watch::Sender<String>) {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    // (time, reports received, bytes recorded) over the last few ticks; video files
    // grow in bursts as the muxer flushes, so rates are averaged over this window
    let mut history: VecDeque<(Instant, u64, u64)> = VecDeque::new();

    loop {
        tick.tick().await;

        let snapshot = {
            let studio = Arc::clone(&studio);
            // Status reads files and locks shared by blocking code
            tokio::task::spawn_blocking(move || {
                (
                    studio.recorder.status(),
                    studio.video.status(),
                    disk_space(&studio.recorder.prefix_dir()),
                )
            })
            .await
        };
        let Ok((recorder, video, disk)) = snapshot else {
            continue;
        };

        let link = &studio.link;
        let now = Instant::now();
        let frames = link.frames.load(Ordering::Relaxed);
        let bytes = recorder.bytes + video.bytes;
        history.push_back((now, frames, bytes));
        if history.len() > RATE_WINDOW {
            history.pop_front();
        }
        let (then, then_frames, then_bytes) = history[0];
        let secs = now.duration_since(then).as_secs_f64();
        let (input_rate, write_rate) = if secs > 0.0 {
            (
                frames.saturating_sub(then_frames) as f64 / secs,
                bytes.saturating_sub(then_bytes) as f64 / secs,
            )
        } else {
            (0.0, 0.0)
        };

        let (disk_free, disk_total) = disk.unwrap_or_default();
        let (mem_available, mem_total) = memory().unwrap_or_default();

        status.send_replace(
            json!({
                "type": "status",
                "link": {
                    "address": studio.pi_address,
                    "connected": link.connected.load(Ordering::Relaxed),
                    "input_rate": input_rate,
                    "dropped": link.dropped.load(Ordering::Relaxed),
                    "clock_offset_ms": link.clock_offset_ms.load(Ordering::Relaxed),
                },
                "recorder": recorder,
                "video": video,
                "write_rate": write_rate,
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
