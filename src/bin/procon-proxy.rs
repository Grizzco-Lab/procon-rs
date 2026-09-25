use anyhow::Context;
use clap::Parser;
use procon::config::Config;
use procon::dump::{AsyncDumper, MultiDumper};
use procon::gadget::ProConGadget;
use procon::priority::set_high_priority;
use procon::proxy::Proxy;
use procon::recorder::Recorder;
use procon::replay::Replay;
use procon::stream::FrameStreamer;

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

    // Setup USB gadget programmatically
    let mut usb_gadget = ProConGadget::new();
    let hid_device_path = usb_gadget
        .setup()
        .context("Failed to setup USB gadget - ensure you have root privileges")?;
    log::info!("USB gadget configured at: {}", hid_device_path);

    // Create multi-dumper for async processing
    let mut multi_dumper = MultiDumper::new();

    // Stream frames to the studio host, which records them with the video
    multi_dumper.add_dumper(Box::new(FrameStreamer::listen(config.stream.port)?));

    // Optional local backup: one session from launch until exit
    if config.dump.autostart {
        let recorder = Recorder::new(config.dump.prefix.as_str());
        recorder.start()?;
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

    log::info!("Starting proxy with async dumping (dump thread runs at normal priority)");

    // Start proxy main loop (runs at high priority)
    let result = proxy.start();

    // Cleanup USB gadget before exit
    usb_gadget.cleanup();

    result?;
    Ok(())
}
