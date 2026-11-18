//! Play a sine: `cargo run --example tone [HZ] [SECONDS] [LEVEL]` (1 kHz, 2 s, 0.25).

use audio::{Client, Spec};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let hz: f32 = args.next().map_or(Ok(1000.0), |a| a.parse())?;
    let secs: f32 = args.next().map_or(Ok(2.0), |a| a.parse())?;
    let level: f32 = args.next().map_or(Ok(0.25), |a| a.parse())?;
    let spec = Spec { rate: 48000, channels: 1 };
    let mut play = Client::connect("audio-tone")?.playback(None, spec)?;
    let n = (secs * spec.rate as f32) as usize;
    // The phase in f64: in f32 it drifts as `i` grows, and the tone grows spurs.
    let w = std::f64::consts::TAU * hz as f64 / spec.rate as f64;
    let samples: Vec<f32> = (0..n).map(|i| level * (w * i as f64).sin() as f32).collect();
    play.write(&samples)?;
    play.drain()?;
    Ok(())
}
