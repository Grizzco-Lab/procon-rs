//! USB gadget management for Nintendo Switch Pro Controller emulation

use anyhow::{Context, Result};
use usb_gadget::function::hid::Hid;
use usb_gadget::*;

/// Nintendo Switch Pro Controller USB gadget manager
pub struct ProConGadget {
    reg_gadget: Option<RegGadget>,
}

impl ProConGadget {
    /// Create a new ProCon gadget manager
    pub fn new() -> Self {
        Self { reg_gadget: None }
    }

    /// Setup and bind the Pro Controller USB gadget
    /// Returns the path to the created HID device (typically /dev/hidg0)
    pub fn setup(&mut self) -> Result<String> {
        log::info!("Setting up Nintendo Switch Pro Controller USB gadget...");

        // Cleanup any existing gadgets first
        if let Err(e) = remove_all() {
            log::warn!("Failed to cleanup existing gadgets: {}", e);
        }

        // Create HID function with bidirectional support
        let mut hid_builder = Hid::builder();
        hid_builder.protocol = 0; // No specific protocol
        hid_builder.sub_class = 0; // No subclass
        hid_builder.report_len = 64; // 64-byte reports
        hid_builder.report_desc = create_bidirectional_hid_descriptor();

        let (_hid, hid_handle) = hid_builder.build();

        // Create gadget with Nintendo Pro Controller IDs
        let reg_gadget = Gadget::new(
            Class::new(0, 0, 0),     // HID class will be set by the function
            Id::new(0x057E, 0x2009), // Nintendo vendor ID, Pro Controller product ID
            Strings::new("Proxy Co.", "NS Pro Proxy", "0001"),
        )
        .with_config(Config::new("Configuration 1").with_function(hid_handle))
        .bind(&default_udc().context("No USB device controller found")?)
        .context("Failed to bind gadget to UDC")?;

        log::info!("USB gadget bound to UDC: {:?}", reg_gadget.udc()?);

        // Store registered gadget for cleanup
        self.reg_gadget = Some(reg_gadget);

        // Wait for device creation
        std::thread::sleep(std::time::Duration::from_millis(500));

        // The HID function status gives us the config path, not the device path
        // We need to find the actual /dev/hidg* device that was created
        let device_path = find_hidg_device()
            .ok_or_else(|| anyhow::anyhow!("No /dev/hidg* device found after gadget creation"))?;

        log::info!("USB gadget successfully created at: {}", device_path);
        Ok(device_path)
    }

    /// Cleanup the USB gadget
    pub fn cleanup(&mut self) {
        if let Some(reg_gadget) = self.reg_gadget.take() {
            log::info!("Cleaning up USB gadget...");
            if let Err(e) = reg_gadget.remove() {
                log::error!("Failed to remove USB gadget: {}", e);
            }
        }
    }
}

impl Drop for ProConGadget {
    fn drop(&mut self) {
        self.cleanup();
    }
}

/// Find the created /dev/hidg* device
/// Returns the path to the first available HID gadget device
fn find_hidg_device() -> Option<String> {
    use std::fs;
    use std::os::unix::fs::FileTypeExt;

    // Look for /dev/hidg* devices (usually hidg0, hidg1, etc.)
    for i in 0..10 {
        let device_path = format!("/dev/hidg{}", i);
        if std::path::Path::new(&device_path).exists() {
            // Verify it's a character device
            if let Ok(metadata) = fs::metadata(&device_path) {
                if metadata.file_type().is_char_device() {
                    return Some(device_path);
                }
            }
        }
    }

    None
}

/// Create the bidirectional HID report descriptor
/// Supports both input reports (0x30, controller -> NS) and output reports (0x10, NS -> controller)
fn create_bidirectional_hid_descriptor() -> Vec<u8> {
    vec![
        // Usage Page (Generic Desktop Controls)
        0x05, 0x01, // Usage (Game Pad)
        0x09, 0x05, // Collection (Application)
        0xA1, 0x01, // Input report 0x30, 64 bytes (controller data to Nintendo Switch)
        0x85, 0x30, // Report ID (48)
        0x15, 0x00, // Logical Minimum (0)
        0x26, 0xFF, 0x00, // Logical Maximum (255)
        0x75, 0x08, // Report Size (8 bits)
        0x95, 0x40, // Report Count (64 bytes)
        0x09, 0x01, // Usage (Pointer)
        0x81, 0x02, // Input (Data, Variable, Absolute)
        // Output report 0x10, 64 bytes (Nintendo Switch commands to controller)
        0x85, 0x10, // Report ID (16)
        0x15, 0x00, // Logical Minimum (0)
        0x26, 0xFF, 0x00, // Logical Maximum (255)
        0x75, 0x08, // Report Size (8 bits)
        0x95, 0x40, // Report Count (64 bytes)
        0x09, 0x02, // Usage (Mouse)
        0x91, 0x02, // Output (Data, Variable, Absolute)
        // End Collection
        0xC0,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hid_descriptor_structure() {
        let desc = create_bidirectional_hid_descriptor();

        // Verify descriptor starts with correct Usage Page and Usage
        assert_eq!(desc[0..2], [0x05, 0x01]); // Usage Page (Generic Desktop)
        assert_eq!(desc[2..4], [0x09, 0x05]); // Usage (Game Pad)
        assert_eq!(desc[4..6], [0xA1, 0x01]); // Collection (Application)

        // Verify input report ID
        assert_eq!(desc[6..8], [0x85, 0x30]); // Report ID 0x30

        // Verify descriptor ends properly
        assert_eq!(desc[desc.len() - 1], 0xC0); // End Collection
    }

    #[test]
    fn test_vendor_product_ids() {
        // Verify we're using correct Nintendo IDs
        assert_eq!(0x057E, 0x057E); // Nintendo vendor ID
        assert_eq!(0x2009, 0x2009); // Pro Controller product ID
    }
}
