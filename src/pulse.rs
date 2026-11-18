//! A client of PulseAudio's native protocol, which PipeWire serves too
//! (`pipewire-pulse`): a Unix socket of packets, each a 20-byte descriptor
//! and a body. Commands and replies are tagstructs on the control channel;
//! sound goes on a stream's own channel, in the packet itself (no shared
//! memory is asked for, so none is used).
//!
//! One connection, one stream: [`Client::record`] and [`Client::playback`]
//! take the client, so a stream's packets are the only ones that come.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::{env, fmt, fs};

use crate::Spec;
use crate::tagstruct::{Malformed, RawSpec, Reader, Writer};

/// The protocol version spoken; the server's, if older, is used.
pub const PROTOCOL_VERSION: u32 = 32;
/// The oldest server spoken to: property lists came in 13.
const OLDEST: u32 = 13;

const CONTROL: u32 = u32::MAX;
const INVALID_INDEX: u32 = u32::MAX;
const VOLUME_NORM: u32 = 0x10000;
const FLOAT32LE: u8 = 5;
/// A packet bigger than this is a broken stream, not a packet.
const MAX_PACKET: usize = 16 << 20;
/// Data packets sent at most this big.
const MAX_WRITE: usize = 64 << 10;

mod command {
    pub const ERROR: u32 = 0;
    pub const REPLY: u32 = 2;
    pub const CREATE_PLAYBACK_STREAM: u32 = 3;
    pub const CREATE_RECORD_STREAM: u32 = 5;
    pub const AUTH: u32 = 8;
    pub const SET_CLIENT_NAME: u32 = 9;
    pub const DRAIN_PLAYBACK_STREAM: u32 = 12;
    pub const GET_SERVER_INFO: u32 = 20;
    pub const GET_SINK_INFO_LIST: u32 = 22;
    pub const GET_SOURCE_INFO_LIST: u32 = 24;
    pub const REQUEST: u32 = 61;
    pub const PLAYBACK_STREAM_KILLED: u32 = 64;
    pub const RECORD_STREAM_KILLED: u32 = 65;
}

/// What went wrong talking to the sound server.
#[derive(Debug)]
pub enum Error {
    /// No server to connect to, and why.
    NoServer(String),
    Io(io::Error),
    /// The server refused a command: which, and its error code.
    Refused {
        command: &'static str,
        code: u32,
    },
    Malformed(Malformed),
    /// The server ended the stream (its device went away, say).
    Killed,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NoServer(why) => write!(f, "no sound server: {why}"),
            Error::Io(e) => write!(f, "sound server: {e}"),
            Error::Refused { command, code } => write!(f, "sound server refused {command}: {}", error_text(*code)),
            Error::Malformed(m) => m.fmt(f),
            Error::Killed => write!(f, "the sound server ended the stream"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Error {
        Error::Io(e)
    }
}

impl From<Malformed> for Error {
    fn from(m: Malformed) -> Error {
        Error::Malformed(m)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// PulseAudio's error codes, as `pa_strerror` words them.
fn error_text(code: u32) -> String {
    let text = match code {
        1 => "access denied",
        2 => "unknown command",
        3 => "invalid argument",
        4 => "entity exists",
        5 => "no such entity",
        6 => "connection refused",
        7 => "protocol error",
        8 => "timeout",
        9 => "no authentication key",
        10 => "internal error",
        11 => "connection terminated",
        12 => "entity killed",
        13 => "invalid server",
        15 => "bad state",
        16 => "no data",
        17 => "incompatible protocol version",
        18 => "too large",
        19 => "not supported",
        _ => return format!("error {code}"),
    };
    text.to_string()
}

/// The server, as `GET_SERVER_INFO` describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInfo {
    /// "PulseAudio (on PipeWire 1.6.9)", say.
    pub name: String,
    pub version: String,
    pub spec: Spec,
    pub default_sink: Option<String>,
    pub default_source: Option<String>,
}

/// A source (something to record) or a sink (something to play to).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub index: u32,
    /// What to pass to [`Client::record`] or [`Client::playback`].
    pub name: String,
    pub description: String,
    pub spec: Spec,
    /// A source: the sink it is the monitor of, if it is one (what that
    /// sink plays). A sink: its monitor source.
    pub monitor: Option<String>,
}

#[derive(Debug)]
struct Packet {
    channel: u32,
    data: Vec<u8>,
}

/// A connection to the sound server, authorised and named.
#[derive(Debug)]
pub struct Client {
    sock: UnixStream,
    /// The protocol version agreed.
    version: u32,
    next_tag: u32,
    /// Packets that came while a reply was awaited, in order.
    held: VecDeque<Packet>,
}

impl Client {
    /// Connect to the server (`$PULSE_SERVER`, else
    /// `$XDG_RUNTIME_DIR/pulse/native`) as the application `name`.
    pub fn connect(name: &str) -> Result<Client> {
        let path = server_path()?;
        let sock = UnixStream::connect(&path).map_err(|e| Error::NoServer(format!("{}: {e}", path.display())))?;
        let mut c = Client { sock, version: PROTOCOL_VERSION, next_tag: 0, held: VecDeque::new() };
        let mut w = c.command(command::AUTH);
        w.u32(PROTOCOL_VERSION).arbitrary(&cookie());
        let reply = c.request("AUTH", w)?;
        // The high bits are shared-memory flags, not asked for.
        let server = Reader::new(&reply).u32()? & 0xffff;
        if server < OLDEST {
            return Err(Error::NoServer(format!("protocol version {server}, older than {OLDEST}")));
        }
        c.version = server.min(PROTOCOL_VERSION);
        let mut w = c.command(command::SET_CLIENT_NAME);
        w.proplist(&[("application.name", name), ("application.process.binary", name)]);
        c.request("SET_CLIENT_NAME", w)?;
        Ok(c)
    }

    /// The protocol version agreed with the server.
    pub fn version(&self) -> u32 {
        self.version
    }

    pub fn server_info(&mut self) -> Result<ServerInfo> {
        let w = self.command(command::GET_SERVER_INFO);
        let reply = self.request("GET_SERVER_INFO", w)?;
        let mut r = Reader::new(&reply);
        let name = r.string()?.unwrap_or_default();
        let version = r.string()?.unwrap_or_default();
        r.skip_n(2)?; // user and host names
        let spec = spec(r.sample_spec()?);
        let default_sink = r.string()?;
        let default_source = r.string()?;
        Ok(ServerInfo { name, version, spec, default_sink, default_source })
    }

    /// The sources there are to record from, monitors of sinks included.
    pub fn sources(&mut self) -> Result<Vec<Device>> {
        self.devices(command::GET_SOURCE_INFO_LIST, "GET_SOURCE_INFO_LIST", 22)
    }

    /// The sinks there are to play to.
    pub fn sinks(&mut self) -> Result<Vec<Device>> {
        self.devices(command::GET_SINK_INFO_LIST, "GET_SINK_INFO_LIST", 21)
    }

    /// Sources and sinks are told alike, but for the version their formats
    /// came in (`formats_from`).
    fn devices(&mut self, cmd: u32, what: &'static str, formats_from: u32) -> Result<Vec<Device>> {
        let w = self.command(cmd);
        let reply = self.request(what, w)?;
        let mut r = Reader::new(&reply);
        let mut out = Vec::new();
        while !r.is_empty() {
            let index = r.u32()?;
            let name = r.string()?.unwrap_or_default();
            let description = r.string()?.unwrap_or_default();
            let spec = spec(r.sample_spec()?);
            r.skip_n(4)?; // channel map, module, volume, mute
            r.skip()?; // monitor's (or monitored sink's) index
            let monitor = r.string()?;
            r.skip_n(3)?; // latency, driver, flags
            r.skip_n(2)?; // v13: proplist, configured latency
            if self.version >= 15 {
                r.skip_n(4)?; // base volume, state, volume steps, card
            }
            if self.version >= 16 {
                let ports = r.u32()?;
                let per_port = if self.version >= 24 { 4 } else { 3 } + if self.version >= 34 { 2 } else { 0 };
                r.skip_n(ports as usize * per_port)?;
                r.skip()?; // active port
            }
            if self.version >= formats_from {
                let n = r.u8()?;
                r.skip_n(n as usize)?;
            }
            out.push(Device { index, name, description, spec, monitor });
        }
        Ok(out)
    }

    /// Record from `device` (a source's name; `None` for the default
    /// source; `"@DEFAULT_MONITOR@"` for what the default sink plays) as
    /// float samples in `spec`, converted by the server. `fragment` frames
    /// are sent at a time, which sets the latency.
    pub fn record(mut self, device: Option<&str>, spec: Spec, fragment: u32) -> Result<Record> {
        let raw = raw_spec(spec)?;
        let frame = spec.channels as u32 * 4;
        let v = self.version;
        let mut w = self.command(command::CREATE_RECORD_STREAM);
        w.sample_spec(raw).channel_map(&channel_map(spec.channels)).u32(INVALID_INDEX).string(device);
        w.u32(u32::MAX) // maxlength: the server's
            .bool(false) // corked
            .u32(fragment.max(1).saturating_mul(frame));
        // No remap, no remix, fix format, rate, channels, don't move, variable rate.
        for _ in 0..7 {
            w.bool(false);
        }
        w.bool(false) // peak detect
            .bool(true) // adjust latency: the fragment is what we want
            .proplist(&[("media.name", "capture")])
            .u32(INVALID_INDEX); // direct on input
        if v >= 14 {
            w.bool(false); // early requests
        }
        if v >= 15 {
            w.bool(false).bool(false); // don't inhibit auto suspend, fail on suspend
        }
        if v >= 22 {
            w.u8(0) // formats: none, the sample spec says
                .cvolume(&vec![VOLUME_NORM; spec.channels as usize])
                .bool(false) // muted
                .bool(false) // volume set
                .bool(false) // muted set
                .bool(false) // relative volume
                .bool(false); // passthrough
        }
        let reply = self.request("CREATE_RECORD_STREAM", w)?;
        let mut r = Reader::new(&reply);
        let channel = r.u32()?;
        r.skip_n(3)?; // stream index, maxlength, fragsize
        let got = r.sample_spec()?;
        r.skip_n(2)?; // channel map, device index
        let device = r.string()?.unwrap_or_default();
        if got.format != FLOAT32LE || self::spec(got) != spec {
            return Err(Error::Malformed(Malformed(format!("asked for {spec:?} as float, got {got:?}"))));
        }
        Ok(Record { client: self, channel, spec, device, partial: Vec::new() })
    }

    /// Play to `device` (a sink's name; `None` for the default sink) float
    /// samples in `spec`, converted by the server.
    pub fn playback(mut self, device: Option<&str>, spec: Spec) -> Result<Playback> {
        let raw = raw_spec(spec)?;
        let v = self.version;
        let mut w = self.command(command::CREATE_PLAYBACK_STREAM);
        w.sample_spec(raw).channel_map(&channel_map(spec.channels)).u32(INVALID_INDEX).string(device);
        w.u32(u32::MAX).bool(false); // maxlength: the server's; not corked
        // Target length, prebuffer, minimum request: the server's; sync group 0.
        w.u32(u32::MAX).u32(u32::MAX).u32(u32::MAX).u32(0);
        w.cvolume(&vec![VOLUME_NORM; spec.channels as usize]);
        for _ in 0..7 {
            w.bool(false);
        }
        w.bool(false).bool(false).proplist(&[("media.name", "playback")]); // start muted, adjust latency
        if v >= 14 {
            w.bool(false).bool(false); // volume set, early requests
        }
        if v >= 15 {
            w.bool(false).bool(false).bool(false); // muted set, don't inhibit suspend, fail on suspend
        }
        if v >= 17 {
            w.bool(false); // relative volume
        }
        if v >= 18 {
            w.bool(false); // passthrough
        }
        if v >= 21 {
            w.u8(0);
        }
        let reply = self.request("CREATE_PLAYBACK_STREAM", w)?;
        let mut r = Reader::new(&reply);
        let channel = r.u32()?;
        r.skip()?; // stream index
        let requested = r.u32()? as usize;
        Ok(Playback { client: self, channel, spec, requested })
    }

    /// A command's header: its number and a fresh tag.
    fn command(&mut self, cmd: u32) -> Writer {
        let mut w = Writer::default();
        self.next_tag = self.next_tag.wrapping_add(1) & 0x7fff_ffff;
        w.u32(cmd).u32(self.next_tag);
        w
    }

    /// Send a command and wait for its reply's body.
    fn request(&mut self, what: &'static str, w: Writer) -> Result<Vec<u8>> {
        let tag = self.next_tag;
        self.send(CONTROL, &w.0)?;
        loop {
            let p = self.read_packet()?;
            if p.channel == CONTROL {
                let mut r = Reader::new(&p.data);
                let (cmd, t) = (r.u32()?, r.u32()?);
                if t == tag && cmd == command::REPLY {
                    return Ok(p.data[10..].to_vec());
                }
                if t == tag && cmd == command::ERROR {
                    return Err(Error::Refused { command: what, code: r.u32()? });
                }
            }
            self.held.push_back(p);
        }
    }

    fn send(&mut self, channel: u32, body: &[u8]) -> Result<()> {
        let mut packet = Vec::with_capacity(20 + body.len());
        // Length, channel, offset (high, low), flags (seek relative).
        for v in [body.len() as u32, channel, 0, 0, 0] {
            packet.extend(v.to_be_bytes());
        }
        packet.extend(body);
        self.sock.write_all(&packet)?;
        Ok(())
    }

    fn read_packet(&mut self) -> Result<Packet> {
        let mut d = [0u8; 20];
        self.sock.read_exact(&mut d)?;
        let word = |i: usize| u32::from_be_bytes(d[i * 4..i * 4 + 4].try_into().expect("four bytes"));
        let (len, channel, flags) = (word(0) as usize, word(1), word(4));
        if len > MAX_PACKET {
            return Err(Error::Malformed(Malformed(format!("a packet of {len} bytes"))));
        }
        if flags & 0xff00_0000 != 0 {
            return Err(Error::Malformed(Malformed("shared memory, not asked for".into())));
        }
        let mut data = vec![0; len];
        self.sock.read_exact(&mut data)?;
        Ok(Packet { channel, data })
    }

    /// The next packet: one held back, else one from the socket.
    fn next_packet(&mut self) -> Result<Packet> {
        match self.held.pop_front() {
            Some(p) => Ok(p),
            None => self.read_packet(),
        }
    }
}

/// A command the server sent unasked: its number and the body after the tag.
fn event(p: &Packet) -> Option<(u32, Reader<'_>)> {
    let mut r = Reader::new(&p.data);
    let cmd = r.u32().ok()?;
    r.u32().ok()?;
    Some((cmd, r))
}

/// A stream of sound from a source.
#[derive(Debug)]
pub struct Record {
    client: Client,
    channel: u32,
    spec: Spec,
    device: String,
    /// The bytes of a sample split between packets.
    partial: Vec<u8>,
}

impl Record {
    pub fn spec(&self) -> Spec {
        self.spec
    }

    /// The source's name, as the server resolved it.
    pub fn device(&self) -> &str {
        &self.device
    }

    /// Wait for the next sound and append it to `out`, interleaved; the
    /// number of samples appended.
    pub fn read(&mut self, out: &mut Vec<f32>) -> Result<usize> {
        loop {
            let p = self.client.next_packet()?;
            if p.channel == self.channel {
                self.partial.extend(&p.data);
                let whole = self.partial.len() / 4 * 4;
                let before = out.len();
                out.extend(self.partial[..whole].as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)));
                self.partial.drain(..whole);
                return Ok(out.len() - before);
            }
            if p.channel == CONTROL
                && let Some((command::RECORD_STREAM_KILLED, mut r)) = event(&p)
                && r.u32().ok() == Some(self.channel)
            {
                return Err(Error::Killed);
            }
        }
    }

    /// A handle on the socket, to shut it down from another thread (which
    /// ends a `read` waiting there).
    pub fn socket(&self) -> io::Result<UnixStream> {
        self.client.sock.try_clone()
    }
}

/// A stream of sound to a sink.
#[derive(Debug)]
pub struct Playback {
    client: Client,
    channel: u32,
    spec: Spec,
    /// Bytes the server has asked for and not had.
    requested: usize,
}

impl Playback {
    pub fn spec(&self) -> Spec {
        self.spec
    }

    /// Play `samples`, interleaved, waiting while the server's buffer is full.
    pub fn write(&mut self, samples: &[f32]) -> Result<()> {
        let frame = self.spec.channels as usize * 4;
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut at = 0;
        while at < bytes.len() {
            while self.requested < frame {
                self.wait_request()?;
            }
            let n = (bytes.len() - at).min(self.requested).min(MAX_WRITE) / frame * frame;
            let n = n.max(frame).min(bytes.len() - at);
            self.client.send(self.channel, &bytes[at..at + n])?;
            self.requested = self.requested.saturating_sub(n);
            at += n;
        }
        Ok(())
    }

    /// Wait until everything written has been played.
    pub fn drain(&mut self) -> Result<()> {
        let mut w = self.client.command(command::DRAIN_PLAYBACK_STREAM);
        w.u32(self.channel);
        self.client.request("DRAIN_PLAYBACK_STREAM", w)?;
        Ok(())
    }

    fn wait_request(&mut self) -> Result<()> {
        let p = self.client.next_packet()?;
        if p.channel != CONTROL {
            return Ok(());
        }
        let Some((cmd, mut r)) = event(&p) else { return Ok(()) };
        if r.u32().ok() != Some(self.channel) {
            return Ok(());
        }
        match cmd {
            command::REQUEST => self.requested += r.u32()? as usize,
            command::PLAYBACK_STREAM_KILLED => return Err(Error::Killed),
            _ => {}
        }
        Ok(())
    }
}

fn raw_spec(s: Spec) -> Result<RawSpec> {
    if s.channels == 0 || s.channels > 32 || s.rate == 0 || s.rate > 384_000 {
        return Err(Error::Malformed(Malformed(format!("no such sample spec: {s:?}"))));
    }
    Ok(RawSpec { format: FLOAT32LE, channels: s.channels as u8, rate: s.rate })
}

fn spec(s: RawSpec) -> Spec {
    Spec { rate: s.rate, channels: s.channels as u16 }
}

/// Mono, stereo (front left, right), else the auxiliary channels.
fn channel_map(channels: u16) -> Vec<u8> {
    match channels {
        1 => vec![0],
        2 => vec![1, 2],
        n => (0..n as u8).map(|i| 12 + i).collect(),
    }
}

/// Where the server listens.
fn server_path() -> Result<PathBuf> {
    if let Ok(s) = env::var("PULSE_SERVER") {
        // A list of addresses; ours are local sockets.
        for a in s.split_whitespace() {
            let a = a.strip_prefix("unix:").unwrap_or(a);
            if a.starts_with('/') {
                return Ok(PathBuf::from(a));
            }
        }
        return Err(Error::NoServer(format!("PULSE_SERVER={s}: only local sockets (unix:/path)")));
    }
    let dir = env::var_os("XDG_RUNTIME_DIR").ok_or_else(|| Error::NoServer("XDG_RUNTIME_DIR is not set".into()))?;
    Ok(PathBuf::from(dir).join("pulse/native"))
}

/// The authentication cookie, or zeros where there is none (PipeWire, and
/// PulseAudio for its own user, don't need it).
fn cookie() -> Vec<u8> {
    let mut paths = Vec::new();
    if let Some(p) = env::var_os("PULSE_COOKIE") {
        paths.push(PathBuf::from(p));
    }
    let config = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    if let Some(c) = config {
        paths.push(c.join("pulse/cookie"));
    }
    if let Some(h) = env::var_os("HOME") {
        paths.push(PathBuf::from(h).join(".pulse-cookie"));
    }
    paths.iter().find_map(|p| fs::read(p).ok().filter(|c| c.len() >= 256)).map_or(vec![0; 256], |c| c[..256].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_and_specs() {
        assert_eq!(channel_map(1), [0]);
        assert_eq!(channel_map(2), [1, 2]);
        assert_eq!(channel_map(3), [12, 13, 14]);
        assert!(raw_spec(Spec { rate: 48000, channels: 0 }).is_err());
        let raw = raw_spec(Spec { rate: 44100, channels: 2 }).unwrap();
        assert_eq!(raw, RawSpec { format: FLOAT32LE, channels: 2, rate: 44100 });
        assert_eq!(spec(raw), Spec { rate: 44100, channels: 2 });
    }

    #[test]
    fn errors_read() {
        assert_eq!(error_text(5), "no such entity");
        assert_eq!(error_text(99), "error 99");
    }
}
