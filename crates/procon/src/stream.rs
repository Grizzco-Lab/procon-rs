//! Frame link between the USB proxy and the studio host
//!
//! The proxy listens on TCP. For each connection it sends [`HEADER`] and then raw
//! [`FRAME_SIZE`]-byte [`Frame`]s, the same bytes a dump file holds. When no
//! report arrives for [`HEARTBEAT`] it sends an empty frame (`packet_size == 0`),
//! so the host can tell an idle controller from a dead link.

use crate::dump::{Dumper, FRAME_SIZE, Frame, stamped, unix_ms};
use alloc::sync::Arc;
use anyhow::{Context, Result, ensure};
use core::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use core::time::Duration;
use std::io::{BufReader, Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::thread;
use std::time::Instant;

/// Sent once per connection: magic and protocol version
pub const HEADER: [u8; 8] = *b"PROCON\0\x01";

/// Longest silence before the proxy sends an empty frame
pub const HEARTBEAT: Duration = Duration::from_secs(1);

/// Frames buffered per connection before a slow host starts losing them
const CLIENT_QUEUE: usize = 512;

/// Window for the clock offset estimate
const OFFSET_WINDOW: Duration = Duration::from_secs(10);

/// Proxy side: dumper that streams every frame to connected studio hosts
pub struct FrameStreamer {
    clients: Arc<Mutex<Vec<SyncSender<Frame>>>>,
}

impl FrameStreamer {
    /// Accept studio connections on `0.0.0.0:port`
    pub fn listen(port: u16) -> Result<Self> {
        let listener = TcpListener::bind(("0.0.0.0", port))
            .with_context(|| format!("cannot listen on port {port}"))?;
        log::info!("Streaming frames on port {}", port);

        let clients: Arc<Mutex<Vec<SyncSender<Frame>>>> = Arc::default();
        let accepted = Arc::clone(&clients);
        thread::spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        let (tx, rx) = sync_channel(CLIENT_QUEUE);
                        accepted.lock().unwrap().push(tx);
                        thread::spawn(move || send_frames(stream, rx));
                    }
                    Err(e) => log::warn!("Failed to accept studio connection: {}", e),
                }
            }
        });

        Ok(Self { clients })
    }
}

impl Dumper for FrameStreamer {
    fn dump(&mut self, frame: &Frame) -> Result<()> {
        // A full queue drops the frame for that host only; its sequence gap shows it
        self.clients.lock().unwrap().retain(|client| {
            !matches!(client.try_send(*frame), Err(TrySendError::Disconnected(_)))
        });
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Write frames to one host until it disconnects
fn send_frames(mut stream: TcpStream, frames: Receiver<Frame>) {
    let peer = stream
        .peer_addr()
        .map_or_else(|_| "unknown".to_string(), |a| a.to_string());
    log::info!("Studio connected from {}", peer);
    // Frames are tiny and latency matters more than packet count
    let _ = stream.set_nodelay(true);

    let result = (|| -> std::io::Result<()> {
        stream.write_all(&HEADER)?;
        let mut last_seq = 0;
        loop {
            let frame = match frames.recv_timeout(HEARTBEAT) {
                Ok(frame) => {
                    last_seq = frame.seq;
                    frame
                }
                Err(RecvTimeoutError::Timeout) => stamped(last_seq, &[]),
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            };
            stream.write_all(&frame.to_bytes())?;
        }
    })();
    log::info!("Studio {} disconnected: {:?}", peer, result);
}

/// Host side: health of the link to the proxy
#[derive(Default)]
pub struct LinkStats {
    /// A proxy stream is currently open
    pub connected: AtomicBool,
    /// Reports received, excluding heartbeats
    pub frames: AtomicU64,
    /// Reports missing from the sequence
    pub dropped: AtomicU64,
    /// Host clock minus proxy clock in ms, as the lowest difference seen over the
    /// last window; it includes the one-way network delay (well under 1 ms on a LAN)
    pub clock_offset_ms: AtomicI64,
    /// Sum, count and maximum of [`Frame::forward_us`] since the dashboard last took them
    pub forward_sum_us: AtomicU64,
    pub forward_count: AtomicU64,
    pub forward_max_us: AtomicU64,
}

impl LinkStats {
    /// Mean and maximum time reports spent in the proxy since the last call, in µs
    pub fn take_forward_us(&self) -> Option<(f64, u64)> {
        let count = self.forward_count.swap(0, Ordering::Relaxed);
        let sum = self.forward_sum_us.swap(0, Ordering::Relaxed);
        let max = self.forward_max_us.swap(0, Ordering::Relaxed);
        (count > 0).then(|| (sum as f64 / count as f64, max))
    }
}

/// Host side: keep a connection to the proxy at `address` and feed its reports into `dumper`
pub fn receive_frames(address: &str, dumper: &mut dyn Dumper, stats: &LinkStats) -> ! {
    loop {
        if let Err(e) = receive_once(address, dumper, stats) {
            log::warn!("Proxy link {}: {:#}", address, e);
        }
        stats.connected.store(false, Ordering::Relaxed);
        thread::sleep(Duration::from_secs(2));
    }
}

fn receive_once(address: &str, dumper: &mut dyn Dumper, stats: &LinkStats) -> Result<()> {
    let socket = address
        .to_socket_addrs()?
        .next()
        .context("address did not resolve")?;
    let stream = TcpStream::connect_timeout(&socket, Duration::from_secs(3))?;
    // Missing a few heartbeats means the proxy is gone
    stream.set_read_timeout(Some(HEARTBEAT * 3))?;
    let mut stream = BufReader::new(stream);

    let mut header = [0u8; HEADER.len()];
    stream.read_exact(&mut header)?;
    ensure!(header == HEADER, "not a procon frame stream");
    log::info!("Connected to proxy at {}", address);
    stats.connected.store(true, Ordering::Relaxed);

    let mut next_seq: Option<u32> = None;
    // Running minimum until the first window completes, then once per window
    let mut first_window = true;
    let mut window_start = Instant::now();
    let mut window_min = i64::MAX;
    let mut bytes = [0u8; FRAME_SIZE];
    loop {
        stream.read_exact(&mut bytes)?;
        let frame = Frame::parse(&bytes);

        window_min = window_min.min(unix_ms() as i64 - frame.timestamp_ms as i64);
        let window_done = window_start.elapsed() >= OFFSET_WINDOW;
        if first_window || window_done {
            stats.clock_offset_ms.store(window_min, Ordering::Relaxed);
        }
        if window_done {
            first_window = false;
            window_start = Instant::now();
            window_min = i64::MAX;
        }

        if frame.packet_size == 0 {
            continue;
        }
        let seq = frame.seq;
        if let Some(expected) = next_seq {
            let missing = seq.wrapping_sub(expected);
            // A huge jump means the proxy restarted, not that frames were lost
            if missing > 0 && missing < 1_000_000 {
                stats.dropped.fetch_add(missing as u64, Ordering::Relaxed);
            }
        }
        next_seq = Some(seq.wrapping_add(1));
        stats.frames.fetch_add(1, Ordering::Relaxed);
        let forward_us = frame.forward_us as u64;
        if forward_us > 0 {
            stats
                .forward_sum_us
                .fetch_add(forward_us, Ordering::Relaxed);
            stats.forward_count.fetch_add(1, Ordering::Relaxed);
            stats
                .forward_max_us
                .fetch_max(forward_us, Ordering::Relaxed);
        }
        dumper.dump(&frame)?;
    }
}
