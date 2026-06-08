# rsmpeg

**Pure Rust multimedia framework — a full FFmpeg equivalent, written in safe Rust.**

rsmpeg is a modular, pure-Rust multimedia processing library inspired by FFmpeg's architecture. It provides a complete suite of media processing tools for reading, inspecting, transforming, and writing audio and video content — all in safe Rust (`#![forbid(unsafe_code)]`).

## Architecture

rsmpeg mirrors FFmpeg's component model with dedicated crates for each subsystem:

```
rsmpeg/                          # Facade crate (unified public API + re-exports)
├── rsmpeg-util/                 # Common utilities (error types, rational numbers,
│                                #   media types, pixel/sample formats, channel layouts)
├── rsmpeg-codec/                # Codec layer (CodecId, Packet, Frame, Codec traits,
│                                #   Decoder/Encoder traits, CodecRegistry, CodecContext)
├── rsmpeg-format/               # Container format layer (IOContext, Stream, probe,
│                                #   InputFormat/OutputFormat traits, FormatRegistry,
│                                #   FormatContext, built-in MP4/MKV/AVI/FLAC/WAV demuxers)
├── rsmpeg-filter/               # Filter graph (FilterGraph DAG, Pad, FilterContext,
│                                #   BufferSrc/Sink, built-in Scale/Trim/Null/Overlay)
├── rsmpeg-scale/                # Video scaling (Scaler, color space conversion,
│                                #   interpolation methods)
├── rsmpeg-resample/             # Audio resampling (Resampler, channel mapping,
│                                #   dithering, format conversion)
└── rsmpeg-cli/                  # Command-line tools (probe, transcode, play)
    └── rsmpeg                  # Binary: `rsmpeg probe|transcode|play|list-formats|list-codecs`
```

| Crate | FFmpeg Equivalent | Purpose |
|-------|-------------------|---------|
| `rsmpeg-util` | `libavutil` | Common types, error handling, rational math, format enums |
| `rsmpeg-codec` | `libavcodec` | Codec identification, packet/frame types, decode/encode traits |
| `rsmpeg-format` | `libavformat` | Container format I/O, demuxer/muxer registry, format detection |
| `rsmpeg-filter` | `libavfilter` | Filter graph DAG, source/sink buffers, built-in video filters |
| `rsmpeg-scale` | `libswscale` | Video scaling, pixel format conversion, color space math |
| `rsmpeg-resample` | `libswresample` | Audio resampling, channel remixing, dithering |

## Quick Start

### Prerequisites

- [Rust](https://rustup.rs/) 1.70 or later

### Build

```bash
git clone https://github.com/stevenke1981/rsmpeg.git
cd rsmpeg
cargo build --workspace
```

### Run Tests

```bash
cargo test --workspace
```

### CLI Usage

```bash
# Show help
cargo run --bin rsmpeg -- --help

# List registered format demuxers
cargo run --bin rsmpeg -- list-formats

# List registered codecs
cargo run --bin rsmpeg -- list-codecs

# Probe a media file (basic)
cargo run --bin rsmpeg -- probe example.wav

# Probe with verbose stream details
cargo run --bin rsmpeg -- probe example.mp4 --verbose

# JSON output for programmatic use
cargo run --bin rsmpeg -- probe example.mkv --json
```

### Examples

```bash
# Run the pipeline example (demonstrates all layers)
cargo run --example pipeline

# Basic file probe
cargo run --example probe_basic -- example.wav

# Filter graph construction
cargo run --example filter_graph

# Version info
cargo run --example version
```

## Features

### ✅ Implemented

- **Util**: Error types, rational arithmetic, media type detection, pixel/sample format enums, channel layout bitflags, dictionary
- **Codec**: CodecId enum (22+ formats), Packet/Frame types, Codec trait + Decoder/Encoder traits, CodecRegistry with global singleton, Builder-pattern CodecContext, built-in RawVideo and PCM audio codecs
- **Format**: IOContext abstraction (File/Buffer), format probing by magic bytes, InputFormat/OutputFormat traits, FormatRegistry, FormatContext with header parsing, **5 real demuxers** (MP4, MKV, AVI, FLAC, WAV)
- **Filter**: Filter trait, FilterGraph DAG, Pad/PadDirection, BufferSrc/BufferSink, built-in filters (Scale, Trim, Null, Overlay, Transpose)
- **Scale**: Scaler with ScalerConfig builder, 7 interpolation methods, color space definitions (BT.601/709/2020/RGB)
- **Resample**: Resampler with ResamplerConfig, channel mapping matrix, dithering methods
- **CLI**: probe (with JSON/verbose output), transcode (skeleton), play (skeleton), list-formats, list-codecs

### 🚧 In Progress

- Full decode → scale → encode pipeline
- Hardware-accelerated codec support via GPU APIs
- Streaming protocol support (RTMP, HLS, SRT)
- Real-time playback via audio/video device output

### ❌ Explicitly Out of Scope

- Patent-encumbered codec algorithms (H.264/H.265/AAC decoding from scratch)
- Unsafe code or FFI bindings to C libraries

## Project Status

This project is in **active development**. The core architecture is stable, and the base component layers are functional. Higher-level features like full frame-by-frame transcoding and real-time playback are in progress.

Current test coverage: **45+ tests** across all crates, all passing.

## Design Principles

1. **Zero `unsafe`** — All crates use `#![forbid(unsafe_code)]`
2. **Trait-based polymorphism** — Codecs, demuxers, filters all use Rust traits for extensibility
3. **Registry pattern** — Global registries via `OnceLock<RwLock<...>>` for runtime codec/format discovery
4. **No C dependencies** — Pure Rust throughout, no FFI or bindgen
5. **Modular architecture** — Each subsystem is a separate crate with minimal cross-dependency

## License

MIT
