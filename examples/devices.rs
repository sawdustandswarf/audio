//! The server, its sources and its sinks: `cargo run --example devices`.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut c = audio::Client::connect("audio-devices")?;
    let info = c.server_info()?;
    println!("{} {} (protocol {}), {:?}", info.name, info.version, c.version(), info.spec);
    println!("default sink {:?}, source {:?}", info.default_sink, info.default_source);
    println!("sources:");
    for d in c.sources()? {
        let m = d.monitor.map(|m| format!(" (monitor of {m})")).unwrap_or_default();
        println!("  {:3} {}  {} Hz x{}  {}{m}", d.index, d.name, d.spec.rate, d.spec.channels, d.description);
    }
    println!("sinks:");
    for d in c.sinks()? {
        println!("  {:3} {}  {} Hz x{}  {}", d.index, d.name, d.spec.rate, d.spec.channels, d.description);
    }
    Ok(())
}
