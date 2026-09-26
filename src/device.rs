//! The physical Pro Controller, through hidapi
//!
//! The controller asks to be polled every 8 ms, and its firmware only sends
//! reports on that beat. Polling it faster (Linux's `usbhid.jspoll`) brings
//! input no sooner; it made rumble writes take ~2 ms instead of ~9, but on the
//! Pi 4 the changed interval broke the output endpoint ("TRB DMA ptr not part
//! of current TD"), so the Switch's handshake never reached the controller.
//!
//! A controller the console knows over Bluetooth (one ever plugged into the
//! console itself) connects to it wirelessly on its own when it sits on the Pi
//! without an active USB session. The console then talks to it over
//! Bluetooth and the proxy's USB handshake stalls, so the proxy [`reset`]s it
//! just before presenting itself to the Switch: that drops the wireless link.

use anyhow::{Context, Result, bail};
use core::time::Duration;
use hidapi::{HidApi, HidDevice};
use std::path::PathBuf;
use std::time::Instant;

const VENDOR_ID: u16 = 0x057e;
const PRODUCT_ID: u16 = 0x2009;

/// Wait between attempts to find the controller again
const RECONNECT_DELAY: Duration = Duration::from_millis(500);
/// USB devices in sysfs
const USB_DEVICES: &str = "/sys/bus/usb/devices";
/// Longest wait for the controller to come back after a reset
const RESET_TIMEOUT: Duration = Duration::from_secs(10);
/// Time Linux's own driver takes to set a controller up once it is back
const SETTLE_TIME: Duration = Duration::from_secs(2);

pub struct ProController(HidDevice);

impl ProController {
    pub fn connect() -> Result<Self> {
        // Debug only: reconnecting calls this twice a second
        log::debug!("Searching for Nintendo Switch Pro Controller...");
        let hid_api = HidApi::new()?;
        let Some(info) = hid_api
            .device_list()
            .find(|info| info.vendor_id() == VENDOR_ID && info.product_id() == PRODUCT_ID)
        else {
            bail!("Nintendo Switch Pro Controller not found")
        };
        let device = info
            .open_device(&hid_api)
            .with_context(|| format!("failed to open device {:?}", info))?;
        Ok(ProController(device))
    }

    /// Wait until the controller is back, however long that takes
    pub fn reconnect(&mut self) {
        log::info!("Waiting for the Pro Controller to come back...");
        loop {
            std::thread::sleep(RECONNECT_DELAY);
            if let Ok(controller) = Self::connect() {
                self.0 = controller.0;
                log::info!("Pro Controller reconnected");
                return;
            }
        }
    }

    /// Read data from the controller with timeout
    pub fn read_timeout(
        &mut self,
        buffer: &mut [u8],
        timeout_ms: i32,
    ) -> Result<usize, hidapi::HidError> {
        self.0.read_timeout(buffer, timeout_ms)
    }

    /// Write data to the controller
    pub fn write(&mut self, data: &[u8]) -> Result<usize, hidapi::HidError> {
        self.0.write(data)
    }
}

/// Disconnect the controller and connect it again, as a replug does, and wait
/// until it is ready; this drops a Bluetooth link to the console
pub fn reset() -> Result<()> {
    let authorized = controller_dir()
        .context("Nintendo Switch Pro Controller not found")?
        .join("authorized");
    std::fs::write(&authorized, "0").context("cannot disconnect the controller")?;
    std::thread::sleep(RECONNECT_DELAY);
    std::fs::write(&authorized, "1").context("cannot reconnect the controller")?;

    let start = Instant::now();
    while ProController::connect().is_err() {
        if start.elapsed() > RESET_TIMEOUT {
            bail!("the controller did not come back after a reset");
        }
        std::thread::sleep(RECONNECT_DELAY);
    }
    std::thread::sleep(SETTLE_TIME);
    Ok(())
}

/// The controller's USB device folder in sysfs, like `/sys/bus/usb/devices/1-1.2`
fn controller_dir() -> Option<PathBuf> {
    let id = |dir: &PathBuf, name: &str| {
        std::fs::read_to_string(dir.join(name))
            .ok()
            .and_then(|text| u16::from_str_radix(text.trim(), 16).ok())
    };
    std::fs::read_dir(USB_DEVICES)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .find(|dir| {
            id(dir, "idVendor") == Some(VENDOR_ID) && id(dir, "idProduct") == Some(PRODUCT_ID)
        })
}
