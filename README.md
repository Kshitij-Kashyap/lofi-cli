# lofi — terminal lo-fi / ambient radio

A small, native Rust terminal app for listening to publicly available
lo-fi / ambient internet radio streams. Keyboard-driven, single executable.

> V1 scope: internet-radio playback only. No local files, downloads,
> accounts, Spotify/YouTube, recording, visualizers, or EQ (see plan §1).

## Stations (verified direct streams)

These endpoints were verified with `curl -I` (HTTP 200, `audio/mpeg`,
public Icecast/Shoutcast). We link to the streams; we do not host content.

| Station | Source | URL |
|---|---|---|
| Groove Salad | SomaFM, ambient beats | `https://ice1.somafm.com/groovesalad-128-mp3` |
| Deep Space One | SomaFM, deep ambient | `https://ice1.somafm.com/deepspaceone-128-mp3` |
| Drone Zone | SomaFM, atmospheric ambient | `https://ice1.somafm.com/dronezone-128-mp3` |
| Lush | SomaFM, vocal electronica | `https://ice1.somafm.com/lush-128-mp3` |
| Radio Paradise | Radio Paradise, eclectic (128k mp3) | `https://stream.radioparadise.com/mp3-128` |

Note: "Lofi Girl" has **no official direct audio stream** (YouTube only),
so it is intentionally not listed. YouTube-backed playback is out of scope
for V1 (plan §7).

## Quick start

```bash
git clone https://github.com/Kshitij-Kashyap/lofi-cli.git
cd lofi-cli
cargo run --release
```

List available stations or start playback from the terminal:

```bash
cargo run --release -- list
cargo run --release -- play "Groove Salad"
```

## Build and install

```bash
cargo build --release
# binary: ./target/release/lofi
```

To install the binary in Cargo's user bin directory:

```bash
cargo install --path .
```

Real audio output needs the `audio` feature (rodio/cpal):

```bash
# Arch
pacman -S alsa-lib
# Debian/Ubuntu
# apt install libasound2-dev

cargo build --release --features audio
```

Without `--features audio` the binary builds anywhere (CI/headless) with a
null audio sink: streaming, decoding, metadata, visualizer levels, and
reconnect logic all work; only sound output is discarded.

No audio device (WSL, headless servers): the app probes for an output
device once at startup, before the TUI opens, and shows
`No audio device — showing stream info only.` Everything else —
streaming, track metadata, visualizer, timer — keeps working.

## Use

```bash
lofi                  # open TUI
lofi list             # list stations
lofi play "Groove Salad"   # headless playback until Ctrl-C
lofi --help
lofi --version
```

Keys:

```text
↑ / ↓ (or k/j)  select station
Enter           play selected station
Space           pause / resume
← / →           volume -5% / +5%
M               mute / unmute
R               reconnect current stream
V               cycle visualizer: bars → wave → off
C               cycle color theme: neon → ember → forest → mono
Q (or Esc)      quit
```

The header shows a session timer (`Session 04:37`, or `1:02:03` past an
hour) tracking how long the TUI has been running.

The visualizer panel renders a live spectrum (32 log-spaced bands,
60Hz–16kHz with AGC + smoothing) or a waveform oscilloscope, computed
from the actually-decoded stream PCM via an in-house FFT — no extra
dependencies. It works even without a sound card, since decoding always
runs; only audible output needs a device.

## Config

```toml
# Linux:   ~/.config/lofi/config.toml
# macOS:   ~/Library/Application Support/lofi/config.toml
# Windows: %APPDATA%/lofi/config.toml
volume = 0.7
default_station = "Groove Salad"

[[stations]]
name = "My Station"
description = "Custom radio"
url = "https://example.com/stream"
```

Built-ins work with no config file. Volume is saved on quit.

## Architecture

Event-driven: keyboard → `PlayerCommand` → `App` state + `AudioWorker`
(networking/decoding/output off the TUI thread). The TUI never touches
audio directly. Pipeline: HTTPS → bytes → ICY demux → decoder → output.
Reconnect backoff: 2s, 5s, 10s, 20s, 30s, then `Unable to reconnect. Press R.`

## Audio proof-of-concept

`stream::verify_station_stream()` connects to a real stream, buffers the
first chunk, and decodes the first frame with Symphonia (proving
connect → buffer → decode). Playback itself streams continuously via
`reqwest::bytes_stream` without downloading first.

Check a station manually:

```bash
curl -s -A "lofi-cli/0.1" https://ice1.somafm.com/groovesalad-128-mp3 \
  | head -c 64000 > /tmp/probe.mp3
ffprobe /tmp/probe.mp3  # or: cargo test stream
```

## Testing

```bash
cargo test          # unit + integration (no audio device needed)
cargo test --features audio   # needs system audio libs + device for real output
```

Manual: Linux terminal, Windows Terminal (TUI keys, reconnect by
disconnecting network, small-terminal layout < 60x12).

## License

MIT — see LICENSE.
