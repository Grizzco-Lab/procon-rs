use anyhow::{Context, bail};
use hidapi::{DeviceInfo, HidApi, HidDevice};

pub trait ProConDevice {
    const VENDOR_ID: u16;
    const PRODUCT_ID: u16;
}

pub fn find_hid_device_by_manufracture(
    hid_api: &HidApi,
    vendor_id: u16,
    product_id: u16,
) -> Option<DeviceInfo> {
    hid_api
        .device_list()
        .find(|di| di.vendor_id() == vendor_id && di.product_id() == product_id)
        .cloned()
}

pub struct ProController(HidDevice);

impl ProConDevice for ProController {
    const VENDOR_ID: u16 = 0x057e;
    const PRODUCT_ID: u16 = 0x2009;
}

impl ProController {
    pub fn connect() -> anyhow::Result<Self> {
        let hid_api = HidApi::new()?;
        log::info!("Searching for Nintendo Switch Pro Controller...");

        let device_info =
            find_hid_device_by_manufracture(&hid_api, Self::VENDOR_ID, Self::PRODUCT_ID);

        if let Some(di) = device_info {
            let hid_device = di
                .open_device(&hid_api)
                .with_context(|| format!("failed to open device {:?}", di))?;
            Ok(ProController(hid_device))
        } else {
            bail!("Nintendo Switch Pro Controller not found")
        }
    }

    pub fn start_capture(&mut self) -> anyhow::Result<()> {
        log::info!("Starting Pro Controller data capture...");
        log::info!("Press Ctrl+C to stop");

        let mut buffer = [0u8; 64];
        let mut _frame_count = 0u64;

        loop {
            match self.0.read_timeout(&mut buffer, 1000) {
                Ok(size) => {
                    assert!(size > 0);
                    _frame_count += 1;
                }
                Err(e) => {
                    log::error!("Failed to read from device: {}", e);
                    break;
                }
            }
        }

        Ok(())
    }

    pub fn start_proxy(&mut self, hidg_path: &str) -> anyhow::Result<()> {
        log::info!("Starting non-blocking bidirectional proxy...");
        log::info!("Press Ctrl+C to stop");

        use std::fs::File;
        use std::io::{Read, Write};
        use std::os::unix::io::AsRawFd;
        use std::time::Duration;

        let mut input_buffer = [0u8; 64];
        let mut output_buffer = [0u8; 64];
        let mut frame_count = 0u64;

        loop {
            // Try to open HID gadget device
            let hidg_file = match File::options().read(true).write(true).open(hidg_path) {
                Ok(f) => f,
                Err(e) => {
                    log::warn!("Failed to open HID gadget device: {} - retrying...", e);
                    std::thread::sleep(Duration::from_millis(1000));
                    continue;
                }
            };

            // Set non-blocking mode for the HID gadget device
            let fd = hidg_file.as_raw_fd();
            unsafe {
                let flags = libc::fcntl(fd, libc::F_GETFL);
                if flags != -1 {
                    libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
                }
            }

            let mut hidg_device = hidg_file;
            log::info!("HID gadget device opened in non-blocking mode");

            // Main proxy loop
            loop {
                // Direction 1: Controller -> NS (Input reports)
                match self.0.read_timeout(&mut input_buffer, 10) {
                    Ok(size) => {
                        if size > 0 {
                            // Forward the raw HID report to the gadget device
                            match hidg_device.write(&input_buffer[..size]) {
                                Ok(written) => {
                                    if written != size {
                                        log::warn!(
                                            "Partial input write: {} of {} bytes",
                                            written,
                                            size
                                        );
                                    }
                                    frame_count += 1;
                                    if frame_count % 100 == 0 {
                                        log::debug!("Forwarded {} input frames", frame_count);
                                    }
                                }
                                Err(e) => {
                                    log::warn!(
                                        "Failed to write input to HID gadget: {} - reopening device...",
                                        e
                                    );
                                    break; // Break inner loop to reopen device
                                }
                            }
                        }
                    }
                    Err(e) if e.to_string().contains("timeout") => {
                        // Timeout is expected for short reads
                    }
                    Err(e) => {
                        log::error!("Failed to read from Pro Controller: {}", e);
                        return Err(e.into());
                    }
                }

                // Direction 2: NS -> Controller (Output reports) - Non-blocking
                match hidg_device.read(&mut output_buffer) {
                    Ok(size) => {
                        if size > 0 {
                            // Forward output report to the controller
                            match self.0.write(&output_buffer[..size]) {
                                Ok(_) => {
                                    log::debug!("Forwarded output report to controller");
                                }
                                Err(e) => {
                                    log::warn!("Failed to write output to controller: {}", e);
                                }
                            }
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        // No output data available, this is expected for non-blocking read
                    }
                    Err(e) => {
                        log::warn!(
                            "Failed to read output from HID gadget: {} - reopening device...",
                            e
                        );
                        break; // Break inner loop to reopen device
                    }
                }

                // Small sleep to prevent busy waiting
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
}
