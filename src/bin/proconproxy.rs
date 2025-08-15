use procon::device::ProController;

const HID_DEVICE_PATH: &str = "/dev/hidg0";

fn main() -> anyhow::Result<()> {
    env_logger::builder().format_source_path(true).init();

    log::info!("ProCon Proxy starting...");

    // Connect to Pro Controller
    let mut procon = ProController::connect()?;
    log::info!("Pro Controller connected");

    // Check if HID gadget device exists
    if !std::path::Path::new(HID_DEVICE_PATH).exists() {
        log::error!(
            "HID gadget device {} not found. Please configure HID gadget first.",
            HID_DEVICE_PATH
        );
        return Err(anyhow::anyhow!("HID gadget device not found"));
    }

    // Start proxy loop with device path
    procon.start_proxy(HID_DEVICE_PATH)?;

    Ok(())
}
