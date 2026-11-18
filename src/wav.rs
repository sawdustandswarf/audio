//! WAV files: read as float samples (8, 16, 24 and 32-bit integer PCM, 32
//! and 64-bit float, plain or `WAVE_FORMAT_EXTENSIBLE`), written as 32-bit
//! float.

use std::fmt;

use crate::Spec;

/// A WAV file's sound: interleaved samples in -1..1.
#[derive(Debug, Clone, PartialEq)]
pub struct Wav {
    pub spec: Spec,
    pub samples: Vec<f32>,
}

/// Not a WAV file this reads, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WAV: {}", self.0)
    }
}

impl std::error::Error for Error {}

const PCM: u16 = 1;
const FLOAT: u16 = 3;
const EXTENSIBLE: u16 = 0xfffe;

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("four bytes"))
}

impl Wav {
    pub fn parse(data: &[u8]) -> Result<Wav, Error> {
        if data.len() < 12 || &data[..4] != b"RIFF" || &data[8..12] != b"WAVE" {
            return Err(Error("not a RIFF WAVE file".into()));
        }
        let mut fmt = None;
        let mut at = 12;
        while at + 8 <= data.len() {
            let id = &data[at..at + 4];
            let len = le32(data, at + 4) as usize;
            let body = &data[at + 8..(at + 8).saturating_add(len).min(data.len())];
            match id {
                b"fmt " if body.len() >= 16 => fmt = Some(body),
                b"data" => {
                    let fmt = fmt.ok_or_else(|| Error("data before fmt".into()))?;
                    return decode(fmt, body);
                }
                _ => {}
            }
            // Chunks are padded to even lengths.
            at = at.saturating_add(8).saturating_add(len).saturating_add(len & 1);
        }
        Err(Error("no data".into()))
    }

    /// As a WAV file of 32-bit floats.
    pub fn encode(&self) -> Vec<u8> {
        let channels = self.spec.channels;
        let data_len = self.samples.len() as u32 * 4;
        let mut out = Vec::with_capacity(44 + data_len as usize);
        out.extend(b"RIFF");
        out.extend((36 + data_len).to_le_bytes());
        out.extend(b"WAVEfmt ");
        out.extend(16u32.to_le_bytes());
        out.extend(FLOAT.to_le_bytes());
        out.extend(channels.to_le_bytes());
        out.extend(self.spec.rate.to_le_bytes());
        out.extend((self.spec.rate * channels as u32 * 4).to_le_bytes());
        out.extend((channels * 4).to_le_bytes());
        out.extend(32u16.to_le_bytes());
        out.extend(b"data");
        out.extend(data_len.to_le_bytes());
        out.extend(self.samples.iter().flat_map(|s| s.to_le_bytes()));
        out
    }
}

fn decode(fmt: &[u8], data: &[u8]) -> Result<Wav, Error> {
    let mut format = le16(fmt, 0);
    let channels = le16(fmt, 2);
    let rate = le32(fmt, 4);
    let bits = le16(fmt, 14);
    if format == EXTENSIBLE {
        // The sub-format GUID starts with the format code.
        if fmt.len() < 26 {
            return Err(Error("short extensible fmt".into()));
        }
        format = le16(fmt, 24);
    }
    if channels == 0 || rate == 0 {
        return Err(Error("no channels, or no rate".into()));
    }
    let samples: Vec<f32> = match (format, bits) {
        (PCM, 8) => data.iter().map(|&b| (b as f32 - 128.0) / 128.0).collect(),
        (PCM, 16) => data.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b) as f32 / 32768.0).collect(),
        (PCM, 24) => data
            .as_chunks::<3>()
            .0
            .iter()
            .map(|b| i32::from_le_bytes([0, b[0], b[1], b[2]]) as f32 / 2_147_483_648.0)
            .collect(),
        (PCM, 32) => data.as_chunks::<4>().0.iter().map(|b| i32::from_le_bytes(*b) as f32 / 2_147_483_648.0).collect(),
        (FLOAT, 32) => data.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect(),
        (FLOAT, 64) => data.as_chunks::<8>().0.iter().map(|b| f64::from_le_bytes(*b) as f32).collect(),
        _ => return Err(Error(format!("format {format} at {bits} bits"))),
    };
    let mut samples = samples;
    samples.truncate(samples.len() / channels as usize * channels as usize);
    Ok(Wav { spec: Spec { rate, channels }, samples })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let w = Wav { spec: Spec { rate: 8000, channels: 2 }, samples: vec![0.0, 0.5, -0.5, 1.0] };
        assert_eq!(Wav::parse(&w.encode()), Ok(w));
    }

    #[test]
    fn sixteen_bit() {
        let mut f = b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0".to_vec();
        f.extend(1u16.to_le_bytes()); // PCM
        f.extend(1u16.to_le_bytes()); // mono
        f.extend(44100u32.to_le_bytes());
        f.extend(88200u32.to_le_bytes());
        f.extend(2u16.to_le_bytes());
        f.extend(16u16.to_le_bytes());
        f.extend(b"LIST\x03\0\0\0abc\0"); // odd, so padded
        f.extend(b"data\x04\0\0\0");
        f.extend(16384i16.to_le_bytes());
        f.extend((-32768i16).to_le_bytes());
        let w = Wav::parse(&f).unwrap();
        assert_eq!(w.spec, Spec { rate: 44100, channels: 1 });
        assert_eq!(w.samples, [0.5, -1.0]);
        assert!(Wav::parse(b"RIFF").is_err());
    }
}
