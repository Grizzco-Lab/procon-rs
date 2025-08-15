use procon::dump::{ConsoleDumper, FileDumper, MultiDumper};
use procon::proxy::Proxy;

const HID_DEVICE_PATH: &str = "/dev/hidg0";
const DUMP_FILE_PATH: &str = "/tmp/procon_hid_dump.bin";

fn main() -> anyhow::Result<()> {
    env_logger::builder().format_source_path(true).init();

    log::info!("ProCon Proxy starting...");

    // Create multi-dumper
    let mut multi_dumper = MultiDumper::new();

    // Add file dumper
    let file_dumper = Box::new(FileDumper::new(DUMP_FILE_PATH)?);
    multi_dumper.add_dumper(file_dumper);

    // Add console dumper if debug logging is enabled
    let console_dumper = Box::new(ConsoleDumper::new());
    multi_dumper.add_dumper(console_dumper);

    // Create and initialize proxy
    let mut proxy = Proxy::new(Box::new(multi_dumper), HID_DEVICE_PATH)?;

    // Start proxy main loop
    proxy.start()?;

    Ok(())
}
