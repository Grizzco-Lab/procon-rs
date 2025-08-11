use anyhow::{Context, bail};
use hidapi::{DeviceInfo, HidApi, HidDevice};

pub trait ProConDevice {
    const VENDOR_ID : u16;
    const PRODUCT_ID : u16;
}

pub fn find_hid_device_by_manufracture(
    hid_api: &HidApi,
    vendor_id: u16,
    product_id: u16,
) -> Option<DeviceInfo> {
    hid_api.device_list().find(|di| {
        di.vendor_id() == vendor_id
            && di.product_id() == product_id
    }).cloned()
}

pub struct ProController(HidDevice);

impl ProConDevice for ProController {
    const VENDOR_ID : u16 = 0x057e;
    const PRODUCT_ID : u16 = 0x2009;
}

impl ProController {
    pub fn connect() -> anyhow::Result<Self> {
        let hid_api = HidApi::new()?;
        log::info!("Searching for Nintendo Switch Pro Controller...");

        let device_info = find_hid_device_by_manufracture(
            &hid_api,
            Self::VENDOR_ID,
            Self::PRODUCT_ID,
        );

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
        let mut frame_count = 0u64;

        loop {
            match self.0.read_timeout(&mut buffer, 1000) {
                Ok(size) => {
                    assert!(size > 0);
                    frame_count += 1;
                }
                Err(e) => {
                    log::error!("Failed to read from device: {}", e);
                    break;
                }
            }
        }

        Ok(())
    }
}
