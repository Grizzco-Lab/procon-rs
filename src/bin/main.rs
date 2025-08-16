use anyhow::Context;
use clap::Parser;
use procon::config::Config;
use procon::dump::{AsyncDumper, ConsoleDumper, FileDumper, MultiDumper};
use procon::gadget::ProConGadget;
use procon::priority::set_high_priority;
use procon::proxy::Proxy;
use procon::web_visualization::WebVisualizationDumper;

/// Nintendo Switch Pro Controller HID Proxy
#[derive(Parser)]
#[command(name = "proconproxy")]
#[command(about = "A HID proxy for Nintendo Switch Pro Controller")]
struct Args {
    /// Path to configuration file
    #[arg(short, long, default_value = "config.toml")]
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

    // Add file dumper
    let file_dumper = Box::new(FileDumper::new(&config.dump.file_path)?);
    multi_dumper.add_dumper(file_dumper);

    // Add console dumper only if enabled
    if config.console.enable {
        let console_dumper = Box::new(ConsoleDumper::new());
        multi_dumper.add_dumper(console_dumper);
    }

    // Add web visualization dumper only if enabled
    let (web_server, web_dumper) = if config.visualization.web_enable {
        let (web_dumper, server) = WebVisualizationDumper::new();
        multi_dumper.add_dumper(Box::new(web_dumper.clone()));
        (Some(server), Some(web_dumper))
    } else {
        (None, None)
    };

    // Wrap in async dumper - this will run dumping in a separate thread
    let async_dumper = AsyncDumper::new(Box::new(multi_dumper));

    // Create and initialize proxy
    let mut proxy = Proxy::new(Box::new(async_dumper), &hid_device_path, config.proxy)?;

    log::info!("Starting proxy with async dumping (dump thread runs at normal priority)");
    if config.console.enable {
        log::info!("Console output enabled");
    } else {
        log::info!("Console output disabled");
    }
    if config.visualization.web_enable {
        log::info!(
            "Web visualization server starting on port {}",
            config.visualization.web_port
        );
    }

    // Start web server if enabled
    if let Some(server) = web_server {
        let port = config.visualization.web_port;
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                if let Err(e) = server.start_server(port).await {
                    log::error!("Web visualization server error: {}", e);
                }
            });
        });
    }

    // Start device connection monitoring if web visualization is enabled
    if let Some(web_dumper_ref) = web_dumper {
        std::thread::spawn(move || {
            let mut last_connected = true;

            loop {
                std::thread::sleep(std::time::Duration::from_millis(2000)); // Check every 2 seconds

                let should_be_connected = web_dumper_ref.is_device_connected();

                if should_be_connected != last_connected {
                    log::info!(
                        "Device connection status changed: {}",
                        if should_be_connected {
                            "connected"
                        } else {
                            "disconnected"
                        }
                    );
                    web_dumper_ref.update_device_status(should_be_connected);
                    last_connected = should_be_connected;
                }
            }
        });
    }

    // Start proxy main loop (runs at high priority)
    let result = proxy.start();

    // Cleanup USB gadget before exit
    usb_gadget.cleanup();

    result?;
    Ok(())
}
