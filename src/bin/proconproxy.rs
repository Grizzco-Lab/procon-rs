use procon::dump::{AsyncDumper, ConsoleDumper, FileDumper, MultiDumper};
use procon::priority::set_high_priority;
use procon::proxy::Proxy;

const HID_DEVICE_PATH: &str = "/dev/hidg0";
const DUMP_FILE_PATH: &str = "/tmp/procon_hid_dump.bin";

fn main() -> anyhow::Result<()> {
    env_logger::builder().format_source_path(true).init();

    log::info!("ProCon Proxy starting...");

    // Set high priority for main proxy thread
    // CPU affinity can be enabled via PROCON_CPU_AFFINITY=1 environment variable
    let enable_cpu_affinity = std::env::var("PROCON_CPU_AFFINITY")
        .map(|v| v == "1" || v.to_lowercase() == "true")
        .unwrap_or(false);

    if enable_cpu_affinity {
        log::info!("CPU affinity enabled via PROCON_CPU_AFFINITY environment variable");
    } else {
        log::debug!("CPU affinity disabled - set PROCON_CPU_AFFINITY=1 to enable");
    }

    set_high_priority(enable_cpu_affinity);

    // Create multi-dumper for async processing
    let mut multi_dumper = MultiDumper::new();

    // Add file dumper
    let file_dumper = Box::new(FileDumper::new(DUMP_FILE_PATH)?);
    multi_dumper.add_dumper(file_dumper);

    // Add console dumper
    let console_dumper = Box::new(ConsoleDumper::new());
    multi_dumper.add_dumper(console_dumper);

    // Wrap in async dumper - this will run dumping in a separate thread
    let async_dumper = AsyncDumper::new(Box::new(multi_dumper));

    // Create and initialize proxy
    let mut proxy = Proxy::new(Box::new(async_dumper), HID_DEVICE_PATH)?;

    log::info!("Starting proxy with async dumping (dump thread runs at normal priority)");

    // Start proxy main loop (runs at high priority)
    proxy.start()?;

    Ok(())
}
