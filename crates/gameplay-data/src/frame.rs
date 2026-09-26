//! The 80-byte record of `controller.bin`: one controller report as the
//! proxy read it, little endian and packed.
//!
//! | offset | size | field |
//! |-------:|-----:|-------|
//! | 0 | 8 | `timestamp_ms`: Unix ms on the proxy's clock when the report was read |
//! | 8 | 1 | `packet_size`: valid report bytes; 0 marks a heartbeat |
//! | 9 | 4 | `seq`: sequence number, gaps are dropped reports |
//! | 13 | 2 | `forward_us`: µs until the console took the report; 0 = unknown |
//! | 15 | 1 | padding |
//! | 16 | 64 | HID input report |
//!
//! Older files have zeros in place of `forward_us`, so they read the same.

/// Size of a record in bytes
pub const FRAME_SIZE: usize = 80;

/// Size of the HID report in a record
pub const REPORT_SIZE: usize = 64;

/// One decoded record
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame {
    /// Unix ms on the proxy's clock when the report was read
    pub timestamp_ms: u64,
    /// Valid bytes of `report`; zero marks a heartbeat without data
    pub packet_size: u8,
    /// Sequence number, so gaps reveal dropped reports
    pub seq: u32,
    /// Microseconds until the console took the report; zero when unknown
    pub forward_us: u16,
    /// HID input report, report id at byte 0
    pub report: [u8; REPORT_SIZE],
}

impl Frame {
    /// A record of `data` (up to [`REPORT_SIZE`] bytes; empty for a
    /// heartbeat) read at `timestamp_ms`
    pub fn new(timestamp_ms: u64, seq: u32, data: &[u8]) -> Self {
        let size = data.len().min(REPORT_SIZE);
        let mut report = [0; REPORT_SIZE];
        report[..size].copy_from_slice(&data[..size]);
        Self {
            timestamp_ms,
            packet_size: size as u8,
            seq,
            forward_us: 0,
            report,
        }
    }

    /// The valid bytes of the report; empty for a heartbeat
    pub fn payload(&self) -> &[u8] {
        &self.report[..usize::from(self.packet_size).min(REPORT_SIZE)]
    }

    /// Decode one record
    pub fn parse(bytes: &[u8; FRAME_SIZE]) -> Self {
        let mut report = [0; REPORT_SIZE];
        report.copy_from_slice(&bytes[16..]);
        Self {
            timestamp_ms: u64::from_le_bytes(bytes[0..8].try_into().unwrap()),
            packet_size: bytes[8],
            seq: u32::from_le_bytes(bytes[9..13].try_into().unwrap()),
            forward_us: u16::from_le_bytes(bytes[13..15].try_into().unwrap()),
            report,
        }
    }

    /// Encode as a record
    pub fn to_bytes(&self) -> [u8; FRAME_SIZE] {
        let mut bytes = [0; FRAME_SIZE];
        bytes[0..8].copy_from_slice(&self.timestamp_ms.to_le_bytes());
        bytes[8] = self.packet_size;
        bytes[9..13].copy_from_slice(&self.seq.to_le_bytes());
        bytes[13..15].copy_from_slice(&self.forward_us.to_le_bytes());
        bytes[16..].copy_from_slice(&self.report);
        bytes
    }

    /// Whether this is a heartbeat rather than a report
    pub fn is_heartbeat(&self) -> bool {
        self.packet_size == 0
    }
}

/// Decode every whole record of a file's bytes; a partial record at the end
/// (a file still being written) is ignored
pub fn parse_frames(bytes: &[u8]) -> impl Iterator<Item = Frame> + '_ {
    bytes.as_chunks::<FRAME_SIZE>().0.iter().map(Frame::parse)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut report = [0; REPORT_SIZE];
        report[0] = 0x30;
        report[63] = 0xAB;
        let frame = Frame {
            timestamp_ms: 1_790_321_071_806,
            packet_size: 64,
            seq: 0xDEAD_BEEF,
            forward_us: 567,
            report,
        };
        let bytes = frame.to_bytes();
        assert_eq!(bytes[8], 64);
        assert_eq!(&bytes[13..15], &567u16.to_le_bytes());
        assert_eq!(bytes[15], 0);
        assert_eq!(Frame::parse(&bytes), frame);
    }

    #[test]
    fn new_and_payload() {
        let frame = Frame::new(5, 9, &[0x30, 1, 2]);
        assert_eq!(frame.payload(), [0x30, 1, 2]);
        assert!(Frame::new(5, 9, &[]).is_heartbeat());
        assert_eq!(Frame::new(5, 9, &[7; 100]).payload().len(), REPORT_SIZE);
    }

    #[test]
    fn partial_record_is_ignored() {
        let mut bytes = alloc::vec![0; FRAME_SIZE * 2 + 5];
        bytes[FRAME_SIZE + 8] = 64;
        let frames: alloc::vec::Vec<_> = parse_frames(&bytes).collect();
        assert_eq!(frames.len(), 2);
        assert!(frames[0].is_heartbeat());
        assert!(!frames[1].is_heartbeat());
    }
}
