use crate::config::ProxyConfig;
use crate::device::ProController;
use crate::dump::{Dumper, Frame};
use anyhow::{Result, bail};
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::io::AsRawFd;
use std::time::Duration;

pub struct Proxy {
    controller: ProController,
    hidg_device: Option<File>,
    dumper: Box<dyn Dumper>,
    hidg_path: String,
    config: ProxyConfig,
}

impl Proxy {
    /// Initialize a new proxy with the given dumper and configuration
    pub fn new(dumper: Box<dyn Dumper>, hidg_path: &str, config: ProxyConfig) -> Result<Self> {
        log::info!("Initializing Proxy...");

        // Connect to Pro Controller
        let controller = ProController::connect()?;
        log::info!("Pro Controller connected");

        // Check if HID gadget device exists
        if !std::path::Path::new(hidg_path).exists() {
            bail!(
                "HID gadget device {} not found. Please configure HID gadget first.",
                hidg_path
            );
        }

        Ok(Proxy {
            controller,
            hidg_device: None,
            dumper,
            hidg_path: hidg_path.to_string(),
            config,
        })
    }

    /// Open the HID gadget device in non-blocking mode
    fn open_hidg_device(&mut self) -> Result<()> {
        let hidg_file = File::options()
            .read(true)
            .write(true)
            .open(&self.hidg_path)?;

        // Set non-blocking mode
        let fd = hidg_file.as_raw_fd();
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags != -1 {
                libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
        }

        self.hidg_device = Some(hidg_file);
        log::info!("HID gadget device opened in non-blocking mode");
        Ok(())
    }

    /// Start the proxy main loop
    pub fn start(&mut self) -> Result<()> {
        log::info!("Starting bidirectional proxy...");
        log::info!("Press Ctrl+C to stop");

        let mut input_buffer = [0u8; 64];
        let mut output_buffer = [0u8; 64];
        let mut frame_count = 0u64;
        // Numbers every report read, so consumers can spot dropped frames
        let mut seq = 0u32;

        loop {
            // Ensure HID gadget device is open
            if self.hidg_device.is_none() {
                match self.open_hidg_device() {
                    Ok(()) => {}
                    Err(e) => {
                        log::warn!("Failed to open HID gadget device: {} - retrying...", e);
                        std::thread::sleep(Duration::from_millis(self.config.hidg_retry_delay_ms));
                        continue;
                    }
                }
            }

            // Main proxy loop
            loop {
                // Direction 1: Controller -> NS (Input reports)
                match self
                    .controller
                    .read_timeout(&mut input_buffer, self.config.controller_read_timeout_ms)
                {
                    Ok(size) => {
                        if size > 0 {
                            // Timestamp once here so every dumper sees the same frame
                            let frame = Frame::new(seq, &input_buffer[..size]);
                            seq = seq.wrapping_add(1);
                            if let Err(e) = self.dumper.dump(&frame) {
                                log::warn!("Failed to dump input data: {}", e);
                            }

                            // Forward to HID gadget
                            if let Some(ref mut hidg_device) = self.hidg_device {
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
                                        if frame_count % self.config.frame_count_log_interval == 0 {
                                            log::debug!("Forwarded {} input frames", frame_count);
                                        }
                                    }
                                    Err(e) => {
                                        log::warn!(
                                            "Failed to write input to HID gadget: {} - reopening device...",
                                            e
                                        );
                                        self.hidg_device = None;
                                        break; // Break inner loop to reopen device
                                    }
                                }
                            }
                        }
                    }
                    Err(e) if e.to_string().contains("timeout") => {
                        // Timeout is expected for short reads
                    }
                    Err(e) => {
                        log::error!("Failed to read from Pro Controller: {}", e);
                        if let Err(reconnect_err) = self.controller.reconnect() {
                            log::error!("Failed to reconnect: {}", reconnect_err);
                            return Err(reconnect_err);
                        }
                        // Continue with reconnected device
                        continue;
                    }
                }

                // Direction 2: NS -> Controller (Output reports)
                if let Some(ref mut hidg_device) = self.hidg_device {
                    match hidg_device.read(&mut output_buffer) {
                        Ok(size) => {
                            if size > 0 {
                                // Forward output report to the controller
                                match self.controller.write(&output_buffer[..size]) {
                                    Ok(_) => {
                                        log::debug!("Forwarded output report to controller");
                                    }
                                    Err(e) => {
                                        log::warn!("Failed to write output to controller: {}", e);
                                        if let Err(reconnect_err) = self.controller.reconnect() {
                                            log::error!(
                                                "Failed to reconnect after output write failure: {}",
                                                reconnect_err
                                            );
                                            return Err(reconnect_err);
                                        }
                                        // Continue with reconnected device
                                        continue;
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
                            self.hidg_device = None;
                            break; // Break inner loop to reopen device
                        }
                    }
                }
            }
        }
    }
}
