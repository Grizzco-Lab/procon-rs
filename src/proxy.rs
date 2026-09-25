//! Forwarding between the Pro Controller and the Switch
//!
//! Input reports (controller → Switch: buttons, sticks, IMU) and output reports
//! (Switch → controller: rumble, LEDs, subcommands) run on separate threads. A
//! write to the controller blocks for about 9 ms, and the Switch sends rumble
//! all through a game, so in one loop every input report could wait for it.
//!
//! Each forwarded report's frame carries how long it spent in the proxy: from
//! reading it to the Switch taking it from the gadget ([`Frame::forward_us`]).

use crate::config::ProxyConfig;
use crate::device::ProController;
use crate::dump::{Dumper, Frame};
use crate::replay::Replay;
use crate::wake::RemoteWakeup;
use anyhow::{Result, bail};
use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::time::{Duration, Instant};

/// Home button in byte 4 of an input report
const HOME: (usize, u8) = (4, 0x10);
/// Least time between wakeup attempts while Home is held
const WAKE_RETRY: Duration = Duration::from_secs(1);
/// Longest wait for the Switch to take a report; it polls every millisecond
const PICKUP_TIMEOUT_MS: i32 = 20;
/// Longest wait for an input report; the controller sends one every 8–16 ms
const READ_TIMEOUT_MS: i32 = 1000;

pub struct Proxy {
    controller: ProController,
    dumper: Box<dyn Dumper>,
    /// Actions from a replay client, applied to input reports while one is connected
    replay: Replay,
    /// Wakes a sleeping Switch when Home is pressed
    wakeup: Option<RemoteWakeup>,
    hidg_path: String,
    config: ProxyConfig,
}

impl Proxy {
    /// Initialize a new proxy with the given dumper and configuration
    pub fn new(
        dumper: Box<dyn Dumper>,
        replay: Replay,
        wakeup: Option<RemoteWakeup>,
        hidg_path: &str,
        config: ProxyConfig,
    ) -> Result<Self> {
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
            dumper,
            replay,
            wakeup,
            hidg_path: hidg_path.to_string(),
            config,
        })
    }

    /// Forward output reports on their own thread, and input reports on this one
    pub fn start(&mut self) -> Result<()> {
        log::info!("Starting bidirectional proxy...");
        log::info!("Press Ctrl+C to stop");

        let hidg_path = self.hidg_path.clone();
        let retry = Duration::from_millis(self.config.hidg_retry_delay_ms);
        // Inherits this thread's real-time priority
        std::thread::spawn(move || forward_output(&hidg_path, retry));

        let mut input_buffer = [0u8; 64];
        let mut hidg: Option<File> = None;
        // Numbers every report read, so consumers can spot dropped frames
        let mut seq = 0u32;
        // The Switch stopped taking reports (asleep), and when we last tried waking it
        let mut host_idle = false;
        let mut last_wake: Option<Instant> = None;

        loop {
            let Some(gadget) = hidg.as_mut() else {
                match open_hidg(&self.hidg_path, true) {
                    Ok(file) => {
                        log::info!("HID gadget device opened for input");
                        hidg = Some(file);
                    }
                    Err(e) => {
                        log::warn!("Failed to open HID gadget device: {} - retrying...", e);
                        std::thread::sleep(retry);
                    }
                }
                continue;
            };

            let size = match self
                .controller
                .read_timeout(&mut input_buffer, READ_TIMEOUT_MS)
            {
                Ok(0) => continue,
                Ok(size) => size,
                Err(e) => {
                    log::error!("Failed to read from Pro Controller: {}", e);
                    self.controller.reconnect();
                    continue;
                }
            };
            let read_at = Instant::now();

            // Recordings see what the Switch sees
            self.replay.apply(&mut input_buffer[..size]);
            // Timestamp once here so every dumper sees the same frame
            let mut frame = Frame::new(seq, &input_buffer[..size]);
            seq = seq.wrapping_add(1);

            match gadget.write(&input_buffer[..size]) {
                Ok(_) => {
                    if host_idle {
                        log::info!("Switch is taking input again");
                        host_idle = false;
                    }
                    if let Some(waited) = wait_for_pickup(gadget, read_at) {
                        frame.forward_us = waited.as_micros().clamp(1, u16::MAX as u128) as u16;
                    }
                }
                // The Switch is not reading (asleep): drop the report and keep
                // the device, which reopening would not change
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    if !host_idle {
                        log::info!("Switch stopped taking input (asleep?); press Home to wake it");
                        host_idle = true;
                    }
                    let home = input_buffer[HOME.0] & HOME.1 != 0;
                    if let Some(wakeup) = &self.wakeup
                        && home
                        && last_wake.is_none_or(|at| at.elapsed() >= WAKE_RETRY)
                    {
                        last_wake = Some(Instant::now());
                        if wakeup.wake() {
                            log::info!("Home pressed: signalled USB resume");
                        } else {
                            log::info!("Home pressed, but the bus is not suspended; cannot wake");
                        }
                    }
                }
                Err(e) => {
                    log::warn!(
                        "Failed to write input to HID gadget: {} - reopening device...",
                        e
                    );
                    hidg = None;
                }
            }

            if let Err(e) = self.dumper.dump(&frame) {
                log::warn!("Failed to dump input data: {}", e);
            }
        }
    }
}

/// Open the HID gadget device, optionally non-blocking
fn open_hidg(path: &str, nonblocking: bool) -> std::io::Result<File> {
    let flags = if nonblocking { libc::O_NONBLOCK } else { 0 };
    File::options()
        .read(true)
        .write(true)
        .custom_flags(flags)
        .open(path)
}

/// Wait until the Switch has taken the report just written, and return the
/// time since it was read from the controller; `None` if it does not come
fn wait_for_pickup(gadget: &File, read_at: Instant) -> Option<Duration> {
    let mut poll = libc::pollfd {
        fd: gadget.as_raw_fd(),
        events: libc::POLLOUT,
        revents: 0,
    };
    // SAFETY: poll reads and writes only the one pollfd we pass
    let ready = unsafe { libc::poll(&mut poll, 1, PICKUP_TIMEOUT_MS) };
    (ready == 1 && poll.revents & libc::POLLOUT != 0).then(|| read_at.elapsed())
}

/// Forward output reports from the Switch to the controller, with a gadget
/// handle and a controller handle of its own; never returns
fn forward_output(hidg_path: &str, retry: Duration) {
    let mut buffer = [0u8; 64];
    loop {
        let gadget = open_hidg(hidg_path, false);
        let controller = ProController::connect();
        let (Ok(mut gadget), Ok(mut controller)) = (gadget, controller) else {
            log::warn!("Output forwarding cannot open its devices - retrying...");
            std::thread::sleep(retry);
            continue;
        };
        log::info!("Forwarding output reports to the controller");
        loop {
            // Blocks until the Switch sends something
            let size = match gadget.read(&mut buffer) {
                Ok(0) => continue,
                Ok(size) => size,
                Err(e) => {
                    log::warn!("Failed to read output from HID gadget: {}", e);
                    break;
                }
            };
            if let Err(e) = controller.write(&buffer[..size]) {
                log::warn!("Failed to write output to controller: {}", e);
                break;
            }
        }
        std::thread::sleep(retry);
    }
}
