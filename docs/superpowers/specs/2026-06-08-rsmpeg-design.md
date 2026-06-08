# rsmpeg — Pure Rust FFmpeg Clone

**Date:** 2026-06-08
**Status:** Approved Design
**Reference:** OxiMedia (https://github.com/cool-japan/oximedia) — design patterns and architecture

## 1. Vision

rsmpeg is a **clean-room, pure Rust reconstruction of FFmpeg** — a memory-safe,
patent-free multimedia processing framework with zero C/Fortran dependencies.

It mirrors FFmpeg's library architecture (libavutil → rsmpeg-util, libavcodec →
rsmpeg-codec, etc.) while providing a modern Rust API with trait-based codec/format
registration, async-aware I/O, and compile-time memory safety.

rsmpeg is **independent** of OxiMedia — it implements its own types, traits, and
processing logic, referencing OxiMedia only for architectural patterns.

## 2. Workspace Architecture

```
rsmpeg/
├── Cargo.toml                   # Workspace manifest (resolver = "2")
├── rsmpeg-util/                 # libavutil — error types, Rational, Dict, BufferPool
├── rsmpeg-codec/                # libavcodec — Codec trait, registry, Packet, Frame
├── rsmpeg-format/               # libavformat — InputFormat/OutputFormat traits, demux/mux
├── rsmpeg-filter/               # libavfilter — Filter trait, FilterGraph (DAG)
├── rsmpeg-scale/                # libswscale — scaling, pixel format conversion
├── rsmpeg-resample/             # libswresample — audio resampling, channel layout
├── rsmpeg-cli/                  # ffmpeg/ffprobe/ffplay CLI (subcommand dispatch)
├── examples/                    # Usage examples
└── docs/
    └── superpowers/
        └── specs/               # Design documents
```

### 2.1 Design Principles

| Principle | Description |
|-----------|-------------|
| **Pure Rust** | `#![forbid(unsafe_code)]` enforced workspace-wide |
| **Patent-Free** | Only royalty-free codecs (AV1, VP9, VP8, Theora, Opus, FLAC, etc.) |
| **Trait-Based Registry** | Codec/Format registered dynamically via trait objects |
| **Builder Pattern** | CodecContext/FormatContext built via builder API |
| **No C Dependencies** | Zero C/Fortran required for default build |
| **Async Optional** | Core processing is synchronous; I/O can use async |

## 3. Crate Specifications

### 3.1 rsmpeg-util (libavutil)

Core utility types shared across all crates.

**Key types:**
- `RsError` / `RsResult<T>` — unified error enum (thiserror)
  - Variants: Io, InvalidData, Unsupported, Codec, Format, Bug, etc.
- `Rational` — rational number (num/den) with arithmetic
- `Dict` — metadata key-value store (AVDictionary equivalent)
- `BufferPool` — reusable memory buffer pool
- `PixelFormat` — pixel format enum (YUV420P, NV12, RGBA, etc.)
- `SampleFormat` — audio sample format enum (S16, S32, F32, F64)
- `ChannelLayout` — audio channel layout (Mono, Stereo, Surround51, etc.)
- `MediaType` — video/audio/subtitle/data/attachment
- `Timestamp` — timestamp with timebase handling

### 3.2 rsmpeg-codec (libavcodec)

Codec encoding and decoding subsystem.

**Key types:**
- `CodecId` — codec identifier enum
  - Video: Av1, Vp9, Vp8, Theora, Mpeg4, H263, Mjpeg, Ffv1, JpegXl
  - Audio: Opus, Vorbis, Flac, Mp3, Pcm*, Alac
  - Image: Png, Gif, WebP
  - Subtitle: Srt, WebVtt
- `Codec` trait — all codecs implement this
- `CodecRegistry` — global codec registration (lazy_static)
- `CodecContext` — codec instance configuration (Builder pattern)
- `CodecParameters` — codec parameter description
- `Packet` — compressed data (AVPacket)
  - `data: Bytes`, `pts/dts: Option<i64>`, `duration`, `stream_index`, `flags`
- `Frame` — uncompressed data (AVFrame)
  - `data: Vec<u8>`, `linesize`, `width/height`, `pix_fmt`, `pts`, `key_frame`
- `PictureType` — I/P/B/SI/SP frame types

**Codec trait:**
```rust
#[async_trait]
pub trait Codec: Send + Sync {
    fn id(&self) -> CodecId;
    fn media_type(&self) -> MediaType;
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> CodecCapabilities;
}
```

**CodecRegistry:**
```rust
lazy_static! {
    pub static ref CODEC_REGISTRY: CodecRegistry = { ... };
}
impl CodecRegistry {
    pub fn register(&mut self, codec: Box<dyn Codec>);
    pub fn find_by_id(&self, id: CodecId) -> Option<&dyn Codec>;
    pub fn find_by_name(&self, name: &str) -> Option<&dyn Codec>;
    pub fn list(&self) -> Vec<&dyn Codec>;
}
```

**CodecContext decoder interface:**
```rust
pub fn decode(&mut self, packet: &Packet) -> Result<Vec<Frame>>;
pub fn send_packet(&mut self, packet: &Packet) -> Result<()>;
pub fn receive_frame(&mut self) -> Result<Option<Frame>>;
```

### 3.3 rsmpeg-format (libavformat)

Container format demuxing and muxing.

**Key types:**
- `InputFormat` trait — demuxer interface
- `OutputFormat` trait — muxer interface
- `FormatRegistry` — global format registration
- `FormatContext` — input/output format instance
- `IOContext` — I/O abstraction (file, memory, network)
- `Stream` — media stream descriptor (index, codec_id, params, time_base, metadata)
- `PacketList` — ordered packet queue for interleaving
- `ProbeScore` — confidence level for format detection

**InputFormat trait:**
```rust
#[async_trait]
pub trait InputFormat: Send + Sync {
    fn name(&self) -> &'static str;
    fn extensions(&self) -> &'static [&'static str];
    fn probe(&self, buf: &[u8]) -> ProbeScore;
    fn read_header(&mut self, ctx: &mut FormatContext) -> Result<()>;
    fn read_frame(&mut self, ctx: &mut FormatContext) -> Result<Option<Packet>>;
    fn seek(&mut self, ctx: &mut FormatContext, timestamp: i64, flags: SeekFlags) -> Result<()>;
}
```

**ProbeScore levels:**
```rust
pub enum ProbeScore {
    NoMatch = 0,
    Possible = 25,
    Likely = 50,
    VeryLikely = 75,
    Certain = 100,
}
```

### 3.4 rsmpeg-filter (libavfilter)

Filter graph processing pipeline.

**Key types:**
- `Filter` trait — individual filter implementation
- `FilterGraph` — DAG of connected filters
- `FilterNode` — a filter instance in the graph
- `FilterEdge` — connection between filter pads
- `BufferSrc` — source pad (inject frames into graph)
- `BufferSink` — sink pad (extract frames from graph)

**Filter trait:**
```rust
pub trait Filter: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn inputs(&self) -> usize;
    fn outputs(&self) -> usize;
    fn process(&mut self, inputs: &[&Frame], params: &FilterParams) -> Result<Vec<Frame>>;
}
```

### 3.5 rsmpeg-scale (libswscale)

Image scaling and pixel format conversion.

**Key types:**
- `SwsContext` — scaling context
- `SwsFlags` — scaling algorithm selection
- `Colorspace` — color space definition (BT.601, BT.709, BT.2020)

```rust
pub struct SwsContext {
    src_format: PixelFormat,
    dst_format: PixelFormat,
    src_size: (usize, usize),
    dst_size: (usize, usize),
    flags: SwsFlags,
    colorspace: Colorspace,
}
impl SwsContext {
    pub fn scale(&self, src: &Frame) -> Result<Frame>;
}
```

### 3.6 rsmpeg-resample (libswresample)

Audio resampling and format conversion.

**Key types:**
- `SwrContext` — audio resampling context
- `AudioFrame` — specialized audio frame with channel interleaving info

### 3.7 rsmpeg-cli

Command-line tools matching FFmpeg's three binaries.

**Subcommand dispatch** (single binary):
```text
rsmpeg probe <input> [options]       → ffprobe equivalent
rsmpeg transcode <input> <output> [opts] → ffmpeg equivalent
rsmpeg play <input> [options]        → ffplay equivalent
```

**ffprobe output format** (JSON):
```json
{
  "format": {
    "filename": "...",
    "nb_streams": 2,
    "format_name": "mp4",
    "duration": "120.500000",
    "size": "52428800",
    "bit_rate": "3481600"
  },
  "streams": [
    {
      "index": 0,
      "codec_type": "video",
      "codec_name": "av1",
      "width": 1920,
      "height": 1080,
      "pix_fmt": "yuv420p",
      "r_frame_rate": "30/1"
    }
  ]
}
```

## 4. Data Flow

### Transcode Pipeline
```
File → IOContext → InputFormat::read_header()
                 → [loop] InputFormat::read_packet()
                         → CodecContext::decode() → Frame
                         → [optional] FilterGraph::process()
                         → CodecContext::encode() → Packet
                         → OutputFormat::write_packet()
                 → OutputFormat::write_trailer()
```

### Probe Pipeline
```
File → IOContext → [magic bytes] → probe all formats
                 → best match → InputFormat::read_header()
                 → extract streams → FormatContext
                 → serialize to JSON
```

### Play Pipeline
```
File → InputFormat → decode → Frame Queue → Audio/Video sync → Output
```

## 5. Supported Formats (Phase 1)

### Containers
| Format | Demuxer | Muxer | Notes |
|--------|---------|-------|-------|
| MP4    | ✅ Skeleton | ✅ Skeleton | ISO BMFF, ftyp box parsing |
| MKV    | ✅ Skeleton | ✅ Skeleton | EBML, Segment, Cluster |
| WebM   | ✅ Skeleton | ✅ Skeleton | Inherits MKV, restricted codecs |
| AVI    | ✅ Skeleton | ✅ Skeleton | RIFF-based |
| MPEG-TS| ✅ Skeleton | ⬜ | Transport stream |

### Codecs
| Codec | Decoder | Encoder | Notes |
|-------|---------|---------|-------|
| AV1   | ✅ Skeleton | ⬜ | OBU parsing |
| VP9   | ✅ Skeleton | ⬜ | Frame/tile parsing |
| VP8   | ✅ Skeleton | ⬜ | Basic frame decode |
| H.263 | ✅ Skeleton | ⬜ | Macroblock decode |
| Opus  | ✅ Skeleton | ⬜ | CELT-only |
| Vorbis| ✅ Skeleton | ⬜ | Header parsing |
| FLAC  | ✅ Skeleton | ✅ Skeleton | Lossless decode/encode |
| MP3   | ✅ Skeleton | ⬜ | Huffman/IMDCT |
| PCM   | ✅ Full | ✅ Full | Raw PCM, all formats |
| MJPEG | ✅ Skeleton | ⬜ | JPEG frames |

## 6. Error Handling

```rust
#[derive(Debug, thiserror::Error)]
pub enum RsError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("Invalid data: {0}")]
    InvalidData(Cow<'static, str>),
    
    #[error("Unsupported feature: {0}")]
    Unsupported(Cow<'static, str>),
    
    #[error("Codec error: {0}")]
    Codec(Cow<'static, str>),
    
    #[error("Format error: {0}")]
    Format(Cow<'static, str>),
    
    #[error("Filter error: {0}")]
    Filter(Cow<'static, str>),
    
    #[error("Resource not found: {0}")]
    NotFound(Cow<'static, str>),
    
    #[error("Internal error: {0}")]
    Bug(Cow<'static, str>),
}
```

## 7. Key Dependencies

| Dependency | Purpose | Used By |
|-----------|---------|---------|
| `thiserror` | Error derive macros | All crates |
| `bytes` | Zero-copy byte buffers | codec, format |
| `tracing` | Structured logging | All crates |
| `bitflags` | Flag types (PacketFlags) | codec |
| `lazy_static` or `once_cell` | Global registries | codec, format |
| `serde` / `serde_json` | JSON probe output | cli |

## 8. Non-Goals (Phase 1)

- Complete pixel-accurate decoder implementations (stubs are acceptable)
- Hardware acceleration (Vulkan/Metal/CUDA)
- Network streaming protocols (HLS/DASH/RTMP/SRT)
- DRM / encryption
- Subtitle rendering
- Frame-accurate seeking
- Audio playback (ffplay uses simple PCM output)
- WASM support
- Patent-encumbered codecs (H.264, H.265, AAC)
