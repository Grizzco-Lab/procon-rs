//! The physical Pro Controller, through hidapi
//!
//! The controller asks to be polled every 8 ms, and its firmware only sends
//! reports on that beat. Polling it faster (Linux's `usbhid.jspoll`) brings
//! input no sooner; it made rumble writes take ~2 ms instead of ~9, but on the
//! Pi 4 the changed interval broke the output endpoint ("TRB DMA ptr not part
//! of current TD"), so the Switch's handshake never reached the controller.

use anyhow::{Context, Result, bail};
use core::time::Duration;
use hidapi::{HidApi, HidDevice};

const VENDOR_ID: u16 = 0x057e;
const PRODUCT_ID: u16 = 0x2009;

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
