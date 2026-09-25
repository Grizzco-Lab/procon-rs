//! Studio host: dashboard, video capture and session recording
//!
//! Runs on the machine with the capture card. It receives controller frames
//! from `procon-proxy` (on the Raspberry Pi), captures video with ffmpeg and records both
//! into session folders.

use alloc::sync::Arc;
use clap::Parser;
use procon::config::{self, StudioConfig};
use procon::dump::MultiDumper;
use procon::player::Player;
use procon::recorder::{Recorder, RecorderState};
use procon::stream::{self, LinkStats};
use procon::studio::{Command, SavedState, Studio};
use procon::video::Video;
use procon::web::{self, LiveFeed};
use std::path::Path;

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

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let config: StudioConfig = config::load(&args.config)?;
    config.logging.validate()?;
    env_logger::builder()
        .filter_level(config.logging.level.parse()?)
        .init();

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
    let player = Player::new(
        config.proxy.replay_address,
        saved.replay_mix.unwrap_or(false),
    );
    // The file may be gone since; then the panel starts empty
    if let Some(path) = saved.replay_path
        && let Err(e) = player.load(&path)
    {
        log::warn!("Could not reload replay file: {:#}", e);
    }
    let link = Arc::new(LinkStats::default());
    let feed = LiveFeed::new();

    // Frames from the proxy go to the recorder and the live view
    let mut pipeline = MultiDumper::new();
    pipeline.add_dumper(Box::new(recorder.clone()));
    pipeline.add_dumper(Box::new(feed.clone()));
    let address = config.proxy.address.clone();
    let receiver_link = Arc::clone(&link);
    std::thread::spawn(move || stream::receive_frames(&address, &mut pipeline, &receiver_link));

    let studio = Arc::new(Studio::new(
        recorder,
        video,
        player,
        link,
        config.proxy.address,
        state_path,
        saved.game_settings.unwrap_or_default(),
    ));

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        tokio::select! {
            _ = web::serve(feed, Arc::clone(&studio), config.web.port) => {}
            _ = tokio::signal::ctrl_c() => {
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
    // Stops ffmpeg even when idle
    studio.video.set_input(None)?;
    Ok(())
}
