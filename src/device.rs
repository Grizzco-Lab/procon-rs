//! The physical Pro Controller, through hidapi
//!
//! The controller asks to be polled every 8 ms, and its firmware only sends
//! reports on that beat. Polling faster ([`set_poll_interval`], Linux's
//! `usbhid.jspoll`) does not bring input sooner, but writes to the controller
//! (rumble) finish in about 2 ms instead of 9.

use anyhow::{Context, Result, bail};
use core::time::Duration;
use hidapi::{HidApi, HidDevice};
use std::fs;
use std::path::Path;

const VENDOR_ID: u16 = 0x057e;
const PRODUCT_ID: u16 = 0x2009;

/// Polling interval for joysticks, applied when a device binds
const JSPOLL: &str = "/sys/module/usbhid/parameters/jspoll";
/// Lets the generic HID driver take devices a specific driver would claim
const IGNORE_SPECIAL: &str = "/sys/module/hid/parameters/ignore_special_drivers";
const USB_DEVICES: &str = "/sys/bus/usb/devices";
const USBHID: &str = "/sys/bus/usb/drivers/usbhid";
/// Wait between attempts to find the controller again
const RECONNECT_DELAY: Duration = Duration::from_millis(500);

pub struct ProController(HidDevice);

impl ProController {
    pub fn connect() -> Result<Self> {
        log::info!("Searching for Nintendo Switch Pro Controller...");
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

/// Poll joysticks every `interval_ms` (0 for what they ask), and if that
/// changes the setting, rebind a plugged-in controller so it takes effect
///
/// The rebind goes to the generic HID driver: Linux's Nintendo driver would
/// try to set up a controller the Switch has already set up, and fail. A
/// controller plugged in later gets the Nintendo driver as usual, which sets
/// it up, and the new interval.
pub fn set_poll_interval(interval_ms: u32) -> Result<()> {
    let current = fs::read_to_string(JSPOLL).with_context(|| format!("cannot read {JSPOLL}"))?;
    if current.trim() == interval_ms.to_string() {
        return Ok(());
    }
    fs::write(JSPOLL, interval_ms.to_string()).with_context(|| format!("cannot write {JSPOLL}"))?;
    log::info!("Polling joysticks every {interval_ms} ms (0: as they ask)");

    let Some(interface) = controller_interface() else {
        return Ok(());
    };
    let previous = fs::read_to_string(IGNORE_SPECIAL)?;
    fs::write(IGNORE_SPECIAL, "1")?;
    let rebind = fs::write(format!("{USBHID}/unbind"), &interface)
        .and_then(|()| fs::write(format!("{USBHID}/bind"), &interface));
    fs::write(IGNORE_SPECIAL, previous.trim())?;
    rebind.with_context(|| format!("cannot rebind {interface}"))?;
    log::info!("Rebound the controller ({interface}) to apply it");
    Ok(())
}

/// USB interface name of a plugged-in controller, like `1-1.2:1.0`
fn controller_interface() -> Option<String> {
    let id = |dir: &Path, name: &str| {
        fs::read_to_string(dir.join(name))
            .ok()
            .and_then(|text| u16::from_str_radix(text.trim(), 16).ok())
    };
    fs::read_dir(USB_DEVICES).ok()?.flatten().find_map(|entry| {
        let dir = entry.path();
        (id(&dir, "idVendor") == Some(VENDOR_ID) && id(&dir, "idProduct") == Some(PRODUCT_ID))
            .then(|| format!("{}:1.0", entry.file_name().to_string_lossy()))
    })
}
