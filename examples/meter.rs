//! A level meter on the terminal: `cargo run --example meter [SOURCE]`
//! (default: what the default sink plays, `@DEFAULT_MONITOR@`).

use audio::{Client, Spec};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let source = std::env::args().nth(1).unwrap_or(audio::DEFAULT_MONITOR.into());
    let spec = Spec { rate: 48000, channels: 2 };
    let mut rec = Client::connect("audio-meter")?.record(Some(&source), spec, 2400)?;
    eprintln!("recording {}", rec.device());
    let mut buf = Vec::new();
    loop {
        buf.clear();
        rec.read(&mut buf)?;
        let peak = buf.iter().fold(0f32, |m, s| m.max(s.abs()));
        let db = 20.0 * peak.max(1e-6).log10();
        let bar = ((db + 60.0).max(0.0) as usize).min(60);
        eprint!("\r{db:6.1} dB |{:<60}|", "#".repeat(bar));
    }
}
