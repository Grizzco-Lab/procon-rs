//! USB remote wakeup for the Pi 4's DWC2 device controller
//!
//! A sleeping Switch suspends the USB bus. A real controller wakes it by
//! signalling "resume" on the bus, which the host allows when the
//! configuration advertises remote wakeup (see [`crate::gadget`]). Linux's
//! dwc2 driver has no wakeup operation (writing the UDC's `srp` file does
//! nothing), so this drives the controller's remote wakeup bit directly
//! through `/dev/mem`, as the driver's own `dwc2_gadget_exit_clock_gating`
//! does: when the bus is suspended the driver stops the controller's clock
//! (`PCGCTL.StopPclk`), which also freezes the status registers, so restart
//! the clock, set `DCTL.RmtWkUpSig`, hold it 1–15 ms and clear it. If the host
//! does not resume, the clock is stopped again as the driver left it.
//!
//! The Switch 2 ignores this (and a Pro Controller plugged into it directly
//! cannot wake it either); the original Switch may not.

use anyhow::{Context, Result, bail};
use core::ptr::NonNull;
use core::time::Duration;
use std::fs::OpenOptions;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;

/// Synopsys ID register; the top half reads "OT" on DWC2 OTG cores
const GSNPSID: usize = 0x040;
/// Device control register; bit 0 signals remote wakeup
const DCTL: usize = 0x804;
/// Device status register; bit 0 is set while the bus is suspended, bits 8–21
/// count the host's frames
const DSTS: usize = 0x808;
/// Power and clock gating control; bit 0 stops the PHY clock, bit 1 gates HCLK
const PCGCTL: usize = 0xE00;
const PCGCTL_STOPPCLK: u32 = 1 << 0;
const PCGCTL_GATEHCLK: u32 = 1 << 1;
const DCTL_RMTWKUPSIG: u32 = 1 << 0;
const DSTS_SUSPSTS: u32 = 1 << 0;
/// How long to signal resume; USB allows 1–15 ms
const SIGNAL_TIME: Duration = Duration::from_millis(10);
/// How long the host gets to resume the bus before the clock is stopped again
const RESUME_WAIT: Duration = Duration::from_millis(50);
const MAP_SIZE: usize = 4096;

/// The DWC2 registers, mapped from `/dev/mem`
pub struct RemoteWakeup {
    regs: NonNull<u32>,
}

// SAFETY: the mapping is plain device memory, used only through volatile access
unsafe impl Send for RemoteWakeup {}

impl RemoteWakeup {
    /// Map the registers of the UDC named like `fe980000.usb`, whose name is
    /// its physical address; fails unless it is a DWC2 core
    pub fn open(udc_name: &str) -> Result<Self> {
        let base = udc_name
            .split('.')
            .next()
            .and_then(|hex| usize::from_str_radix(hex, 16).ok())
            .with_context(|| format!("no register address in UDC name {udc_name:?}"))?;
        let mem = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_SYNC)
            .open("/dev/mem")
            .context("cannot open /dev/mem")?;
        // SAFETY: maps one page of device registers; checked for failure below
        let map = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                MAP_SIZE,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                mem.as_raw_fd(),
                base as libc::off_t,
            )
        };
        if map == libc::MAP_FAILED {
            bail!(
                "cannot map registers at {base:#x}: {}",
                std::io::Error::last_os_error()
            );
        }
        let wakeup = RemoteWakeup {
            regs: NonNull::new(map.cast()).context("mmap returned null")?,
        };
        let id = wakeup.read(GSNPSID);
        if id >> 16 != 0x4f54 {
            bail!("{udc_name} is not a DWC2 controller (id {id:#010x})");
        }
        Ok(wakeup)
    }

    /// The host has suspended the bus, as a sleeping Switch does; with the
    /// clock stopped the status bit is stale, so the stopped clock counts too
    pub fn bus_suspended(&self) -> bool {
        self.read(DSTS) & DSTS_SUSPSTS != 0 || self.read(PCGCTL) & PCGCTL_STOPPCLK != 0
    }

    /// Signal resume if the bus is suspended; returns whether the host came back
    pub fn wake(&self) -> Option<bool> {
        if !self.bus_suspended() {
            return None;
        }
        let gating = self.read(PCGCTL);
        // Start the clock again: ungate HCLK, then the PHY clock
        self.write(PCGCTL, gating & !PCGCTL_GATEHCLK);
        self.write(PCGCTL, gating & !(PCGCTL_GATEHCLK | PCGCTL_STOPPCLK));
        let frame = self.read(DSTS) >> 8 & 0x3FFF;
        self.write(DCTL, self.read(DCTL) | DCTL_RMTWKUPSIG);
        std::thread::sleep(SIGNAL_TIME);
        self.write(DCTL, self.read(DCTL) & !DCTL_RMTWKUPSIG);
        std::thread::sleep(RESUME_WAIT);
        // Frames counting again means the host resumed the bus
        let resumed = self.read(DSTS) >> 8 & 0x3FFF != frame;
        if !resumed {
            // Leave the controller as the driver expects while suspended
            self.write(PCGCTL, gating);
        }
        Some(resumed)
    }

    fn read(&self, offset: usize) -> u32 {
        // SAFETY: offset is one of the register constants, inside the mapped page
        unsafe { self.regs.add(offset / 4).read_volatile() }
    }

    fn write(&self, offset: usize, value: u32) {
        // SAFETY: as in read
        unsafe { self.regs.add(offset / 4).write_volatile(value) }
    }
}

impl Drop for RemoteWakeup {
    fn drop(&mut self) {
        // SAFETY: unmaps the page mapped in open
        unsafe { libc::munmap(self.regs.as_ptr().cast(), MAP_SIZE) };
    }
}
