//! Recording in the background: a thread reads a [`Record`] stream into a
//! ring of the last few seconds, which the program looks into when it
//! wants, never waiting for sound. Either the newest samples, each frame
//! (what a meter needs), or every sample in turn by frame number, told when
//! it fell behind and the ring overwrote some (what an analyser that
//! misses nothing needs).

use std::fmt;

use std::net::Shutdown;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};

use crate::Spec;
use crate::pulse::Record;

#[derive(Debug)]
struct Ring {
    /// Interleaved samples, `len` of them; the oldest at `next` once full.
    samples: Vec<f32>,
    next: usize,
    /// Frames ever written.
    total: u64,
    /// Why recording stopped, if it has.
    error: Option<String>,
}

/// Why [`Tap::read`] couldn't give the frames asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gap {
    /// Some are gone, overwritten: the oldest the ring still holds.
    Gone { oldest: u64 },
    /// Some haven't come yet: the frames recorded so far.
    NotYet { total: u64 },
}

impl fmt::Display for Gap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Gap::Gone { oldest } => write!(f, "overwritten; the oldest frame held is {oldest}"),
            Gap::NotYet { total } => write!(f, "not recorded yet; {total} frames so far"),
        }
    }
}

impl std::error::Error for Gap {}

/// A source recorded into a ring in the background; stopped on drop.
#[derive(Debug)]
pub struct Tap {
    ring: Arc<Mutex<Ring>>,
    spec: Spec,
    device: String,
    socket: Option<std::os::unix::net::UnixStream>,
    thread: Option<JoinHandle<()>>,
}

impl Tap {
    /// Record `stream` in a thread, keeping the last `seconds`.
    pub fn start(mut stream: Record, seconds: f32) -> std::io::Result<Tap> {
        let spec = stream.spec();
        let frames = ((spec.rate as f32 * seconds.max(0.01)) as usize).max(1);
        let ring = Arc::new(Mutex::new(Ring {
            samples: vec![0.0; frames * spec.channels as usize],
            next: 0,
            total: 0,
            error: None,
        }));
        let socket = stream.socket()?;
        let device = stream.device().to_string();
        let shared = ring.clone();
        let thread = thread::Builder::new().name("audio tap".into()).spawn(move || {
            let mut buf = Vec::new();
            loop {
                buf.clear();
                if let Err(e) = stream.read(&mut buf) {
                    lock(&shared).error = Some(e.to_string());
                    return;
                }
                let mut r = lock(&shared);
                r.push(&buf, spec.channels as usize);
            }
        })?;
        Ok(Tap { ring, spec, device, socket: Some(socket), thread: Some(thread) })
    }

    pub fn spec(&self) -> Spec {
        self.spec
    }

    /// The source's name, as the server resolved it.
    pub fn device(&self) -> &str {
        &self.device
    }

    /// The newest `frames` frames, oldest first, interleaved, into `out`
    /// (zeros before the first sound); and the frames recorded so far, which
    /// tells new sound from old.
    pub fn latest(&self, frames: usize, out: &mut Vec<f32>) -> u64 {
        let r = lock(&self.ring);
        r.latest(frames, self.spec.channels as usize, out);
        r.total
    }

    /// Frames recorded so far: the number of the next to come.
    pub fn total(&self) -> u64 {
        lock(&self.ring).total
    }

    /// Frames the ring holds: how far a reader may fall behind before
    /// frames are lost.
    pub fn capacity(&self) -> usize {
        lock(&self.ring).samples.len() / self.spec.channels as usize
    }

    /// Frames `from` to `from + frames` (numbered from the first recorded),
    /// interleaved, into `out`; all of them or none.
    pub fn read(&self, from: u64, frames: usize, out: &mut Vec<f32>) -> Result<(), Gap> {
        let r = lock(&self.ring);
        r.read(from, frames, self.spec.channels as usize, out)
    }

    /// Why recording stopped, if it has (the stream ended, the server went).
    pub fn error(&self) -> Option<String> {
        lock(&self.ring).error.clone()
    }
}

impl Drop for Tap {
    fn drop(&mut self) {
        // Ends the thread's read, and with it the thread.
        if let Some(s) = self.socket.take() {
            let _ = s.shutdown(Shutdown::Both);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn lock(ring: &Mutex<Ring>) -> MutexGuard<'_, Ring> {
    // A panic mid-push leaves samples, which are still samples.
    ring.lock().unwrap_or_else(|e| e.into_inner())
}

impl Ring {
    fn push(&mut self, samples: &[f32], channels: usize) {
        let len = self.samples.len();
        self.total += (samples.len() / channels) as u64;
        // More than fits: only the end of it.
        let samples = &samples[samples.len().saturating_sub(len)..];
        let first = samples.len().min(len - self.next);
        self.samples[self.next..self.next + first].copy_from_slice(&samples[..first]);
        self.samples[..samples.len() - first].copy_from_slice(&samples[first..]);
        self.next = (self.next + samples.len()) % len;
    }

    fn read(&self, from: u64, frames: usize, channels: usize, out: &mut Vec<f32>) -> Result<(), Gap> {
        let len = self.samples.len();
        let oldest = self.total.saturating_sub((len / channels) as u64);
        if from < oldest {
            return Err(Gap::Gone { oldest });
        }
        if from + frames as u64 > self.total {
            return Err(Gap::NotYet { total: self.total });
        }
        // Back from the newest: the ring's `next` is frame `total`.
        let back = (self.total - from) as usize * channels;
        let start = (self.next + len - back) % len;
        let want = frames * channels;
        let first = want.min(len - start);
        out.clear();
        out.extend(&self.samples[start..start + first]);
        out.extend(&self.samples[..want - first]);
        Ok(())
    }

    fn latest(&self, frames: usize, channels: usize, out: &mut Vec<f32>) {
        let len = self.samples.len();
        let want = frames * channels;
        out.clear();
        // Older than the ring holds, or than anything recorded: silence.
        let have = (self.total.min((len / channels) as u64) as usize * channels).min(want);
        out.resize(want - have, 0.0);
        let start = (self.next + len - have) % len;
        let first = have.min(len - start);
        out.extend(&self.samples[start..start + first]);
        out.extend(&self.samples[..have - first]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring(frames: usize) -> Ring {
        Ring { samples: vec![0.0; frames], next: 0, total: 0, error: None }
    }

    #[test]
    fn wraps() {
        let mut r = ring(4);
        let mut out = Vec::new();
        r.latest(3, 1, &mut out);
        assert_eq!(out, [0.0, 0.0, 0.0]);
        r.push(&[1.0, 2.0], 1);
        r.latest(3, 1, &mut out);
        assert_eq!(out, [0.0, 1.0, 2.0]);
        r.push(&[3.0, 4.0, 5.0], 1);
        r.latest(4, 1, &mut out);
        assert_eq!(out, [2.0, 3.0, 4.0, 5.0]);
        r.latest(6, 1, &mut out);
        assert_eq!(out, [0.0, 0.0, 2.0, 3.0, 4.0, 5.0], "more than the ring holds");
        r.push(&[6.0, 7.0, 8.0, 9.0, 10.0, 11.0], 1);
        r.latest(2, 1, &mut out);
        assert_eq!(out, [10.0, 11.0]);
        assert_eq!(r.total, 11);
    }

    #[test]
    fn reads_by_frame_number() {
        let mut r = ring(4);
        let mut out = Vec::new();
        r.push(&[0.0, 1.0, 2.0], 1);
        assert_eq!(r.read(1, 2, 1, &mut out), Ok(()));
        assert_eq!(out, [1.0, 2.0]);
        assert_eq!(r.read(2, 2, 1, &mut out), Err(Gap::NotYet { total: 3 }));
        r.push(&[3.0, 4.0, 5.0], 1); // frames 0 and 1 overwritten
        assert_eq!(r.read(1, 2, 1, &mut out), Err(Gap::Gone { oldest: 2 }));
        assert_eq!(r.read(2, 4, 1, &mut out), Ok(()));
        assert_eq!(out, [2.0, 3.0, 4.0, 5.0], "across the wrap");
        assert_eq!(r.read(6, 0, 1, &mut out), Ok(()), "nothing, at the end");
        // Stereo: frames, not samples.
        let mut r = ring(6);
        r.push(&[1.0, -1.0, 2.0, -2.0, 3.0, -3.0, 4.0, -4.0], 2);
        assert_eq!(r.read(1, 3, 2, &mut out), Ok(()));
        assert_eq!(out, [2.0, -2.0, 3.0, -3.0, 4.0, -4.0]);
        assert_eq!(r.read(0, 1, 2, &mut out), Err(Gap::Gone { oldest: 1 }));
    }

    #[test]
    fn stereo() {
        let mut r = ring(6);
        r.push(&[1.0, -1.0, 2.0, -2.0], 2);
        let mut out = Vec::new();
        r.latest(2, 2, &mut out);
        assert_eq!(out, [1.0, -1.0, 2.0, -2.0]);
        assert_eq!(r.total, 2);
    }
}
