# AGENTS.md

Guidance for AI coding assistants (and people) working in this repository.

## What this is

`audio` is a small Rust library for sound in and out on Linux. It speaks PulseAudio's native protocol over its Unix socket, which PipeWire also serves, and it reads and writes WAV files. It uses std only. `README.md` has the API table and an explanation of the protocol.

## Commands

```sh
cargo build
cargo test
cargo clippy --all-targets
cargo fmt
cargo run --example devices            # needs a running PulseAudio or pipewire-pulse
cargo run --example meter [SOURCE]
cargo run --example tone [HZ] [S] [L]
```

## Layout

| File | What |
|---|---|
| `src/lib.rs` | public API, re-exports, crate docs |
| `src/pulse.rs` | `Client`, the connection, `Record` and `Playback` streams |
| `src/tagstruct.rs` | the protocol's typed values, encoding and decoding |
| `src/tap.rs` | `Tap`: a recording read in a thread into a ring buffer |
| `src/wav.rs` | WAV reading and writing |
| `examples/` | `devices`, `meter`, `tone` |

## Docs

`README.md` is the overview and the public API; `TODO.md` is the smaller open work, as a checklist.

## Rules

- **No third-party crates:** std only.
- **Samples are `f32` on this side.** The server converts formats, rates and channel counts.
- **Tests must not need a sound server.** Protocol code is tested on bytes (see `tagstruct.rs`). Only the examples talk to a live server.
- **Public API changes** update the table in `README.md`. Other programs depend on this crate from git.
- **Commits:** only when asked.
