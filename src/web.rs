//! Studio dashboard server: live controller view, video preview, recording controls
//!
//! - `GET /`, `/style.css`, `/app.js`, `/controller3d.js`: the page, embedded from `web/`
//! - `GET /ws`: WebSocket pushing `{"type":"state"}` text for every input
//!   report, `{"type":"status"}` text twice a second, and the video preview
//!   as binary fragmented-MP4 messages (an init segment, then one per frame)
//! - `POST /api/command`: a [`Command`] such as `{"action":"start"}`, answered
//!   with `{"recorder": ..., "replay": ...}` or `{"error": "..."}`
//! - `GET /api/inspect/...`: the Inspector app's data, see [`crate::inspect`];
//!   errors are `400` with `{"error": "..."}`

use crate::dump::{Dumper, Frame};
use crate::inspect::Inspector;
use crate::motion::Orientation;
use crate::parser::ProConParser;
use crate::studio::{Command, Studio};
use crate::video::{ChunkKind, PreviewChunk};
use alloc::collections::VecDeque;
use alloc::ffi::CString;
use alloc::sync::Arc;
use anyhow::Result;
use core::sync::atomic::Ordering;
use core::time::Duration;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::collections::HashMap;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::Instant;
use tokio::sync::{broadcast, watch};
use warp::Filter;
use warp::http::StatusCode;
use warp::ws::{Message, WebSocket};

/// Dumper that publishes parsed input reports to dashboard clients
#[derive(Clone, Default)]
pub struct LiveFeed {
    /// Latest controller state as JSON
    state: watch::Sender<String>,
    /// Pose for Splatoon mode; tracked on every report, watched or not
    orientation: Orientation,
}

impl LiveFeed {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Dumper for LiveFeed {
    fn dump(&mut self, frame: &Frame) -> Result<()> {
        let data = frame.payload();
        if data.first() != Some(&0x30) {
            return Ok(());
        }
        let Ok(state) = ProConParser::parse_input_report(data) else {
            return Ok(());
        };
        self.orientation.update(&state, frame.timestamp_ms);

        // Skip serializing while no browser is watching
        if self.state.receiver_count() > 0 {
            let message = json!({
                "type": "state",
                "state": state,
                "orientation": self.orientation.quaternion(),
            });
            self.state.send_replace(message.to_string());
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Serve the dashboard on all interfaces
pub async fn serve(feed: LiveFeed, studio: Arc<Studio>, inspector: Arc<Inspector>, port: u16) {
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
    let model = warp::path!("controller3d.js").map(|| {
        asset(
            include_str!("../web/controller3d.js"),
            "text/javascript; charset=utf-8",
        )
    });
    let inspect_script = warp::path!("inspect.js").map(|| {
        asset(
            include_str!("../web/inspect.js"),
            "text/javascript; charset=utf-8",
        )
    });

    // Inspector data reads files and runs ffmpeg; keep that off the async workers
    let inspect = warp::path!("api" / "inspect" / String)
        .and(warp::query::<HashMap<String, String>>())
        .and(warp::header::optional::<String>("range"))
        .and_then(
            move |endpoint: String, query: HashMap<String, String>, range: Option<String>| {
                let inspector = Arc::clone(&inspector);
                async move {
                    let result = tokio::task::spawn_blocking(move || {
                        inspector.handle(&endpoint, &query, range.as_deref())
                    })
                    .await;
                    let reply = match result {
                        Ok(Ok(reply)) => {
                            let builder = warp::http::Response::builder()
                                .header("content-type", reply.content_type)
                                .header("accept-ranges", "bytes");
                            match reply.content_range {
                                Some(range) => builder
                                    .status(StatusCode::PARTIAL_CONTENT)
                                    .header("content-range", range),
                                None => builder,
                            }
                            .body(reply.body)
                        }
                        Ok(Err(e)) => warp::http::Response::builder()
                            .status(StatusCode::BAD_REQUEST)
                            .header("content-type", "application/json")
                            .body(
                                json!({ "error": format!("{e:#}") })
                                    .to_string()
                                    .into_bytes(),
                            ),
                        Err(e) => warp::http::Response::builder()
                            .status(StatusCode::INTERNAL_SERVER_ERROR)
                            .body(e.to_string().into_bytes()),
                    };
                    Ok::<_, core::convert::Infallible>(reply.unwrap())
                }
            },
        );

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
                    let reply = json!({
                        "recorder": studio.recorder.status(),
                        "replay": studio.player.status(),
                    });
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
        .and(
            index
                .or(style)
                .or(script)
                .or(model)
                .or(inspect_script)
                .or(inspect),
        )
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
    (init, mut preview): (Option<PreviewChunk>, broadcast::Receiver<PreviewChunk>),
) {
    let (mut tx, mut rx) = socket.split();
    // Send the current status right away instead of waiting for the next tick
    status.mark_changed();
    let mut ping = tokio::time::interval(Duration::from_secs(30));

    // The player needs the init segment first, then frames from a keyframe on
    if let Some(init) = init
        && tx.send(Message::binary(init.data.to_vec())).await.is_err()
    {
        return;
    }
    let mut synced = false;

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
            chunk = preview.recv() => match chunk {
                Ok(chunk) => {
                    match chunk.kind {
                        // A new stream: start over from its next keyframe
                        ChunkKind::Init => synced = false,
                        ChunkKind::Key => synced = true,
                        ChunkKind::Delta if !synced => continue,
                        ChunkKind::Delta => {}
                    }
                    Message::binary(chunk.data.to_vec())
                }
                // Missed frames would not decode; wait for the next keyframe
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    synced = false;
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
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

/// How often the dashboard gets a status snapshot
const STATUS_EVERY: Duration = Duration::from_millis(500);

/// Rates are averaged over this many ticks (3 s), smoothing out muxer flushes
const RATE_WINDOW: usize = 7;

/// Re-count the size of every session this often; it walks the folder
const ALL_SESSIONS_EVERY: Duration = Duration::from_secs(10);

/// Counters at one status tick, for rates
struct Sample {
    at: Instant,
    frames: u64,
    controller_bytes: u64,
    video_bytes: u64,
}

/// Publish a status snapshot twice a second
async fn publish_status(studio: Arc<Studio>, status: watch::Sender<String>) {
    let mut tick = tokio::time::interval(STATUS_EVERY);
    let mut history: VecDeque<Sample> = VecDeque::new();
    let mut other_sessions: Option<(Instant, u64)> = None;

    loop {
        tick.tick().await;

        let recount = other_sessions.is_none_or(|(at, _)| at.elapsed() >= ALL_SESSIONS_EVERY);
        let snapshot = {
            let studio = Arc::clone(&studio);
            // Status reads files and locks shared by blocking code
            tokio::task::spawn_blocking(move || {
                (
                    studio.recorder.status(),
                    studio.video.status(),
                    studio.video_bytes(),
                    recount.then(|| studio.other_sessions_bytes()),
                    disk_space(&studio.recorder.prefix_dir()),
                )
            })
            .await
        };
        let Ok((recorder, video, video_bytes, recounted, disk)) = snapshot else {
            continue;
        };
        if let Some(bytes) = recounted {
            other_sessions = Some((Instant::now(), bytes));
        }

        let link = &studio.link;
        let now = Sample {
            at: Instant::now(),
            frames: link.frames.load(Ordering::Relaxed),
            controller_bytes: recorder.bytes,
            video_bytes,
        };
        let then = history.front().unwrap_or(&now);
        let secs = now.at.duration_since(then.at).as_secs_f64();
        let rate = |now: u64, then: u64| {
            if secs > 0.0 {
                now.saturating_sub(then) as f64 / secs
            } else {
                0.0
            }
        };
        let input_rate = rate(now.frames, then.frames);
        let controller_rate = rate(now.controller_bytes, then.controller_bytes);
        let video_rate = rate(now.video_bytes, then.video_bytes);
        history.push_back(now);
        if history.len() > RATE_WINDOW {
            history.pop_front();
        }

        let (disk_free, disk_total) = disk.unwrap_or_default();
        let (mem_available, mem_total) = memory().unwrap_or_default();

        status.send_replace(
            json!({
                "type": "status",
                "link": {
                    "address": studio.proxy_address,
                    "connected": link.connected.load(Ordering::Relaxed),
                    "input_rate": input_rate,
                    "dropped": link.dropped.load(Ordering::Relaxed),
                    "clock_offset_ms": link.clock_offset_ms.load(Ordering::Relaxed),
                    // Time reports spend in the proxy since the last status, in µs
                    "forward_us": link.take_forward_us()
                        .map(|(mean, max)| json!({ "mean": mean, "max": max })),
                },
                "recorder": recorder,
                "game_settings": studio.game_settings(),
                "replay": studio.player.status(),
                "video": video,
                // Bytes per second, and bytes of the current or last session
                "rates": { "controller": controller_rate, "video": video_rate },
                "sizes": {
                    "controller": recorder.bytes,
                    "video": video_bytes,
                    // Earlier sessions, recounted now and then, plus this one live
                    "all_sessions": other_sessions
                        .map(|(_, bytes)| bytes + recorder.bytes + video_bytes),
                },
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
