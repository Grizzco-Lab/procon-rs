use anyhow::Result;
use smallvec::SmallVec;
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::mem;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::keystate::{ButtonState, StickData};
use crate::parser::ProConParser;

const FLUSH_INTERVAL: u64 = 1000;

/// Maximum size of the dump channel buffer
const DUMP_CHANNEL_CAPACITY: usize = 1000;

/// Warning threshold for queue size (percentage of capacity)
const QUEUE_WARNING_THRESHOLD: f64 = 0.8;

/// Interval for checking and logging queue size warnings
const QUEUE_CHECK_INTERVAL: u64 = 100;

/// One controller report as stored in dump files and sent to the studio host
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct Frame {
    /// Millisecond Unix timestamp, taken when the report was read
    pub timestamp_ms: u64,
    /// Actual packet size; zero marks a heartbeat without data
    pub packet_size: u8,
    /// Capture sequence number, so gaps reveal dropped frames (zero in older files)
    pub seq: u32,
    /// Padding to 16-byte alignment
    _padding: [u8; 3],
    /// HID data (NS Pro Controller full report is 64 bytes)
    pub data: [u8; 64],
}

/// Size of a [`Frame`] in bytes
pub const FRAME_SIZE: usize = mem::size_of::<Frame>();

// Compile-time assertion that our assumptions are correct
const _: () = {
    assert!(FRAME_SIZE == 80, "Frame size must be 80 bytes");
    assert!(mem::align_of::<Frame>() == 1, "Frame must be packed");
};

impl Frame {
    /// Timestamp `data` with the current time
    pub fn new(seq: u32, data: &[u8]) -> Self {
        let mut frame = Frame {
            timestamp_ms: unix_ms(),
            packet_size: data.len().min(64) as u8,
            seq,
            _padding: [0; 3],
            data: [0; 64],
        };

        let copy_len = frame.packet_size as usize;
        frame.data[..copy_len].copy_from_slice(&data[..copy_len]);
        frame
    }

    /// The HID report carried by this frame; empty for a heartbeat
    pub fn payload(&self) -> &[u8] {
        &self.data[..self.packet_size as usize]
    }

    pub fn as_bytes(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self as *const _ as *const u8, FRAME_SIZE) }
    }

    pub fn from_bytes(bytes: &[u8; FRAME_SIZE]) -> Self {
        // SAFETY: Frame is plain packed data, valid for any bit pattern
        unsafe { core::ptr::read_unaligned(bytes.as_ptr() as *const Frame) }
    }
}

/// Current Unix time in milliseconds
pub fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// Trait for dumping Pro Controller input data
pub trait Dumper: Send {
    fn dump(&mut self, frame: &Frame) -> Result<()>;
    fn flush(&mut self) -> Result<()>;
}

/// Message type for async dumper communication
#[derive(Clone)]
enum DumpMessage {
    Data(Frame),
    Flush,
    Shutdown,
}

/// Async dumper that runs in a separate thread
pub struct AsyncDumper {
    sender: Sender<DumpMessage>,
    _handle: thread::JoinHandle<()>,
    queue_size: Arc<AtomicUsize>,
    drop_count: Arc<AtomicU64>,
}

/// State for the async dump thread
struct AsyncDumperState {
    dumper: Box<dyn Dumper>,
    queue_size: Arc<AtomicUsize>,
    drop_count: Arc<AtomicU64>,
    processed_count: u64,
}

impl AsyncDumperState {
    fn new(
        dumper: Box<dyn Dumper>,
        queue_size: Arc<AtomicUsize>,
        drop_count: Arc<AtomicU64>,
    ) -> Self {
        Self {
            dumper,
            queue_size,
            drop_count,
            processed_count: 0,
        }
    }

    /// Process a single dump message
    fn process_message(&mut self, message: DumpMessage) -> bool {
        self.processed_count += 1;

        match message {
            DumpMessage::Data(data) => {
                if let Err(e) = self.dumper.dump(&data) {
                    log::error!("Dump error: {}", e);
                }
                false // Continue processing
            }
            DumpMessage::Flush => {
                if let Err(e) = self.dumper.flush() {
                    log::error!("Flush error: {}", e);
                }
                false // Continue processing
            }
            DumpMessage::Shutdown => {
                let _ = self.dumper.flush(); // Final flush before shutdown
                true // Stop processing
            }
        }
    }

    /// Check for queue overflow and log warnings if needed
    fn check_queue_overflow(&self, batch_processed: usize) {
        // Update queue size estimate
        self.queue_size
            .fetch_sub(batch_processed, Ordering::Relaxed);

        // Log warnings periodically
        if self.processed_count % QUEUE_CHECK_INTERVAL == 0 {
            let size = self.queue_size.load(Ordering::Relaxed);
            let drops = self.drop_count.load(Ordering::Relaxed);

            if size > (DUMP_CHANNEL_CAPACITY as f64 * QUEUE_WARNING_THRESHOLD) as usize {
                log::warn!(
                    "Dump queue backpressure: {} items queued ({}% of capacity), {} packets dropped",
                    size,
                    (size as f64 / DUMP_CHANNEL_CAPACITY as f64 * 100.0) as u32,
                    drops
                );
            }
            if drops > 0 && self.processed_count % (QUEUE_CHECK_INTERVAL * 10) == 0 {
                log::warn!("Total packets dropped due to backpressure: {}", drops);
            }
        }
    }
}

impl AsyncDumper {
    /// Create a new async dumper with the given dumper implementation
    pub fn new(dumper: Box<dyn Dumper>) -> Self {
        let (sender, receiver) = mpsc::channel::<DumpMessage>();
        let queue_size = Arc::new(AtomicUsize::new(0));
        let drop_count = Arc::new(AtomicU64::new(0));

        let queue_size_worker = Arc::clone(&queue_size);
        let drop_count_worker = Arc::clone(&drop_count);

        let handle = thread::spawn(move || {
            Self::dump_thread_worker(receiver, dumper, queue_size_worker, drop_count_worker);
        });

        AsyncDumper {
            sender,
            _handle: handle,
            queue_size,
            drop_count,
        }
    }

    /// Worker function that runs in the dump thread
    fn dump_thread_worker(
        receiver: Receiver<DumpMessage>,
        dumper: Box<dyn Dumper>,
        queue_size: Arc<AtomicUsize>,
        drop_count: Arc<AtomicU64>,
    ) {
        let mut state = AsyncDumperState::new(dumper, queue_size, drop_count);

        loop {
            // Process all available messages in the queue
            let mut batch_processed = 0;

            loop {
                match receiver.try_recv() {
                    Ok(message) => {
                        batch_processed += 1;
                        if state.process_message(message) {
                            // Shutdown requested
                            return;
                        }
                    }
                    Err(TryRecvError::Empty) => {
                        // No more messages available, break to update stats
                        break;
                    }
                    Err(TryRecvError::Disconnected) => {
                        // Channel closed, shutdown
                        let _ = state.dumper.flush();
                        return;
                    }
                }
            }

            // Check for queue overflow and log warnings if needed
            state.check_queue_overflow(batch_processed);

            // If no messages were processed, block on next message
            if batch_processed == 0 {
                match receiver.recv() {
                    Ok(message) => {
                        if state.process_message(message) {
                            // Shutdown requested
                            return;
                        }
                        // Update queue size for the single processed message
                        state.check_queue_overflow(1);
                    }
                    Err(_) => {
                        // Channel closed
                        let _ = state.dumper.flush();
                        return;
                    }
                }
            }
        }
    }
}

impl Dumper for AsyncDumper {
    fn dump(&mut self, frame: &Frame) -> Result<()> {
        // Check current queue size and implement backpressure
        let current_size = self.queue_size.load(Ordering::Relaxed);

        if current_size >= DUMP_CHANNEL_CAPACITY {
            // Queue is full, drop this packet and increment drop counter
            self.drop_count.fetch_add(1, Ordering::Relaxed);
            // Don't log every dropped packet to avoid log spam
            return Ok(());
        }

        // Count the frame before sending it: the dump thread may receive it
        // and subtract before this thread gets to add, wrapping the count
        self.queue_size.fetch_add(1, Ordering::Relaxed);
        if self.sender.send(DumpMessage::Data(*frame)).is_err() {
            self.queue_size.fetch_sub(1, Ordering::Relaxed);
            log::warn!("Dump thread has shut down");
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        // Flush is always sent (it's important for data integrity); the dump
        // thread subtracts every message it takes, so count this one too
        self.queue_size.fetch_add(1, Ordering::Relaxed);
        if self.sender.send(DumpMessage::Flush).is_err() {
            self.queue_size.fetch_sub(1, Ordering::Relaxed);
            log::warn!("Dump thread has shut down");
        }
        Ok(())
    }
}

impl Drop for AsyncDumper {
    fn drop(&mut self) {
        // Send shutdown message
        let _ = self.sender.send(DumpMessage::Shutdown);
        // Note: We can't wait for the thread here as _handle is moved
    }
}

/// File-based dumper that writes timestamped frames to a binary file
pub struct FileDumper {
    writer: BufWriter<std::fs::File>,
    frame_count: u64,
}

impl FileDumper {
    pub fn new(file_path: impl AsRef<Path>) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file_path)?;

        let writer = BufWriter::new(file);

        log::info!("FileDumper created: {}", file_path.as_ref().display());
        log::info!("Frame size: {} bytes", FRAME_SIZE);

        Ok(FileDumper {
            writer,
            frame_count: 0,
        })
    }

    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }

    /// Flush buffered frames and wait until they reach the disk
    pub fn sync(&mut self) -> Result<()> {
        self.writer.flush()?;
        self.writer.get_ref().sync_all()?;
        Ok(())
    }

    /// Calculate file position for a given frame number
    pub fn frame_offset(frame_number: u64) -> u64 {
        frame_number * FRAME_SIZE as u64
    }

    /// Get the size of each frame in bytes
    pub fn get_frame_size() -> usize {
        FRAME_SIZE
    }
}

impl Dumper for FileDumper {
    fn dump(&mut self, frame: &Frame) -> Result<()> {
        self.writer.write_all(frame.as_bytes())?;
        self.frame_count += 1;

        // Flush every FLUSH_INTERVAL frames
        if self.frame_count % FLUSH_INTERVAL == 0 {
            self.writer.flush()?;
            log::debug!("Dumped {} frames", self.frame_count);
        }

        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.writer.flush()?;
        Ok(())
    }
}

/// Console dumper that outputs formatted packet information to stdout (tcpdump-style)
pub struct ConsoleDumper {
    frame_count: u64,
}

impl ConsoleDumper {
    /// Create a new console dumper
    pub fn new() -> Self {
        ConsoleDumper { frame_count: 0 }
    }
    fn format_timestamp(&self, timestamp_ms: u64) -> String {
        let ms = timestamp_ms % 1000;
        let sec = (timestamp_ms / 1000) % 60;
        let min = (timestamp_ms / 60000) % 60;
        let hour = (timestamp_ms / 3600000) % 24;
        format!("{:02}:{:02}:{:02}.{:03}", hour, min, sec, ms)
    }

    fn format_buttons(&self, buttons: &ButtonState) -> String {
        let mut pressed: SmallVec<[&str; 8]> = SmallVec::new();

        // Face buttons
        if buttons.a {
            pressed.push("A");
        }
        if buttons.b {
            pressed.push("B");
        }
        if buttons.x {
            pressed.push("X");
        }
        if buttons.y {
            pressed.push("Y");
        }

        // Shoulder buttons
        if buttons.l {
            pressed.push("L");
        }
        if buttons.r {
            pressed.push("R");
        }
        if buttons.zl {
            pressed.push("ZL");
        }
        if buttons.zr {
            pressed.push("ZR");
        }

        // D-pad
        if buttons.up {
            pressed.push("UP");
        }
        if buttons.down {
            pressed.push("DN");
        }
        if buttons.left {
            pressed.push("LT");
        }
        if buttons.right {
            pressed.push("RT");
        }

        // System buttons
        if buttons.minus {
            pressed.push("-");
        }
        if buttons.plus {
            pressed.push("+");
        }
        if buttons.home {
            pressed.push("HOME");
        }
        if buttons.capture {
            pressed.push("CAP");
        }

        // Stick clicks
        if buttons.l_stick {
            pressed.push("LS");
        }
        if buttons.r_stick {
            pressed.push("RS");
        }

        if pressed.is_empty() {
            "---".to_string()
        } else {
            pressed.join(",")
        }
    }

    fn format_sticks(&self, left: &StickData, right: &StickData) -> String {
        // Convert to percentage (center is ~2048)
        let left_x_pct = ((left.x as i32 - 2048) * 100 / 2048).clamp(-100, 100);
        let left_y_pct = ((left.y as i32 - 2048) * 100 / 2048).clamp(-100, 100);
        let right_x_pct = ((right.x as i32 - 2048) * 100 / 2048).clamp(-100, 100);
        let right_y_pct = ((right.y as i32 - 2048) * 100 / 2048).clamp(-100, 100);

        format!(
            "L:{:+3},{:+3} R:{:+3},{:+3}",
            left_x_pct, left_y_pct, right_x_pct, right_y_pct
        )
    }

    fn decode_frame_compact(&self, frame: &Frame) -> String {
        let data = &frame.data[..frame.packet_size as usize];

        if data.is_empty() {
            return "Empty".to_string();
        }

        match data[0] {
            0x30 => {
                // Input report - use ProConParser to get ControllerState
                match ProConParser::parse_input_report(data) {
                    Ok(state) => {
                        let buttons = self.format_buttons(&state.buttons);
                        let sticks = self.format_sticks(&state.left_stick, &state.right_stick);
                        format!("Input [{}] {}", buttons, sticks)
                    }
                    Err(_) => "Input (parse error)".to_string(),
                }
            }
            0x21 => "SubcommandReply".to_string(),
            0x81 => "RequestMac".to_string(),
            0x01 => "Subcommand".to_string(),
            0x10 => "Rumble".to_string(),
            _ => format!("Unknown(0x{:02x})", data[0]),
        }
    }
}

impl Dumper for ConsoleDumper {
    fn dump(&mut self, frame: &Frame) -> Result<()> {
        let timestamp = self.format_timestamp(frame.timestamp_ms);

        let decoded = self.decode_frame_compact(frame);
        println!("{} #{:08} {}", timestamp, self.frame_count, decoded);

        self.frame_count += 1;
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Multi-dumper that can write to multiple dumpers simultaneously
pub struct MultiDumper {
    dumpers: Vec<Box<dyn Dumper>>,
}

impl MultiDumper {
    pub fn new() -> Self {
        MultiDumper {
            dumpers: Vec::new(),
        }
    }

    pub fn add_dumper(&mut self, dumper: Box<dyn Dumper>) {
        self.dumpers.push(dumper);
    }
}

impl Dumper for MultiDumper {
    fn dump(&mut self, frame: &Frame) -> Result<()> {
        for dumper in &mut self.dumpers {
            dumper.dump(frame)?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        for dumper in &mut self.dumpers {
            dumper.flush()?;
        }
        Ok(())
    }
}
