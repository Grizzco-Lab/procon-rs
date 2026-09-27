//! Studio dashboard server: live controller view, video preview, recording controls
//!
//! - `GET /favicon.ico`: the app icon (`web/icons/app-studio.svg`); unknown
//!   paths answer 404, known ones asked with another method 405
//! - `GET /` and every app path (`/studio`, `/inspect/...`, `/cuttlefish/...`,
//!   `/vision/...`, `/predictor/...`, see [`APPS`]): the page, which shows
//!   the app its path names; `/style.css`, `/app.js`, `/controller3d.js`,
//!   `/inspect.js`, `/sketch.js`, `/label.js`, `/cuttlefish.js`, `/knowledge.js`,
//!   `/translate.js`, `/source.js`, `/pedia.js`, `/stages.js`, `/vision.js`,
//!   `/predictor.js`, `/i18n.js`, `/i18n-zh.js`:
//!   the page, embedded from `web/`
//! - `GET /ws`: WebSocket pushing `{"type":"state"}` text for every input
//!   report, `{"type":"status"}` text twice a second, and the video preview
//!   as binary fragmented-MP4 messages (an init segment, then one per frame)
//! - `POST /api/command`: a [`Command`] such as `{"action":"start"}`, answered
//!   with `{"recorder": ..., "replay": ...}` or `{"error": "..."}`
//! - `GET /api/inspect/...`: the Inkspector app's data, see [`crate::inspect`];
//!   errors are `400` with `{"error": "..."}`; `/api/inspect/follow/...`:
//!   Follow in its labeling mode, see [`crate::follow`]
//! - `/api/cuttlefish/...`: the Cuttlefish app's reviews, videos and
//!   knowledge, see [`crate::cuttlefish`] and [`crate::knowledge`]
//! - `/api/vision/...`: the Vision app's runs and results, see
//!   [`crate::vision`]
//! - `/api/predictor/...`: the Predictor app's runs and predictions, see
//!   [`crate::predictor`]

use crate::config::WebConfig;
use crate::cuttlefish::{self, Cuttlefish};
use crate::dump::{Dumper, Frame};
use crate::follow::{self, Follow};
use crate::inspect::Inspector;
use crate::motion::Orientation;
use crate::parser::ProConParser;
use crate::predictor::{self, Predictor};
use crate::studio::{Command, Studio};
use crate::video::{ChunkKind, PreviewChunk};
use crate::vision::{self, Vision};
use alloc::collections::VecDeque;
use alloc::ffi::CString;
use alloc::sync::Arc;
use anyhow::Result;
use core::sync::atomic::Ordering;
use core::time::Duration;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
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
#[allow(clippy::too_many_arguments)]
pub async fn serve(
    feed: LiveFeed,
    studio: Arc<Studio>,
    inspector: Arc<Inspector>,
    cuttlefish: Arc<Cuttlefish>,
    vision: Arc<Vision>,
    predictor: Arc<Predictor>,
    follow: Arc<Follow>,
    web: &WebConfig,
) {
    let port = web.port;
    let status = watch::Sender::new(String::new());
    tokio::spawn(publish_status(Arc::clone(&studio), status.clone()));

    let index = warp::path::end().and(warp::get()).map(page);
    // The page again under every app path, after the asset and API routes;
    // other paths never get the page
    let app_pages = warp::path::param::<String>()
        .and(warp::path::tail())
        .and_then(|app: String, _: warp::path::Tail| async move {
            if APPS.contains(&app.as_str()) {
                Ok(page())
            } else {
                Err(warp::reject::not_found())
            }
        })
        .and(warp::get());
    let style = warp::path!("style.css")
        .and(warp::get())
        .map(|| asset(include_str!("../web/style.css"), "text/css; charset=utf-8"));
    let script = warp::path!("app.js").and(warp::get()).map(|| {
        asset(
            include_str!("../web/app.js"),
            "text/javascript; charset=utf-8",
        )
    });
    // The app icon, for browsers that ask without reading the page's link
    let favicon = warp::path!("favicon.ico").and(warp::get()).map(|| {
        let icon = ICONS
            .get_file("app-studio.svg")
            .map_or(&[][..], |f| f.contents());
        warp::reply::with_header(icon, "content-type", "image/svg+xml")
    });
    let model = warp::path!("controller3d.js").and(warp::get()).map(|| {
        asset(
            include_str!("../web/controller3d.js"),
            "text/javascript; charset=utf-8",
        )
    });
    let inspect_script = warp::path!("inspect.js").and(warp::get()).map(|| {
        asset(
            include_str!("../web/inspect.js"),
            "text/javascript; charset=utf-8",
        )
    });

    // The drawing layer, the shared video player, the stage map links, the
    // Inkspector's labeling mode, the Cuttlefish app with its knowledge,
    // translate and Pedia views and its source popover, the Vision app, the
    // Predictor and the page's dictionaries
    let scripts = warp::path!(String).and_then(|name: String| async move {
        let body = match name.as_str() {
            "sketch.js" => include_str!("../web/sketch.js"),
            "player.js" => include_str!("../web/player.js"),
            "stages.js" => include_str!("../web/stages.js"),
            "label.js" => include_str!("../web/label.js"),
            "cuttlefish.js" => include_str!("../web/cuttlefish.js"),
            "knowledge.js" => include_str!("../web/knowledge.js"),
            "translate.js" => include_str!("../web/translate.js"),
            "source.js" => include_str!("../web/source.js"),
            "pedia.js" => include_str!("../web/pedia.js"),
            "vision.js" => include_str!("../web/vision.js"),
            "predictor.js" => include_str!("../web/predictor.js"),
            "i18n.js" => include_str!("../web/i18n.js"),
            "i18n-zh.js" => include_str!("../web/i18n-zh.js"),
            _ => return Err(warp::reject::not_found()),
        };
        Ok(asset(body, "text/javascript; charset=utf-8"))
    });

    // The icon set with its gallery (`/icons/`), and the artwork, theme and
    // font of the Salmon Run theme (`/art/`)
    let icons = embedded_dir("icons", &ICONS).and(warp::get());
    let art = embedded_dir("art", &ART).and(warp::get());

    let delay_inspector = Arc::clone(&inspector);
    let objects_inspector = Arc::clone(&inspector);
    // Inkspector data reads files and runs ffmpeg; keep that off the async workers
    let inspect = warp::path!("api" / "inspect" / String)
        .and(warp::get())
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

    // A session's delay set by hand, written to the calibration file
    let delay = warp::path!("api" / "inspect" / "delay")
        .and(warp::post())
        .and(warp::body::content_length_limit(1024))
        .and(warp::body::json())
        .map(move |body: Value| {
            let session = body["s"].as_str().unwrap_or_default().to_string();
            let delay_ms = if body["remove"] == true {
                None
            } else {
                body["video_delay_ms"].as_f64()
            };
            let result = if delay_ms.is_none() && body["remove"] != true {
                Err(anyhow::anyhow!("give video_delay_ms or remove"))
            } else {
                tokio::task::block_in_place(|| delay_inspector.set_delay(&session, delay_ms))
            };
            match result {
                Ok(calibration) => {
                    warp::reply::with_status(warp::reply::json(&calibration), StatusCode::OK)
                }
                Err(e) => warp::reply::with_status(
                    warp::reply::json(&json!({ "error": format!("{e:#}") })),
                    StatusCode::BAD_REQUEST,
                ),
            }
        });

    // A frame's object labels, from the Inkspector's labeling mode
    let objects = warp::path!("api" / "inspect" / "objects")
        .and(warp::post())
        .and(warp::body::content_length_limit(1 << 20))
        .and(warp::body::json())
        .and_then(move |body: Value| {
            let inspector = Arc::clone(&objects_inspector);
            async move {
                let result =
                    tokio::task::spawn_blocking(move || inspector.save_objects(&body)).await;
                let (status, reply) = match result {
                    Ok(Ok(saved)) => (StatusCode::OK, saved),
                    Ok(Err(e)) => (
                        StatusCode::BAD_REQUEST,
                        json!({ "error": format!("{e:#}") }),
                    ),
                    Err(e) => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        json!({ "error": e.to_string() }),
                    ),
                };
                Ok::<_, core::convert::Infallible>(warp::reply::with_status(
                    warp::reply::json(&reply),
                    status,
                ))
            }
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

    // Each route checks its path before its method, so an unknown path is a
    // 404 and only a known one asked with the wrong method is a 405; the
    // apps' routes check the method first and are gated by their prefix
    let routes = index
        .or(favicon)
        .or(style)
        .or(script)
        .or(model)
        .or(inspect_script)
        .or(scripts.and(warp::get()))
        .or(icons)
        .or(art)
        .or(inspect)
        .or(app_pages)
        .or(websocket)
        .or(api)
        .or(delay)
        .or(objects)
        .or(under("api/inspect/follow").and(follow::routes(follow)))
        .or(under("api/cuttlefish").and(cuttlefish::routes(cuttlefish)))
        .or(under("api/vision").and(vision::routes(vision)))
        .or(under("api/predictor").and(predictor::routes(predictor)));
    let routes = same_origin(web.allowed_hosts.clone())
        .and(routes)
        .recover(forbidden);

    log::info!("Dashboard on http://0.0.0.0:{}", port);
    warp::serve(routes).run(([0, 0, 0, 0], port)).await;
}

/// A request from another site, or for a host name the dashboard does not
/// serve
#[derive(Debug)]
struct Forbidden(String);

impl warp::reject::Reject for Forbidden {}

/// Only requests for our own host from our own pages: the `Host` must be
/// `localhost`, an IP address or one of `allowed`, which stops DNS
/// rebinding, and an `Origin`, when the browser sends one, must be that same
/// host, which stops other sites posting commands (CSRF)
fn same_origin(allowed: Vec<String>) -> impl Filter<Extract = (), Error = warp::Rejection> + Clone {
    warp::header::optional::<String>("host")
        .and(warp::header::optional::<String>("origin"))
        .and_then(move |host: Option<String>, origin: Option<String>| {
            let verdict = check_origin(host.as_deref(), origin.as_deref(), &allowed);
            async move { verdict.map_err(|e| warp::reject::custom(Forbidden(e))) }
        })
        .untuple_one()
}

/// Whether a request's `Host` and `Origin` headers are acceptable
fn check_origin(
    host: Option<&str>,
    origin: Option<&str>,
    allowed: &[String],
) -> Result<(), String> {
    let host = host.ok_or("no Host header")?;
    let name = match host.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(rest),
        None => host.rsplit_once(':').map_or(host, |(name, _)| name),
    };
    let known = name.eq_ignore_ascii_case("localhost")
        || name.parse::<std::net::IpAddr>().is_ok()
        || allowed.iter().any(|a| a.eq_ignore_ascii_case(name));
    if !known {
        return Err(format!(
            "host {name} is not served here; add it to [web] allowed_hosts"
        ));
    }
    match origin {
        None => Ok(()),
        Some(origin) => {
            let authority = origin.split_once("://").map(|(_, rest)| rest);
            match authority {
                Some(authority) if authority.eq_ignore_ascii_case(host) => Ok(()),
                _ => Err(format!("requests from {origin} are not accepted")),
            }
        }
    }
}

/// Answer refused requests with 403 and let other rejections through
async fn forbidden(rejection: warp::Rejection) -> Result<impl warp::Reply, warp::Rejection> {
    match rejection.find::<Forbidden>() {
        Some(Forbidden(reason)) => {
            log::warn!("Refused a request: {reason}");
            Ok(warp::reply::with_status(
                reason.clone(),
                StatusCode::FORBIDDEN,
            ))
        }
        None => Err(rejection),
    }
}

/// Requests whose path is `prefix` or under it, for routes that check their
/// method first: those reject every other path with 405, not 404
fn under(prefix: &'static str) -> impl Filter<Extract = (), Error = warp::Rejection> + Clone {
    warp::path::full()
        .and_then(move |path: warp::path::FullPath| async move {
            let rest = path.as_str().trim_start_matches('/');
            match rest.strip_prefix(prefix) {
                Some("") => Ok(()),
                Some(tail) if tail.starts_with('/') => Ok(()),
                _ => Err(warp::reject::not_found()),
            }
        })
        .untuple_one()
}

/// The icon set (`web/icons/`): SVGs and the gallery page
static ICONS: include_dir::Dir = include_dir::include_dir!("$CARGO_MANIFEST_DIR/web/icons");

/// Artwork for the Salmon Run theme (`web/art/`): illustrations, background
/// tiles, the theme's stylesheet and its font
static ART: include_dir::Dir = include_dir::include_dir!("$CARGO_MANIFEST_DIR/web/art");

/// Serve an embedded folder under `/<prefix>/`; the bare folder is its `index.html`
fn embedded_dir(
    prefix: &'static str,
    dir: &'static include_dir::Dir<'static>,
) -> impl Filter<Extract = (impl warp::Reply,), Error = warp::Rejection> + Clone {
    warp::path(prefix)
        .and(warp::path::tail())
        .and_then(move |tail: warp::path::Tail| async move {
            let name = match tail.as_str() {
                "" => "index.html",
                name => name,
            };
            let file = dir.get_file(name).ok_or_else(warp::reject::not_found)?;
            let content_type = match name.rsplit_once('.') {
                Some((_, "svg")) => "image/svg+xml",
                Some((_, "js")) => "text/javascript; charset=utf-8",
                Some((_, "css")) => "text/css; charset=utf-8",
                Some((_, "ttf")) => "font/ttf",
                Some((_, "txt")) => "text/plain; charset=utf-8",
                _ => "text/html; charset=utf-8",
            };
            Ok::<_, warp::Rejection>(warp::reply::with_header(
                file.contents(),
                "content-type",
                content_type,
            ))
        })
}

/// The apps the page holds, each at `/<app>` with its state after it (see
/// `appUrl` in `web/app.js`)
const APPS: [&str; 5] = ["studio", "inspect", "cuttlefish", "vision", "predictor"];

/// The dashboard page
fn page() -> warp::reply::Html<&'static str> {
    warp::reply::html(include_str!("../web/index.html"))
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

#[cfg(test)]
mod tests {
    use super::check_origin;

    #[test]
    fn hosts_and_origins() {
        let allowed = [String::from("studio.example.com")];
        let ok = |host, origin| check_origin(Some(host), origin, &allowed).is_ok();
        // Same-origin pages, with or without an Origin header
        assert!(ok("localhost:8090", None));
        assert!(ok("192.168.4.20:8090", Some("http://192.168.4.20:8090")));
        assert!(ok("[::1]:8090", Some("http://[::1]:8090")));
        assert!(ok("studio.example.com", Some("https://studio.example.com")));
        // DNS rebinding: an unknown name pointing at us
        assert!(!ok("evil.example.net:8090", None));
        // CSRF: another site posting to us
        assert!(!ok("localhost:8090", Some("https://evil.example.net")));
        assert!(!ok("localhost:8090", Some("null")));
        assert!(check_origin(None, None, &allowed).is_err());
    }
}
