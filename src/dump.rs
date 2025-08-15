use anyhow::Result;
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::mem;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::keystate::{ButtonState, StickData};
use crate::parser::ProConParser;

const FLUSH_INTERVAL: u64 = 1000;

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct Frame {
    pub timestamp_ms: u64, // Millisecond Unix timestamp
    pub packet_size: u8,   // Actual packet size
    _padding: [u8; 7],     // Padding to 16-byte alignment
    pub data: [u8; 64],    // HID data (NS Pro Controller full report is 64 bytes)
}

const FRAME_SIZE: usize = mem::size_of::<Frame>();

// Compile-time assertion that our assumptions are correct
const _: () = {
    assert!(FRAME_SIZE == 80, "Frame size must be 80 bytes");
    assert!(mem::align_of::<Frame>() == 1, "Frame must be packed");
};

impl Frame {
    fn new(data: &[u8]) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let mut frame = Frame {
            timestamp_ms: now,
            packet_size: data.len().min(64) as u8,
            _padding: [0; 7],
            data: [0; 64],
        };

        let copy_len = frame.packet_size as usize;
        frame.data[..copy_len].copy_from_slice(&data[..copy_len]);
        frame
    }

    fn as_bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self as *const _ as *const u8, FRAME_SIZE) }
    }
}

/// Trait for dumping Pro Controller input data
pub trait Dumper {
    fn dump(&mut self, data: &[u8]) -> Result<()>;
    fn flush(&mut self) -> Result<()>;
}

/// File-based dumper that writes timestamped frames to a binary file
pub struct FileDumper {
    writer: BufWriter<std::fs::File>,
    frame_count: u64,
}

impl FileDumper {
    pub fn new(file_path: &str) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(file_path)?;

        let writer = BufWriter::new(file);

        log::info!("FileDumper created: {}", file_path);
        log::info!("Frame size: {} bytes", FRAME_SIZE);

        Ok(FileDumper {
            writer,
            frame_count: 0,
        })
    }

    pub fn frame_count(&self) -> u64 {
        self.frame_count
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
    fn dump(&mut self, data: &[u8]) -> Result<()> {
        let frame = Frame::new(data);
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
        let mut pressed = Vec::new();

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
    fn dump(&mut self, data: &[u8]) -> Result<()> {
        let frame = Frame::new(data);

        let timestamp = self.format_timestamp(frame.timestamp_ms);

        let decoded = self.decode_frame_compact(&frame);
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
    fn dump(&mut self, data: &[u8]) -> Result<()> {
        for dumper in &mut self.dumpers {
            dumper.dump(data)?;
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
