use procon::dumper::NullDumper;
use procon::proxy::Proxy;

const HID_DEVICE_PATH: &str = "/dev/hidg0";

fn main() -> anyhow::Result<()> {
    env_logger::builder().format_source_path(true).init();

    log::info!("ProCon Proxy starting...");

    // Create dumper (using NullDumper for now)
    let dumper = Box::new(NullDumper::new());

    // Create and initialize proxy
    let mut proxy = Proxy::new(dumper, HID_DEVICE_PATH)?;

    // Start proxy main loop
    proxy.start()?;

    Ok(())
}
