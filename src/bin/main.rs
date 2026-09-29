//! Studio host: dashboard, video capture and session recording
//!
//! Runs on the machine with the capture card. It receives controller frames
//! from `procon-proxy` (on the Raspberry Pi), captures video with ffmpeg and records both
//! into session folders.

use alloc::sync::Arc;
use clap::Parser;
use procon::config::{self, StudioConfig};
use procon::cuttlefish::Cuttlefish;
use procon::dump::MultiDumper;
use procon::follow::{self, Follow};
use procon::inspect::Inspector;
use procon::pipeline::{self, Pipeline};
use procon::player::Player;
use procon::predictor::online::{Bot, Online};
use procon::predictor::{self, Predictor};
use procon::recorder::{Recorder, RecorderState};
use procon::stream::{self, LinkStats};
use procon::studio::{Command, SavedState, Studio};
use procon::video::Video;
use procon::vision::{self, Vision};
use procon::web::{self, LiveFeed};
use std::path::{Path, PathBuf};

extern crate alloc;

/// Nintendo Switch Pro Controller recording studio
#[derive(Parser)]
#[command(name = "procon")]
#[command(about = "Dashboard, video capture and recording for the Pro Controller proxy")]
struct Args {
    /// Path to configuration file; dashboard settings are saved next to it
    #[arg(short, long, default_value = "config.toml")]
    config: String,
}

/// Whether a log record is the web server reporting a client that went
/// away mid-request: a browser aborting a video range request when seeking,
/// a frame image it no longer needs, or a slow request cut off by a reload.
/// Those are expected, so they are logged at debug level, not as errors.
fn is_client_abort(target: &str, message: &str) -> bool {
    target.starts_with("warp::server")
        && message.starts_with("server connection error")
        && ["IncompleteMessage", "ConnectionReset", "BrokenPipe"]
            .iter()
            .any(|kind| message.contains(kind))
}

/// env_logger, with [`is_client_abort`] records moved down to debug level
struct Logger(env_logger::Logger);

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        self.0.enabled(metadata)
    }

    fn log(&self, record: &log::Record) {
        if record.level() == log::Level::Error {
            let message = record.args().to_string();
            if is_client_abort(record.target(), &message) {
                let debug = log::Metadata::builder()
                    .level(log::Level::Debug)
                    .target(record.target())
                    .build();
                if self.0.enabled(&debug) {
                    self.0.log(
                        &log::Record::builder()
                            .args(format_args!("{message} (the client closed the connection)"))
                            .metadata(debug)
                            .build(),
                    );
                }
                return;
            }
        }
        self.0.log(record);
    }

    fn flush(&self) {
        self.0.flush();
    }
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let config: StudioConfig = config::load(&args.config)?;
    config.logging.validate()?;
    let logger = env_logger::builder()
        .filter_level(config.logging.level.parse()?)
        .build();
    log::set_max_level(logger.filter());
    log::set_boxed_logger(Box::new(Logger(logger)))?;

    // Settings chosen on the dashboard win over the config file
    let state_path = Path::new(&args.config).with_extension("state.json");
    let saved = SavedState::load(&state_path);
    let prefix = saved.prefix.unwrap_or(config.recording.prefix);
    let input = saved
        .video_input
        .unwrap_or_else(|| config.video.input.clone());

    let mut video_config = config.video;
    if let Some(height) = saved.video_height {
        video_config.record_height = height;
    }
    if let Some(fps) = saved.video_fps {
        video_config.record_fps = fps;
    }

    let recorder = Recorder::new(&prefix);
    let video = Video::new(
        video_config,
        Some(input).filter(|id| !id.is_empty()),
        saved.preview_matches_recording.unwrap_or(false),
        saved.record_audio.unwrap_or(true),
    );
    let link = Arc::new(LinkStats::default());
    // The Predictor's online mode plays the Switch through the same port,
    // held to what it may press (a broken saved setting means the defaults)
    let limits = saved
        .bot_limits
        .filter(|limits| limits.validate().is_ok())
        .unwrap_or_default();
    let bot = Bot::new(
        config.proxy.replay_address.clone(),
        limits,
        Arc::clone(&link),
    );
    let player = Player::new(
        config.proxy.replay_address,
        saved.replay_mix.unwrap_or(false),
        saved.replay_path,
    );
    let feed = LiveFeed::new();

    // Frames from the proxy go to the recorder and the live view, and to the
    // bot, which measures a person's tapping from them
    let mut pipeline = MultiDumper::new();
    pipeline.add_dumper(Box::new(recorder.clone()));
    pipeline.add_dumper(Box::new(feed.clone()));
    pipeline.add_dumper(Box::new(bot.clone()));
    let address = config.proxy.address.clone();
    let receiver_link = Arc::clone(&link);
    std::thread::spawn(move || stream::receive_frames(&address, &mut pipeline, &receiver_link));

    let studio = Arc::new(Studio::new(
        recorder,
        video,
        player,
        bot,
        link,
        config.proxy.address,
        state_path,
        saved.game_settings.unwrap_or_default(),
        saved.techniques.unwrap_or_default(),
        saved.record_bot_runs.unwrap_or(true),
    ));
    // The last replay file may sit on a slow network mount: the dashboard
    // does not wait for it
    let restoring = Arc::clone(&studio);
    std::thread::spawn(move || restoring.player.restore());

    // Relative Inspector paths start at the config file's folder
    let config_dir = Path::new(&args.config)
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf();
    let follow_settings = follow::Settings::from_config(&config.inspect, &config_dir)?;
    let calibration = config
        .inspect
        .calibration
        .as_deref()
        .unwrap_or("../AgentZero/calibration.json");
    let root = config.inspect.root.map(|root| config_dir.join(root));
    // Annotations, reviews, vision results and predictions sit next to the
    // sessions' folder by default
    let sessions = root.clone().unwrap_or_else(|| studio.recorder.prefix_dir());
    let beside = |name: &str| sessions.parent().unwrap_or(Path::new(".")).join(name);
    let annotations = config
        .inspect
        .annotations
        .map_or_else(|| beside("Annotations"), |dir| config_dir.join(dir));
    let inspector = Arc::new(Inspector::new(
        root,
        studio.recorder.clone(),
        config_dir.join(calibration),
        annotations,
    ));

    let reviews = config
        .cuttlefish
        .reviews
        .map_or_else(|| beside("Reviews"), |dir| config_dir.join(dir));
    let knowledge = config
        .cuttlefish
        .knowledge
        .map_or_else(|| beside("Knowledge"), |dir| config_dir.join(dir));
    // Our entries of the store of before (in ~/.local/share/cuttlefish, a
    // folder another program owns) are copied over once and, when the copy
    // checks out, moved into a procon-migrated-*.safe-to-delete folder there
    if let Err(e) = cuttlefish::store::migrate(&cuttlefish::store::legacy_root(), &knowledge) {
        log::warn!("Could not bring the older knowledge store over: {:#}", e);
    }
    let settings = cuttlefish::llm::Settings {
        model: config.cuttlefish.model,
        backend: match config.cuttlefish.backend {
            Some(name) => name
                .parse()
                .map_err(|e| anyhow::anyhow!("[cuttlefish] backend: {e}"))?,
            None => cuttlefish::llm::Backend::Auto,
        },
        ..Default::default()
    };
    let predictor_settings =
        predictor::Settings::from_config(config.predictor, &config_dir, beside("Predictions"));
    let cuttlefish = Arc::new(Cuttlefish::new(
        Arc::clone(&inspector),
        reviews,
        knowledge,
        predictor_settings.results.clone(),
        settings,
        config.cuttlefish.translate_model,
        procon::knowledge::AutoApply {
            on: config.cuttlefish.slang_auto_apply.unwrap_or(true),
            threshold: config
                .cuttlefish
                .slang_threshold
                .unwrap_or(cuttlefish::slang::DEFAULT_THRESHOLD)
                .clamp(0.0, 1.0),
        },
    ));
    // Reviews of the older layout move into folders, with their YouTube
    // videos from the download cache of before, then the reviews list is
    // read for the library; on a thread, since the reviews may sit on a
    // network mount (and a video is moved across)
    let legacy_cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join("procon-cuttlefish");
    let migrating = Arc::clone(&cuttlefish);
    std::thread::spawn(move || {
        if let Err(e) = migrating.migrate(&legacy_cache) {
            log::warn!("Could not move reviews into folders: {:#}", e);
        }
        if let Err(e) = migrating.warm() {
            log::warn!("Could not read the reviews: {:#}", e);
        }
    });

    let vision = Arc::new(Vision::new(
        Arc::clone(&inspector),
        vision::Settings::from_config(config.vision, &config_dir, beside("Vision"))?,
    ));

    // The Pipeline's queue sits in the AgentZero folder by default
    let agentzero = predictor_settings.agentzero.clone();
    let predictor = Arc::new(Predictor::new(
        Arc::clone(&inspector),
        Arc::clone(&cuttlefish),
        predictor_settings,
    ));
    let online = Online::new(Arc::clone(&predictor), Arc::clone(&studio));
    let follow = Arc::new(Follow::new(Arc::clone(&inspector), follow_settings));
    // The GPU and the experiment queue, sampled from now on for the timeline
    let pipeline = Pipeline::start(pipeline::Settings::from_config(
        config.pipeline,
        &config_dir,
        &agentzero,
    ));

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        tokio::select! {
            _ = web::serve(feed, Arc::clone(&studio), inspector, cuttlefish, Arc::clone(&vision), Arc::clone(&predictor), Arc::clone(&online), Arc::clone(&follow), pipeline, &config.web) => {}
            _ = tokio::signal::ctrl_c() => {
                // AgentZero may be playing the Switch: the controller first
                tokio::task::block_in_place(|| online.shutdown());
                // Let ffmpeg finish the video file and session.json get its end time
                if studio.recorder.status().state != RecorderState::Idle {
                    log::info!("Stopping the recording before exit");
                    if let Err(e) = tokio::task::block_in_place(|| studio.run(Command::Stop)) {
                        log::error!("Failed to stop recording: {:#}", e);
                    }
                }
            }
        }
    });
    // A tracker, detector, prediction or AgentZero started from the page
    // ends with the studio
    online.shutdown();
    follow.stop_service();
    vision.stop_detector();
    predictor.stop();
    // Stops ffmpeg even when idle
    studio.video.set_input(None)?;
    studio.video.stop_audio();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_aborts_are_told_apart() {
        let target = "warp::server::run";
        for message in [
            "server connection error: hyper::Error(IncompleteMessage)",
            "server connection error: hyper::Error(Io, Os { code: 104, kind: ConnectionReset, message: \"Connection reset by peer\" })",
            "server connection error: hyper::Error(Io, Os { code: 32, kind: BrokenPipe, message: \"Broken pipe\" })",
        ] {
            assert!(is_client_abort(target, message), "{message}");
        }
        // Other server errors, and other modules, stay errors
        assert!(!is_client_abort(
            target,
            "server connection error: hyper::Error(HeaderTimeout)"
        ));
        assert!(!is_client_abort(
            "procon::web",
            "server connection error: hyper::Error(IncompleteMessage)"
        ));
    }
}
