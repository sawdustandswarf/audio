//! PulseAudio's tagstruct: the body of every command packet. Each value is a
//! one-byte tag and its bytes, big-endian, so a reader can check what it is
//! given and skip what it doesn't want.

use std::fmt;

const STRING: u8 = b't';
const NULL_STRING: u8 = b'N';
const U32: u8 = b'L';
const U8: u8 = b'B';
const U64: u8 = b'R';
const S64: u8 = b'r';
const SAMPLE_SPEC: u8 = b'a';
const ARBITRARY: u8 = b'x';
const TRUE: u8 = b'1';
const FALSE: u8 = b'0';
const TIMEVAL: u8 = b'T';
const USEC: u8 = b'U';
const CHANNEL_MAP: u8 = b'm';
const CVOLUME: u8 = b'v';
const PROPLIST: u8 = b'P';
const VOLUME: u8 = b'V';
const FORMAT_INFO: u8 = b'f';

/// A tagstruct that isn't what was expected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Malformed(pub String);

impl fmt::Display for Malformed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "malformed reply from the sound server: {}", self.0)
    }
}

impl std::error::Error for Malformed {}

type Result<T> = std::result::Result<T, Malformed>;

/// A sample spec as the protocol carries it: format code, channels, rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawSpec {
    pub format: u8,
    pub channels: u8,
    pub rate: u32,
}

/// Building a tagstruct.
#[derive(Debug, Default)]
pub struct Writer(pub Vec<u8>);

impl Writer {
    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.0.push(U32);
        self.0.extend(v.to_be_bytes());
        self
    }

    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.0.extend([U8, v]);
        self
    }

    pub fn bool(&mut self, v: bool) -> &mut Self {
        self.0.push(if v { TRUE } else { FALSE });
        self
    }

    /// A string, or the null string for `None`.
    pub fn string(&mut self, v: Option<&str>) -> &mut Self {
        match v {
            Some(s) => {
                self.0.push(STRING);
                // A NUL would end it early; none of ours have one.
                self.0.extend(s.bytes().filter(|&b| b != 0));
                self.0.push(0);
            }
            None => self.0.push(NULL_STRING),
        }
        self
    }

    pub fn arbitrary(&mut self, v: &[u8]) -> &mut Self {
        self.0.push(ARBITRARY);
        self.0.extend((v.len() as u32).to_be_bytes());
        self.0.extend(v);
        self
    }

    pub fn sample_spec(&mut self, s: RawSpec) -> &mut Self {
        self.0.extend([SAMPLE_SPEC, s.format, s.channels]);
        self.0.extend(s.rate.to_be_bytes());
        self
    }

    pub fn channel_map(&mut self, positions: &[u8]) -> &mut Self {
        self.0.extend([CHANNEL_MAP, positions.len() as u8]);
        self.0.extend(positions);
        self
    }

    pub fn cvolume(&mut self, volumes: &[u32]) -> &mut Self {
        self.0.extend([CVOLUME, volumes.len() as u8]);
        for v in volumes {
            self.0.extend(v.to_be_bytes());
        }
        self
    }

    /// A property list of UTF-8 values (stored NUL-terminated, as libpulse does).
    pub fn proplist(&mut self, props: &[(&str, &str)]) -> &mut Self {
        self.0.push(PROPLIST);
        for (k, v) in props {
            let mut value = v.as_bytes().to_vec();
            value.push(0);
            self.string(Some(k));
            self.u32(value.len() as u32);
            self.arbitrary(&value);
        }
        self.string(None)
    }
}

/// Reading a tagstruct, value by value.
#[derive(Debug)]
pub struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Reader<'a> {
        Reader { data, at: 0 }
    }

    pub fn is_empty(&self) -> bool {
        self.at >= self.data.len()
    }

    fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.data.len());
        let end = end.ok_or_else(|| Malformed("ends early".into()))?;
        let b = &self.data[self.at..end];
        self.at = end;
        Ok(b)
    }

    fn be32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.bytes(4)?.try_into().expect("four bytes")))
    }

    fn tag(&mut self, want: u8, what: &str) -> Result<()> {
        let t = self.bytes(1)?[0];
        if t == want { Ok(()) } else { Err(Malformed(format!("{what}: tag {:?}", t as char))) }
    }

    pub fn u32(&mut self) -> Result<u32> {
        self.tag(U32, "u32")?;
        self.be32()
    }

    pub fn u8(&mut self) -> Result<u8> {
        self.tag(U8, "u8")?;
        Ok(self.bytes(1)?[0])
    }

    #[cfg(test)]
    pub fn bool(&mut self) -> Result<bool> {
        match self.bytes(1)?[0] {
            TRUE => Ok(true),
            FALSE => Ok(false),
            t => Err(Malformed(format!("bool: tag {:?}", t as char))),
        }
    }

    /// A string; `None` for the null string.
    pub fn string(&mut self) -> Result<Option<String>> {
        match self.bytes(1)?[0] {
            NULL_STRING => Ok(None),
            STRING => {
                let rest = &self.data[self.at..];
                let len = rest.iter().position(|&b| b == 0).ok_or_else(|| Malformed("string without NUL".into()))?;
                let s = String::from_utf8_lossy(&rest[..len]).into_owned();
                self.at += len + 1;
                Ok(Some(s))
            }
            t => Err(Malformed(format!("string: tag {:?}", t as char))),
        }
    }

    pub fn sample_spec(&mut self) -> Result<RawSpec> {
        self.tag(SAMPLE_SPEC, "sample spec")?;
        let b = self.bytes(2)?;
        let (format, channels) = (b[0], b[1]);
        Ok(RawSpec { format, channels, rate: self.be32()? })
    }

    #[cfg(test)]
    pub fn channel_map(&mut self) -> Result<Vec<u8>> {
        self.tag(CHANNEL_MAP, "channel map")?;
        let n = self.bytes(1)?[0] as usize;
        Ok(self.bytes(n)?.to_vec())
    }

    /// A property list, as (key, value) with values that are text.
    pub fn proplist(&mut self) -> Result<Vec<(String, String)>> {
        self.tag(PROPLIST, "proplist")?;
        let mut out = Vec::new();
        while let Some(key) = self.string()? {
            let len = self.u32()? as usize;
            self.tag(ARBITRARY, "proplist value")?;
            let n = self.be32()? as usize;
            if n != len {
                return Err(Malformed("proplist value length".into()));
            }
            let v = self.bytes(n)?;
            let v = v.strip_suffix(&[0]).unwrap_or(v);
            out.push((key, String::from_utf8_lossy(v).into_owned()));
        }
        Ok(out)
    }

    /// Skip one value, whatever it is.
    pub fn skip(&mut self) -> Result<()> {
        let t = self.bytes(1)?[0];
        match t {
            STRING => {
                self.at -= 1;
                self.string()?;
            }
            NULL_STRING | TRUE | FALSE => {}
            U32 | VOLUME => {
                self.bytes(4)?;
            }
            U8 => {
                self.bytes(1)?;
            }
            U64 | S64 | USEC | TIMEVAL => {
                self.bytes(8)?;
            }
            SAMPLE_SPEC => {
                self.bytes(6)?;
            }
            ARBITRARY => {
                let n = self.be32()? as usize;
                self.bytes(n)?;
            }
            CHANNEL_MAP => {
                let n = self.bytes(1)?[0] as usize;
                self.bytes(n)?;
            }
            CVOLUME => {
                let n = self.bytes(1)?[0] as usize;
                self.bytes(n * 4)?;
            }
            PROPLIST => {
                self.at -= 1;
                self.proplist()?;
            }
            FORMAT_INFO => {
                self.u8()?;
                self.proplist()?;
            }
            t => return Err(Malformed(format!("unknown tag {:?}", t as char))),
        }
        Ok(())
    }

    pub fn skip_n(&mut self, n: usize) -> Result<()> {
        (0..n).try_for_each(|_| self.skip())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut w = Writer::default();
        let spec = RawSpec { format: 5, channels: 2, rate: 48000 };
        w.u32(7).u8(3).bool(true).string(Some("hi")).string(None).sample_spec(spec);
        w.channel_map(&[1, 2]).cvolume(&[0x10000, 0x10000]).proplist(&[("application.name", "myapp")]);
        w.arbitrary(&[1, 2, 3]).u32(9);
        let mut r = Reader::new(&w.0);
        assert_eq!(r.u32(), Ok(7));
        assert_eq!(r.u8(), Ok(3));
        assert_eq!(r.bool(), Ok(true));
        assert_eq!(r.string(), Ok(Some("hi".into())));
        assert_eq!(r.string(), Ok(None));
        assert_eq!(r.sample_spec(), Ok(spec));
        assert_eq!(r.channel_map(), Ok(vec![1, 2]));
        r.skip().unwrap(); // cvolume
        assert_eq!(r.proplist(), Ok(vec![("application.name".into(), "myapp".into())]));
        r.skip().unwrap(); // arbitrary
        assert_eq!(r.u32(), Ok(9));
        assert!(r.is_empty());
        assert!(r.u32().is_err(), "past the end");
    }

    #[test]
    fn wrong_tag() {
        let mut w = Writer::default();
        w.u8(1);
        assert!(Reader::new(&w.0).u32().is_err());
    }
}
