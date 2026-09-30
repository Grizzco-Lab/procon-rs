//! Capture from a V4L2 device directly: the kernel's own buffers mapped into
//! memory, a few of them, each frame handed on as soon as it is dequeued,
//! with the kernel's timestamp.
//!
//! Only what the lab's capture card needs: YUYV frames of one size at one
//! rate, memory-mapped streaming. ffmpeg's v4l2 input does the same with 256
//! buffers asked for (its `desired_video_buffers`, with no option; uvcvideo
//! grants 32), so frames it falls behind on wait in the kernel for good, and
//! each frame then passes its decoder, filter, encoder and muxer threads
//! before a pipe. Here [`BUFFERS`] are queued: when the reader falls behind,
//! the driver drops frames instead of queueing them, and the reader takes
//! whatever is ready, newest last.
//!
//! The timestamp is the kernel's `CLOCK_MONOTONIC` time of the frame; for
//! uvcvideo (without its `hwtimestamps` parameter), when its first USB packet
//! arrived.
//!
//! The ioctls and structures are those of `linux/videodev2.h` on 64-bit
//! Linux; their sizes and offsets are checked at compile time against the
//! header's (`sizeof`, `offsetof` on x86-64).

use anyhow::{Context, Result, ensure};
use core::mem::{offset_of, size_of};
use core::time::Duration;
use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

/// Buffers queued with the driver: one it fills, one ready, one the reader
/// reads, one the reader holds for a repeat (see `video::ConstantRate`)
pub const BUFFERS: u32 = 4;

/// `V4L2_PIX_FMT_YUYV`: 4:2:2, Y0 U Y1 V
pub const YUYV: u32 = u32::from_le_bytes(*b"YUYV");

const VIDIOC_QUERYCAP: libc::c_ulong = 0x8068_5600;
const VIDIOC_S_FMT: libc::c_ulong = 0xc0d0_5605;
const VIDIOC_REQBUFS: libc::c_ulong = 0xc014_5608;
const VIDIOC_QUERYBUF: libc::c_ulong = 0xc058_5609;
const VIDIOC_QBUF: libc::c_ulong = 0xc058_560f;
const VIDIOC_DQBUF: libc::c_ulong = 0xc058_5611;
const VIDIOC_STREAMON: libc::c_ulong = 0x4004_5612;
const VIDIOC_STREAMOFF: libc::c_ulong = 0x4004_5613;
const VIDIOC_S_PARM: libc::c_ulong = 0xc0cc_5616;

const CAP_VIDEO_CAPTURE: u32 = 0x1;
const CAP_STREAMING: u32 = 0x0400_0000;
const CAP_DEVICE_CAPS: u32 = 0x8000_0000;
const BUF_TYPE_VIDEO_CAPTURE: u32 = 1;
const MEMORY_MMAP: u32 = 1;
const FIELD_ANY: u32 = 0;
const BUF_FLAG_ERROR: u32 = 0x40;
const BUF_FLAG_TIMESTAMP_MASK: u32 = 0xe000;
const BUF_FLAG_TIMESTAMP_MONOTONIC: u32 = 0x2000;

/// `struct v4l2_capability`
#[repr(C)]
#[allow(dead_code)] // the kernel's layout; not every field is read
struct Capability {
    driver: [u8; 16],
    card: [u8; 32],
    bus_info: [u8; 32],
    version: u32,
    capabilities: u32,
    device_caps: u32,
    reserved: [u32; 3],
}

/// `struct v4l2_pix_format`
#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)] // the kernel's layout; not every field is read
struct PixFormat {
    width: u32,
    height: u32,
    pixelformat: u32,
    field: u32,
    bytesperline: u32,
    sizeimage: u32,
    colorspace: u32,
    private: u32,
    flags: u32,
    ycbcr_enc: u32,
    quantization: u32,
    xfer_func: u32,
}

/// `struct v4l2_format` with its union as `pix`: the union is 8-byte
/// aligned (it holds pointers), so it starts at 8
#[repr(C)]
#[allow(dead_code)] // the kernel's layout; not every field is read
struct Format {
    kind: u32,
    align: u32,
    pix: PixFormat,
    rest: [u8; 200 - size_of::<PixFormat>()],
}

/// `struct v4l2_requestbuffers`
#[repr(C)]
#[allow(dead_code)] // the kernel's layout; not every field is read
struct RequestBuffers {
    count: u32,
    kind: u32,
    memory: u32,
    capabilities: u32,
    flags: u8,
    reserved: [u8; 3],
}

/// `struct v4l2_buffer`, its union `m` as the `u64` it takes (`offset` in
/// the low half on little-endian machines)
#[repr(C)]
#[allow(dead_code)] // the kernel's layout; not every field is read
struct Buffer {
    index: u32,
    kind: u32,
    bytesused: u32,
    flags: u32,
    field: u32,
    timestamp: libc::timeval,
    timecode: [u32; 4],
    sequence: u32,
    memory: u32,
    m: u64,
    length: u32,
    reserved2: u32,
    request_fd: u32,
}

/// `struct v4l2_streamparm` with its union as `struct v4l2_captureparm`'s
/// first fields
#[repr(C)]
#[allow(dead_code)] // the kernel's layout; not every field is read
struct StreamParm {
    kind: u32,
    capability: u32,
    capturemode: u32,
    numerator: u32,
    denominator: u32,
    rest: [u8; 200 - 16],
}

// The header's sizes and offsets on x86-64
const _: () = assert!(size_of::<Capability>() == 104 && offset_of!(Capability, capabilities) == 84);
const _: () = assert!(size_of::<PixFormat>() == 48);
const _: () = assert!(size_of::<Format>() == 208 && offset_of!(Format, pix) == 8);
const _: () = assert!(size_of::<RequestBuffers>() == 20);
const _: () = assert!(size_of::<Buffer>() == 88);
const _: () = assert!(offset_of!(Buffer, timestamp) == 24 && offset_of!(Buffer, sequence) == 56);
const _: () = assert!(offset_of!(Buffer, m) == 64 && offset_of!(Buffer, length) == 72);
const _: () = assert!(size_of::<StreamParm>() == 204 && offset_of!(StreamParm, numerator) == 12);

/// All zeros, as V4L2 wants its structures before filling them in
fn zeroed<T>() -> T {
    // SAFETY: only used for the plain C structures above, all integers
    unsafe { core::mem::zeroed() }
}

/// One ioctl, again when a signal interrupts it
fn ioctl<T>(fd: &OwnedFd, request: libc::c_ulong, arg: &mut T) -> std::io::Result<()> {
    loop {
        // SAFETY: `arg` is the structure `request` takes (sizes checked above)
        if unsafe { libc::ioctl(fd.as_raw_fd(), request as _, arg as *mut T) } >= 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// A frame the driver filled, dequeued: its buffer is the reader's until
/// [`Capture::requeue`]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dequeued {
    /// The buffer holding it
    pub index: u32,
    /// The kernel's timestamp (`CLOCK_MONOTONIC`, ns), when it is one
    pub captured: Option<u64>,
    /// The driver's count of frames, gaps where it dropped some
    pub sequence: u32,
    /// Bytes of it filled
    pub bytes: u32,
    /// The driver marked it corrupted
    pub error: bool,
}

/// A device streaming YUYV frames into [`BUFFERS`] memory-mapped buffers
pub struct Capture {
    fd: OwnedFd,
    /// Each buffer's mapping and length
    maps: Vec<(*mut libc::c_void, usize)>,
    width: u32,
    height: u32,
}

// SAFETY: the mappings belong to the capture and are only reached through
// it, from one thread at a time
unsafe impl Send for Capture {}

impl Capture {
    /// Open `path` and stream `width` x `height` YUYV at `fps`
    pub fn open(path: &Path, width: u32, height: u32, fps: u32) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
            .with_context(|| format!("cannot open {}", path.display()))?;
        let fd = OwnedFd::from(file);
        let mut capability: Capability = zeroed();
        ioctl(&fd, VIDIOC_QUERYCAP, &mut capability).context("not a V4L2 device")?;
        let caps = if capability.capabilities & CAP_DEVICE_CAPS != 0 {
            capability.device_caps
        } else {
            capability.capabilities
        };
        ensure!(
            caps & CAP_VIDEO_CAPTURE != 0 && caps & CAP_STREAMING != 0,
            "{} cannot stream video",
            path.display()
        );

        let mut format: Format = zeroed();
        format.kind = BUF_TYPE_VIDEO_CAPTURE;
        format.pix.width = width;
        format.pix.height = height;
        format.pix.pixelformat = YUYV;
        format.pix.field = FIELD_ANY;
        ioctl(&fd, VIDIOC_S_FMT, &mut format).context("cannot set the format")?;
        let pix = format.pix;
        ensure!(
            (pix.width, pix.height, pix.pixelformat) == (width, height, YUYV),
            "the device offers {}x{} {:?} instead of {width}x{height} YUYV",
            pix.width,
            pix.height,
            pix.pixelformat.to_le_bytes().map(char::from)
        );
        ensure!(
            pix.bytesperline == width * 2,
            "lines of {} bytes: padded lines are not read here",
            pix.bytesperline
        );

        let mut parm: StreamParm = zeroed();
        parm.kind = BUF_TYPE_VIDEO_CAPTURE;
        parm.numerator = 1;
        parm.denominator = fps;
        if let Err(e) = ioctl(&fd, VIDIOC_S_PARM, &mut parm) {
            log::warn!("{} keeps its frame rate: {e}", path.display());
        }

        let mut request: RequestBuffers = zeroed();
        request.count = BUFFERS;
        request.kind = BUF_TYPE_VIDEO_CAPTURE;
        request.memory = MEMORY_MMAP;
        ioctl(&fd, VIDIOC_REQBUFS, &mut request).context("no buffers")?;
        ensure!(
            request.count >= 2,
            "the driver gave {} buffers",
            request.count
        );
        let mut capture = Self {
            fd,
            maps: Vec::new(),
            width,
            height,
        };
        for index in 0..request.count {
            let mut buffer = capture.buffer(index);
            ioctl(&capture.fd, VIDIOC_QUERYBUF, &mut buffer).context("cannot query a buffer")?;
            let length = buffer.length as usize;
            ensure!(
                length >= capture.frame_bytes(),
                "a buffer of {length} bytes is too small"
            );
            // SAFETY: maps the buffer the driver just described, at its offset
            let map = unsafe {
                libc::mmap(
                    core::ptr::null_mut(),
                    length,
                    libc::PROT_READ,
                    libc::MAP_SHARED,
                    capture.fd.as_raw_fd(),
                    (buffer.m as u32) as libc::off_t,
                )
            };
            if map == libc::MAP_FAILED {
                return Err(std::io::Error::last_os_error()).context("cannot map a buffer");
            }
            capture.maps.push((map, length));
            ioctl(&capture.fd, VIDIOC_QBUF, &mut buffer).context("cannot queue a buffer")?;
        }
        let mut kind = BUF_TYPE_VIDEO_CAPTURE as libc::c_int;
        ioctl(&capture.fd, VIDIOC_STREAMON, &mut kind).context("cannot start streaming")?;
        Ok(capture)
    }

    /// A buffer structure for buffer `index`, to fill
    fn buffer(&self, index: u32) -> Buffer {
        let mut buffer: Buffer = zeroed();
        buffer.index = index;
        buffer.kind = BUF_TYPE_VIDEO_CAPTURE;
        buffer.memory = MEMORY_MMAP;
        buffer
    }

    /// Width and height of the frames
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Bytes of a whole frame
    pub fn frame_bytes(&self) -> usize {
        (self.width * self.height * 2) as usize
    }

    /// Wait up to `timeout` for a frame, then dequeue every one ready,
    /// oldest first (none when the time is up)
    pub fn ready(&mut self, timeout: Duration) -> Result<Vec<Dequeued>> {
        let mut poll = libc::pollfd {
            fd: self.fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one pollfd, ours
        let n = unsafe { libc::poll(&mut poll, 1, timeout.as_millis() as libc::c_int) };
        if n < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == ErrorKind::Interrupted {
                return Ok(Vec::new());
            }
            return Err(error).context("cannot wait for frames");
        }
        if n == 0 {
            return Ok(Vec::new());
        }
        ensure!(
            poll.revents & libc::POLLIN != 0,
            "the device stopped (poll events {:#x})",
            poll.revents
        );
        let mut frames = Vec::new();
        loop {
            let mut buffer = self.buffer(0);
            match ioctl(&self.fd, VIDIOC_DQBUF, &mut buffer) {
                Ok(()) => frames.push(Dequeued {
                    index: buffer.index,
                    captured: (buffer.flags & BUF_FLAG_TIMESTAMP_MASK
                        == BUF_FLAG_TIMESTAMP_MONOTONIC)
                        .then(|| {
                            buffer.timestamp.tv_sec as u64 * 1_000_000_000
                                + buffer.timestamp.tv_usec as u64 * 1000
                        }),
                    sequence: buffer.sequence,
                    bytes: buffer.bytesused,
                    error: buffer.flags & BUF_FLAG_ERROR != 0,
                }),
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) => return Err(e).context("cannot dequeue a frame"),
            }
        }
        Ok(frames)
    }

    /// The frame in buffer `index`, dequeued and not requeued yet
    pub fn frame(&self, index: u32) -> &[u8] {
        let (map, _) = self.maps[index as usize];
        // SAFETY: the mapping holds at least a frame (checked when mapped),
        // and the driver leaves a dequeued buffer alone until it is queued
        unsafe { core::slice::from_raw_parts(map as *const u8, self.frame_bytes()) }
    }

    /// Give buffer `index` back to the driver to fill
    pub fn requeue(&mut self, index: u32) -> Result<()> {
        let mut buffer = self.buffer(index);
        ioctl(&self.fd, VIDIOC_QBUF, &mut buffer).context("cannot queue a buffer")
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let mut kind = BUF_TYPE_VIDEO_CAPTURE as libc::c_int;
        let _ = ioctl(&self.fd, VIDIOC_STREAMOFF, &mut kind);
        for &(map, length) in &self.maps {
            // SAFETY: a mapping made in `open`, unmapped once
            unsafe { libc::munmap(map, length) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_that_is_no_device_is_refused() {
        let error = Capture::open(Path::new("/dev/null"), 1920, 1080, 60)
            .err()
            .unwrap();
        assert!(
            format!("{error:#}").contains("not a V4L2 device"),
            "{error:#}"
        );
        assert!(Capture::open(Path::new("/nonexistent/video9"), 1920, 1080, 60).is_err());
    }

    #[test]
    fn yuyv_is_the_headers_fourcc() {
        assert_eq!(YUYV, 0x5659_5559);
    }
}
