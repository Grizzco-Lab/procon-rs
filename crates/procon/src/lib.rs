//! What the USB proxy (`procon-proxy`, on the Raspberry Pi) and Grizzco Lab
//! (`grizzco-lab`, on the capture host) share: the Pro Controller's frames on
//! their way through both.
//!
//! - [`dump`]: frames stamped with the time and handed to [`dump::Dumper`]s
//!   (a thread of their own, files, several at once);
//! - [`recorder`]: session folders with their `controller.bin`, started,
//!   paused and stopped;
//! - [`stream`]: the frame link, the proxy's [`stream::FrameStreamer`] and
//!   the lab's [`stream::receive_frames`];
//! - [`replay`]: replayed [`replay::Action`]s as JSON lines, and the proxy's
//!   replay port that applies them;
//! - [`config`]: loading either binary's TOML file and its `[logging]`
//!   section.
//!
//! The frame record itself is `gameplay-data`'s, which the training code
//! reads too.

extern crate alloc;

pub mod config;
pub mod dump;
pub mod recorder;
pub mod replay;
pub mod stream;
