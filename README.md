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
│                                #   Decoder/Encoder traits, CodecRegistry, CodecContext,
│                                #   FlacDecoder, MjpegDecoder, PCMAudio, RawVideo, SRT/WebVTT)
├── rsmpeg-format/               # Container format layer (IOContext with 64 KiB read
│                                #   buffer, Stream, probe, InputFormat/OutputFormat
│                                #   traits, FormatRegistry, FormatContext input/output,
│                                #   7 demuxers + WAV/Y4M muxers)
├── rsmpeg-filter/               # Filter graph (FilterGraph DAG, Pad, FilterContext,
│                                #   BufferSrc/Sink, built-in Scale/Trim/Null/Overlay)
├── rsmpeg-scale/                # Video scaling (Scaler, color space conversion,
│                                #   interpolation methods)
├── rsmpeg-resample/             # Audio resampling (Resampler, channel mapping,
│                                #   dithering, format conversion)
├── rsmpeg-player/               # Unified playback engine (native demux/decode +
│                                #   OpenH264/Symphonia backends, sync, audio/video output)
├── rsmpeg-gui/                  # GPUI desktop media player (`docs/gui.md`)
└── rsmpeg-cli/                  # Command-line tools (probe, decode, transcode, play, gui)
    └── rsmpeg                  # Binary: `rsmpeg probe|decode|transcode|play|gui|list-formats|list-codecs`
```

| Crate | FFmpeg Equivalent | Purpose |
|-------|-------------------|---------|
| `rsmpeg-util` | `libavutil` | Common types, error handling, rational math, format enums |
| `rsmpeg-codec` | `libavcodec` | Codec identification, packet/frame types, decode/encode traits, pure-Rust decoders |
| `rsmpeg-format` | `libavformat` | Container format I/O, demuxer/muxer registry, format detection |
| `rsmpeg-filter` | `libavfilter` | Filter graph DAG, source/sink buffers, built-in video filters |
| `rsmpeg-scale` | `libswscale` | Video scaling, pixel format conversion, color space math |
| `rsmpeg-resample` | `libswresample` | Audio resampling, channel remixing, dithering |
| `rsmpeg-player` | — | Unified playback control plane, clock sync, native/backend pipelines |
| `rsmpeg-gui` | — | GPU-accelerated desktop player built with GPUI |

## Quick Start

### Prerequisites

- [Rust](https://rustup.rs/) 1.75 or later (`rsmpeg-gui` requires Rust 1.88+ for `gpui 0.2.2`)

### Build

```bash
git clone https://github.com/stevenke1981/rsmpeg.git
cd rsmpeg
cargo build --workspace
```

#### Pure-Rust / Feature-Gated Builds (`rsmpeg-player` & `rsmpeg-cli`)

`rsmpeg-player` and `rsmpeg-cli` gate external/C backends behind Cargo features so you can build and test the pure-Rust pipeline independently:

```bash
# Pure-Rust decoders + native demuxers/muxers only (no OpenH264, no Symphonia, no Rodio)
cargo test -p rsmpeg-player -p rsmpeg-cli --no-default-features

# Pure-Rust playback including Symphonia audio codecs + Rodio output (no C/C++ OpenH264)
cargo build -p rsmpeg-cli --no-default-features --features pure-rust

# rsmpeg native demuxers/decoders only + Rodio audio output
cargo build -p rsmpeg-cli --no-default-features --features native-only
```

### Run Tests

```bash
cargo test --workspace
cargo test -p rsmpeg-player -p rsmpeg-cli --no-default-features
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

# Decode FLAC or WAV audio with pure-Rust decoders and export 16-bit PCM WAV
cargo run --bin rsmpeg -- decode audio.flac --output decoded.wav

# Decode MJPEG / RawVideo frames and export the first frame as binary PPM
cargo run --bin rsmpeg -- decode mjpeg_clip.mp4 --output first_frame.ppm --max-frames 30

# Pure-Rust audio transcoding to WAV (FLAC/WAV -> resampled/remixed PCM WAV)
cargo run --bin rsmpeg -- transcode input.flac output.wav --acodec pcm_s16le --sample-rate 48000 --channels 2
cargo run --bin rsmpeg -- transcode input.wav output_mono.wav --sample-rate 48000 --channels 1

# Pure-Rust video transcoding to Y4M (MJPEG AVI/MP4 or Y4M -> scaled raw YUV4MPEG2)
cargo run --bin rsmpeg -- transcode mjpeg_clip.avi output.y4m --vcodec rawvideo --width 640 --height 360 --max-frames 60
cargo run --bin rsmpeg -- transcode input.y4m scaled.y4m --width 1280 --height 720

# Parse and inspect SRT / WebVTT subtitle cues
cargo run --bin rsmpeg -- decode subtitles.srt
cargo run --bin rsmpeg -- decode captions.vtt

# Play media file or launch the GPUI desktop player (see docs/gui.md)
cargo run --bin rsmpeg -- play example.mp4
cargo run -p rsmpeg-gui -- example.mp4
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
- **Codec**: `CodecId` enum (27+ formats), `Packet`/`Frame` types, `Codec` + `Decoder`/`Encoder` traits, `CodecRegistry` global singleton, builder-pattern `CodecContext`, and pure-Rust codecs/decoders:
  - `FlacDecoder` (`FlacCodec`): RFC 9639 lossless FLAC audio decoder with 64-bit `BitReader` (CRC-8/CRC-16 verification, CONSTANT/VERBATIM/FIXED/LPC subframes, stereo decorrelation, multi-frame packets)
  - `MjpegDecoder` (`MjpegCodec`): ITU-T T.81 Baseline DCT Motion-JPEG video decoder with integer IDCT (`SOF0`, Annex K.3.3 default Huffman tables, 8/16-bit DQT, grayscale & YCbCr sampling, restart markers, outputs `Rgb24`)
  - `PCMAudioDecoder` / `PCMAudioEncoder` (`PCMAudioCodec`): uncompressed PCM (`U8`, `S16`, `S32`, `F32`, `F64`)
  - `RawVideoDecoder` / `RawVideoEncoder` (`RawVideoCodec`): uncompressed video frame passthrough with planar/semi-planar/packed layout splitting
  - `SrtCodec`, `WebVttCodec`, `SubtitleDecoder`, `SubtitleTrack`, `SubtitleCue`, and `parse_srt` / `parse_webvtt` / `parse_subtitle_file`
- **Format**: `IOContext` abstraction (File with 64 KiB read buffer / Memory Buffer), magic-byte format probing, `InputFormat`/`OutputFormat` traits, `FormatRegistry`, `FormatContext` input/output (`open_input`, `open_output`, `open_output_buffer`), **7 registered demuxers** (`mp4`, `mkv`, `avi`, `flac`, `wav`, `rawvideo`, `y4m`), and **2 registered muxers** (`wav`, `y4m`):
  - Stateful `FLACDemuxer`: `read_header` (STREAMINFO/SEEKTABLE/VORBIS_COMMENT), streaming `read_frame` with frame-sync & CRC-16 boundary scanning, and `seek` using SEEKTABLE binary search / byte-offset estimation
  - `MP4Demuxer`: H.264/AAC plus MJPEG visual sample entries (`jpeg`, `mjpa`, `mjpb`, `mjpg`, and `mp4v` with `esds` `objectTypeIndication = 0x6C`) and FLAC audio sample entries (`fLaC` + `dfLa` STREAMINFO extradata)
  - `Y4mDemuxer` / `Y4mMuxer` & `WAVMuxer`: pure-Rust YUV4MPEG2 (`C420`/`C422`/`C444`/`Cmono`) and RIFF/WAVE PCM (`U8`/`S16`/`S32`/`F32`) muxing & demuxing with seek support
- **Filter**: Filter trait, FilterGraph DAG, Pad/PadDirection, BufferSrc/BufferSink, built-in filters (Scale, Trim, Null, Overlay, Transpose, plus RGBA stride-safe fast paths in `BoxBlurFilter`, `CropFilter`, `GrayscaleFilter`, `MirrorFilter`, `RotateFilter`)
- **Scale**: Scaler with ScalerConfig builder, 4 active interpolation kernels (`nearest`, `bilinear`, `bicubic`, `lanczos`), color space definitions (BT.601/BT.709/BT.2020/RGB, limited/full range), and 1:1 fast paths for `Yuv420P`/`Yuv422P`/`Yuv444P`/`Nv12`/`Nv21` → packed RGB/BGR/RGBA
- **Resample**: Resampler with ResamplerConfig, `U8`/`S16`/`S32`/`F32`/`F64` + planar (`S16P`/`S32P`/`F32P`/`F64P`) input support, weighted `ChannelMapping` matrix downmix (including 5.1 → stereo), and dithering methods
- **Player & GUI**: `rsmpeg-player` unified playback worker (feature-gated `backend-symphonia`, `backend-openh264`, `audio-rodio`, `pure-rust`, `native-only`; native `rsmpeg-format` + pure-Rust `FlacDecoder`/`MjpegDecoder`/`RawVideoDecoder`/`PCMAudioDecoder` pipeline, OpenH264 & Symphonia fallback, `DecoderInfo` reporting, frame-stepping) and `rsmpeg-gui` GPUI desktop player (see [docs/gui.md](docs/gui.md))
- **CLI**: `probe` (JSON/verbose), `decode` (pure-Rust FLAC/MJPEG/PCM/RawVideo/SRT/WebVTT decoding with `.wav`/`.ppm` export and `--max-frames`), `transcode` (pure-Rust `.flac`/`.wav` → `.wav` audio resampling/remixing and `.avi`/`.mp4`/`.y4m` → `.y4m` video scaling/conversion), `play`, `gui`, `list-formats` (demuxers + muxers), `list-codecs`

### 🚧 In Progress

- Full decode → scale → encode transcoding pipeline
- Hardware-accelerated codec support via GPU APIs
- Streaming protocol support (RTMP, HLS, SRT)

### ❌ Explicitly Out of Scope

- Patent-encumbered codec algorithms (H.264/H.265/AAC decoding from scratch)
- Unsafe code or FFI bindings to C libraries

## Project Status

This project is in **active development**. The core architecture is stable, pure-Rust decoders and demuxers power both CLI tools and native playback in `rsmpeg-player` / `rsmpeg-gui`, and higher-level transcoding workflows are under active expansion.

Comprehensive unit and regression tests live across all workspace crates.

## Design Principles

1. **Zero `unsafe`** — All crates use `#![forbid(unsafe_code)]`
2. **Trait-based polymorphism** — Codecs, demuxers, filters all use Rust traits for extensibility
3. **Registry pattern** — Global registries via `OnceLock<RwLock<...>>` for runtime codec/format discovery
4. **No C dependencies** — Pure Rust throughout, no FFI or bindgen
5. **Modular architecture** — Each subsystem is a separate crate with minimal cross-dependency

## License

MIT
