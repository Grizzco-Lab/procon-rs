//! USB proxy between the Pro Controller and the Switch, on the Raspberry Pi
//!
//! Resets the controller, presents itself to the Switch as a wired Pro
//! Controller (a USB gadget) and forwards reports both ways, streaming each
//! input report to the capture host and applying the actions replayed to it
//! (see the `procon-core` crate for the frames, the link and the replay format).

mod config;
mod device;
mod gadget;
mod priority;
mod proxy;
mod wake;

use anyhow::Context;
use clap::Parser;
use config::Config;
use gadget::ProConGadget;
use priority::set_high_priority;
use procon_core::dump::{AsyncDumper, MultiDumper};
use procon_core::recorder::Recorder;
use procon_core::replay::Replay;
use procon_core::stream::FrameStreamer;
use proxy::Proxy;

/// Nintendo Switch Pro Controller HID Proxy
#[derive(Parser)]
#[command(name = "procon-proxy")]
#[command(about = "A HID proxy for Nintendo Switch Pro Controller")]
struct Args {
    /// Path to configuration file
    #[arg(short, long, default_value = "proxy.toml")]
    config: String,
}

/// Initialize logging system with configured level
fn init_log(level: &str) -> anyhow::Result<()> {
    let log_level = match level.to_lowercase().as_str() {
        "error" => log::LevelFilter::Error,
        "warn" => log::LevelFilter::Warn,
        "info" => log::LevelFilter::Info,
        "debug" => log::LevelFilter::Debug,
        "trace" => log::LevelFilter::Trace,
        _ => {
            eprintln!("Warning: Invalid log level '{}', using 'info'", level);
            log::LevelFilter::Info
        }
    };

    env_logger::builder()
        .filter_level(log_level)
        .format_source_path(true)
        .init();

    Ok(())
}

fn main() -> anyhow::Result<()> {
    // Parse command line arguments
    let args = Args::parse();

    // Load configuration from specified file
    let config = Config::load_from_file(&args.config)?;
    config.validate()?;

    // Initialize logging with configured level
    init_log(&config.logging.level)?;

    log::info!("ProCon Proxy starting...");
    log::debug!("Configuration loaded: {:#?}", config);

    // Set high priority for main proxy thread
    set_high_priority(config.performance.enable_cpu_affinity);

    // A controller left idle may have connected to the console over Bluetooth;
    // a reset drops that, so the handshake that follows goes over USB
    log::info!("Resetting the Pro Controller so it talks over USB");
    if let Err(e) = device::reset() {
        log::warn!("Could not reset the Pro Controller: {:#}", e);
    }

    // Setup USB gadget programmatically
    let mut usb_gadget = ProConGadget::new();
    let hid_device_path = usb_gadget
        .setup()
        .context("Failed to setup USB gadget - ensure you have root privileges")?;
    log::info!("USB gadget configured at: {}", hid_device_path);

    // Create multi-dumper for async processing
    let mut multi_dumper = MultiDumper::new();

    // Stream frames to Grizzco Lab, which records them with the video
    multi_dumper.add_dumper(Box::new(FrameStreamer::listen(config.stream.port)?));

    // Optional local backup: one session from launch until exit
    if config.dump.autostart {
        let recorder = Recorder::new(config.dump.prefix.as_str());
        recorder.start("")?;
        multi_dumper.add_dumper(Box::new(recorder));
    }

    // Wrap in async dumper - this will run dumping in a separate thread
    let async_dumper = AsyncDumper::new(Box::new(multi_dumper));

    // Create and initialize proxy
    // Model or file actions sent here replace the controller's while connected
    let replay = Replay::listen(config.replay.port)?;

    let mut proxy = Proxy::new(
        Box::new(async_dumper),
        replay,
        usb_gadget.remote_wakeup(),
        &hid_device_path,
        config.proxy,
    )?;

    log::info!("Starting proxy with async dumping (threads inherit the real-time priority)");

    // Start proxy main loop (runs at high priority)
    let result = proxy.start();

    // Cleanup USB gadget before exit
    usb_gadget.cleanup();

    result?;
    Ok(())
}
