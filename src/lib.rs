//! `audio`: sound in and out on Linux, our own, with nothing but std.
//!
//! - [`pulse`]: a client of PulseAudio's native protocol, which PipeWire
//!   serves as well (`pipewire-pulse`): sources and sinks listed, recording
//!   and playing float samples, the server converting formats and rates.
//! - [`tap`]: recording in a background thread into a ring of the last few
//!   seconds: the newest sound each frame, or every frame in turn by number,
//!   told of any the ring overwrote before they were read.
//! - [`wav`]: WAV files read and written.
//!
//! ```no_run
//! use audio::{Client, Spec, Tap};
//! let client = Client::connect("myapp")?;
//! // What the default sink plays; `None` for the default source (a microphone).
//! let stream = client.record(Some(audio::DEFAULT_MONITOR), Spec { rate: 48000, channels: 2 }, 512)?;
//! let tap = Tap::start(stream, 2.0)?;
//! let mut samples = Vec::new();
//! tap.latest(4096, &mut samples); // the newest 4096 frames, interleaved
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

pub mod pulse;
mod tagstruct;
pub mod tap;
pub mod wav;

pub use pulse::{Client, Device, Playback, Record, ServerInfo};
pub use tap::{Gap, Tap};
pub use wav::Wav;

/// The source that is what the default sink plays (its monitor), by the
/// name PulseAudio and PipeWire both resolve.
pub const DEFAULT_MONITOR: &str = "@DEFAULT_MONITOR@";

/// How sound is laid out: frames per second, and samples per frame
/// (interleaved). Samples are always `f32`, -1 to 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Spec {
    pub rate: u32,
    pub channels: u16,
}
