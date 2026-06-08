# rsmpeg Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Build a pure-Rust FFmpeg clone with workspace skeleton (7 lib crates + CLI) implementing core types, trait-based codec/format registries, and three CLI tools (probe/transcode/play).

**Architecture:** 7 library crates mirroring FFmpeg's libavutil/libavcodec/libavformat/libavfilter/libswscale/libswresample + facade crate + CLI crate. Trait-based dynamic registration for codecs and formats.

**Tech Stack:** Rust 2021 edition, thiserror, bytes, tracing, bitflags, serde/serde_json, clap

---

## File Structure

```
D:\rsmpeg\
├── Cargo.toml                          # Workspace
├── rsmpeg-util/
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── error.rs                    # RsError, RsResult
│       ├── rational.rs                 # Rational
│       ├── media_type.rs               # MediaType
│       ├── pixel_format.rs             # PixelFormat
│       ├── sample_format.rs            # SampleFormat
│       ├── channel_layout.rs           # ChannelLayout
│       └── dict.rs                     # Metadata Dict
├── rsmpeg-codec/
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── codec_id.rs                 # CodecId
│       ├── codec.rs                    # Codec trait
│       ├── codec_registry.rs           # CodecRegistry
│       ├── codec_context.rs            # CodecContext
│       ├── codec_parameters.rs         # CodecParameters
│       ├── packet.rs                   # Packet
│       ├── frame.rs                    # Frame
│       └── picture_type.rs             # PictureType
├── rsmpeg-format/
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── format.rs                   # InputFormat/OutputFormat traits
│       ├── format_registry.rs          # FormatRegistry
│       ├── format_context.rs           # FormatContext
│       ├── io_context.rs               # IOContext
│       ├── stream.rs                   # Stream
│       └── probe.rs                    # ProbeScore, probe_format
├── rsmpeg-filter/
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── filter.rs                   # Filter trait
│       ├── filter_graph.rs             # FilterGraph (DAG)
│       └── pad.rs                      # FilterPad, BufferSrc, BufferSink
├── rsmpeg-scale/
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── sws_context.rs              # SwsContext
│       └── colorspace.rs               # Colorspace
├── rsmpeg-resample/
│   ├── Cargo.toml
│   └── src/
│       └── lib.rs                      # SwrContext
├── rsmpeg/
│   ├── Cargo.toml
│   └── src/lib.rs
├── rsmpeg-cli/
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs                     # Subcommand dispatch
│       ├── probe_cmd.rs                # ffprobe equivalent
│       ├── transcode_cmd.rs            # ffmpeg equivalent
│       └── play_cmd.rs                 # ffplay equivalent
├── rsmpeg-codec/src/codecs/
│   └── mod.rs                          # Codec impl stubs
├── rsmpeg-format/src/formats/
│   └── mod.rs                          # Format impl stubs
└── examples/
    ├── probe_file.rs
    └── simple_transcode.rs
```

---

### Task 1: Create workspace root + rsmpeg-util Cargo.toml

**Files:**
- Create: `D:\rsmpeg\Cargo.toml`
- Create: `D:\rsmpeg\rsmpeg-util\Cargo.toml`
- Create: `D:\rsmpeg\rsmpeg-util\src\lib.rs`

- [ ] **Step 1: Create workspace Cargo.toml**

```toml
[workspace]
resolver = "2"
members = [
    "rsmpeg-util",
    "rsmpeg-codec",
    "rsmpeg-format",
    "rsmpeg-filter",
    "rsmpeg-scale",
    "rsmpeg-resample",
    "rsmpeg",
    "rsmpeg-cli",
]

[workspace.package]
version = "0.1.0"
edition = "2021"
license = "Apache-2.0"
rust-version = "1.75"
authors = ["rsmpeg Contributors"]
description = "Pure Rust FFmpeg clone — patent-free multimedia processing"

[workspace.lints.rust]
unsafe_code = "deny"
missing_docs = "allow"

[workspace.dependencies]
thiserror = "2"
bytes = "1"
tracing = "0.1"
bitflags = "2"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
clap = { version = "4", features = ["derive"] }
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
anyhow = "1"
```

- [ ] **Step 2: Create rsmpeg-util Cargo.toml**

```toml
[package]
name = "rsmpeg-util"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true
description = "Core utility types for rsmpeg (libavutil equivalent)"

[lints]
workspace = true

[dependencies]
thiserror.workspace = true
tracing.workspace = true
serde.workspace = true
bitflags.workspace = true
```

- [ ] **Step 3: Create rsmpeg-util/src/lib.rs**

```rust
//! rsmpeg-util — Core utility types (libavutil equivalent)
//!
//! Provides foundational types shared across all rsmpeg crates:
//! error handling, rational arithmetic, media types, pixel/sample formats.

#![forbid(unsafe_code)]

pub mod error;
pub mod rational;
pub mod media_type;
pub mod pixel_format;
pub mod sample_format;
pub mod channel_layout;
pub mod dict;

pub use error::{RsError, RsResult};
pub use rational::Rational;
pub use media_type::MediaType;
pub use pixel_format::PixelFormat;
pub use sample_format::SampleFormat;
pub use channel_layout::ChannelLayout;
pub use dict::Dict;
```

### Task 2: Implement rsmpeg-util — error, rational, media_type

- [ ] **Step 1: Write error.rs**

```rust
use std::borrow::Cow;
use std::fmt;

/// Unified error type for all rsmpeg operations.
#[derive(Debug)]
pub enum RsError {
    /// I/O operation failed (file not found, read error, etc.)
    Io(std::io::Error),
    /// Data is malformed or violates format/codec spec
    InvalidData(Cow<'static, str>),
    /// Feature or codec not supported
    Unsupported(Cow<'static, str>),
    /// Codec processing error
    Codec(Cow<'static, str>),
    /// Container format processing error
    Format(Cow<'static, str>),
    /// Filter graph error
    Filter(Cow<'static, str>),
    /// Resource (codec, format) not found
    NotFound(Cow<'static, str>),
    /// Internal logic error (shouldn't happen)
    Bug(Cow<'static, str>),
}

impl fmt::Display for RsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RsError::Io(e) => write!(f, "I/O error: {e}"),
            RsError::InvalidData(msg) => write!(f, "Invalid data: {msg}"),
            RsError::Unsupported(msg) => write!(f, "Unsupported: {msg}"),
            RsError::Codec(msg) => write!(f, "Codec error: {msg}"),
            RsError::Format(msg) => write!(f, "Format error: {msg}"),
            RsError::Filter(msg) => write!(f, "Filter error: {msg}"),
            RsError::NotFound(msg) => write!(f, "Not found: {msg}"),
            RsError::Bug(msg) => write!(f, "Internal error: {msg}"),
        }
    }
}

impl std::error::Error for RsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RsError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for RsError {
    fn from(e: std::io::Error) -> Self {
        RsError::Io(e)
    }
}

/// Convenience alias for rsmpeg results.
pub type RsResult<T> = Result<T, RsError>;

/// Helper macros for creating common errors.
#[macro_export]
macro_rules! invalid_data {
    ($msg:expr) => { $crate::error::RsError::InvalidData(std::borrow::Cow::Borrowed($msg)) };
    ($fmt:expr, $($arg:tt)*) => {
        $crate::error::RsError::InvalidData(std::borrow::Cow::Owned(format!($fmt, $($arg)*)))
    };
}

#[macro_export]
macro_rules! unsupported {
    ($msg:expr) => { $crate::error::RsError::Unsupported(std::borrow::Cow::Borrowed($msg)) };
    ($fmt:expr, $($arg:tt)*) => {
        $crate::error::RsError::Unsupported(std::borrow::Cow::Owned(format!($fmt, $($arg)*)))
    };
}

#[macro_export]
macro_rules! codec_error {
    ($msg:expr) => { $crate::error::RsError::Codec(std::borrow::Cow::Borrowed($msg)) };
    ($fmt:expr, $($arg:tt)*) => {
        $crate::error::RsError::Codec(std::borrow::Cow::Owned(format!($fmt, $($arg)*)))
    };
}

#[macro_export]
macro_rules! not_found {
    ($msg:expr) => { $crate::error::RsError::NotFound(std::borrow::Cow::Borrowed($msg)) };
}
```

- [ ] **Step 2: Write rational.rs**

```rust
use std::fmt;

/// Rational number (numerator/denominator), equivalent to FFmpeg's AVRational.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rational {
    pub num: i32,
    pub den: i32,
}

impl Rational {
    pub const fn new(num: i32, den: i32) -> Self {
        Rational { num, den }
    }

    /// Create from a floating-point value with a reasonable approximation.
    pub fn from_f64(value: f64) -> Self {
        // Simple rational approximation using continued fractions
        const MAX_DEN: i32 = 1_000_000;
        if value.is_nan() || value.is_infinite() {
            return Rational { num: 0, den: 1 };
        }
        // Handle negative values
        let sign = if value < 0.0 { -1 } else { 1 };
        let value = value.abs();

        // Basic continued fraction approach
        let mut a0 = value.floor() as i32;
        let mut frac = value - value.floor();
        let mut num_prev = 0i64;
        let mut num_curr = 1i64;
        let mut den_prev = 1i64;
        let mut den_curr = 0i64;

        for _ in 0..20 {
            if frac < 1e-12 {
                break;
            }
            let inv = 1.0 / frac;
            let a = inv.floor() as i64;
            frac = inv - inv.floor();

            let num_next = a * num_curr + num_prev;
            let den_next = a * den_curr + den_prev;

            if den_next > MAX_DEN as i64 {
                break;
            }

            num_prev = num_curr;
            num_curr = num_next;
            den_prev = den_curr;
            den_curr = den_next;
        }

        let num = (a0 as i64 * den_curr + num_curr) * sign as i64;
        Rational {
            num: num as i32,
            den: den_curr as i32,
        }
    }

    /// Convert to f64.
    pub fn to_f64(self) -> f64 {
        if self.den == 0 {
            0.0
        } else {
            self.num as f64 / self.den as f64
        }
    }

    /// Reduce fraction by GCD.
    pub fn reduced(self) -> Self {
        let gcd = gcd(self.num.abs(), self.den.abs());
        if gcd > 1 {
            Rational {
                num: self.num / gcd,
                den: self.den / gcd,
            }
        } else {
            self
        }
    }

    /// Multiply two rationals.
    pub fn mul(self, other: Rational) -> Self {
        Rational::new(self.num * other.num, self.den * other.den).reduced()
    }

    /// Divide two rationals.
    pub fn div(self, other: Rational) -> Self {
        Rational::new(self.num * other.den, self.den * other.num).reduced()
    }

    /// Add two rationals.
    pub fn add(self, other: Rational) -> Self {
        Rational::new(
            self.num * other.den + other.num * self.den,
            self.den * other.den,
        )
        .reduced()
    }

    /// Check if denominator is zero.
    pub fn is_zero(self) -> bool {
        self.num == 0 || self.den == 0
    }
}

fn gcd(a: i32, b: i32) -> i32 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

impl fmt::Display for Rational {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.num, self.den)
    }
}

impl From<(i32, i32)> for Rational {
    fn from((num, den): (i32, i32)) -> Self {
        Rational::new(num, den)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rational_basic() {
        let r = Rational::new(1, 2);
        assert_eq!(r.to_f64(), 0.5);
    }

    #[test]
    fn test_rational_from_f64() {
        let r = Rational::from_f64(0.33333);
        assert!((r.to_f64() - 1.0 / 3.0).abs() < 0.001);
    }

    #[test]
    fn test_rational_reduce() {
        let r = Rational::new(4, 8).reduced();
        assert_eq!(r.num, 1);
        assert_eq!(r.den, 2);
    }

    #[test]
    fn test_rational_mul() {
        let a = Rational::new(1, 2);
        let b = Rational::new(2, 3);
        assert_eq!(a.mul(b).to_f64(), 1.0 / 3.0);
    }
}
```

- [ ] **Step 3: Write media_type.rs**

```rust
use serde::{Deserialize, Serialize};

/// Media stream type, equivalent to FFmpeg's AVMediaType.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MediaType {
    Video,
    Audio,
    Subtitle,
    Data,
    Attachment,
}

impl MediaType {
    pub fn name(self) -> &'static str {
        match self {
            MediaType::Video => "video",
            MediaType::Audio => "audio",
            MediaType::Subtitle => "subtitle",
            MediaType::Data => "data",
            MediaType::Attachment => "attachment",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "video" => Some(MediaType::Video),
            "audio" => Some(MediaType::Audio),
            "subtitle" => Some(MediaType::Subtitle),
            "data" => Some(MediaType::Data),
            "attachment" => Some(MediaType::Attachment),
            _ => None,
        }
    }
}
```

- [ ] **Step 4: Run tests for error, rational, media_type**

Run: `cd D:\rsmpeg && cargo test -p rsmpeg-util`
Expected: All tests pass

### Task 3: Implement rsmpeg-util — pixel_format, sample_format, channel_layout, dict

**Files:**
- Create: `D:\rsmpeg\rsmpeg-util\src\pixel_format.rs`
- Create: `D:\rsmpeg\rsmpeg-util\src\sample_format.rs`
- Create: `D:\rsmpeg\rsmpeg-util\src\channel_layout.rs`
- Create: `D:\rsmpeg\rsmpeg-util\src\dict.rs`

- [ ] **Step 1: Write pixel_format.rs**

```rust
use serde::{Deserialize, Serialize};

/// Pixel format, equivalent to FFmpeg's AVPixelFormat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PixelFormat {
    /// YUV 4:2:0 planar (8-bit)
    Yuv420P,
    /// YUV 4:2:2 planar (8-bit)
    Yuv422P,
    /// YUV 4:4:4 planar (8-bit)
    Yuv444P,
    /// YUV 4:2:0 semi-planar (NV12)
    Nv12,
    /// YUV 4:2:0 semi-planar (NV21)
    Nv21,
    /// RGB 24-bit
    Rgb24,
    /// BGR 24-bit
    Bgr24,
    /// RGBA 32-bit
    Rgba,
    /// BGRA 32-bit
    Bgra,
    /// ARGB 32-bit
    Argb,
    /// Gray 8-bit
    Gray8,
    /// Gray 16-bit
    Gray16,
    /// YUV 4:2:0 10-bit planar
    Yuv420P10,
    /// YUV 4:2:0 12-bit planar
    Yuv420P12,
    /// None / unknown
    None,
}

impl PixelFormat {
    pub fn name(self) -> &'static str {
        match self {
            PixelFormat::Yuv420P => "yuv420p",
            PixelFormat::Yuv422P => "yuv422p",
            PixelFormat::Yuv444P => "yuv444p",
            PixelFormat::Nv12 => "nv12",
            PixelFormat::Nv21 => "nv21",
            PixelFormat::Rgb24 => "rgb24",
            PixelFormat::Bgr24 => "bgr24",
            PixelFormat::Rgba => "rgba",
            PixelFormat::Bgra => "bgra",
            PixelFormat::Argb => "argb",
            PixelFormat::Gray8 => "gray8",
            PixelFormat::Gray16 => "gray16",
            PixelFormat::Yuv420P10 => "yuv420p10",
            PixelFormat::Yuv420P12 => "yuv420p12",
            PixelFormat::None => "none",
        }
    }

    /// Number of bits per pixel (approximate).
    pub fn bits_per_pixel(self) -> usize {
        match self {
            PixelFormat::Yuv420P => 12,
            PixelFormat::Yuv422P => 16,
            PixelFormat::Yuv444P => 24,
            PixelFormat::Nv12 | PixelFormat::Nv21 => 12,
            PixelFormat::Rgb24 | PixelFormat::Bgr24 => 24,
            PixelFormat::Rgba | PixelFormat::Bgra | PixelFormat::Argb => 32,
            PixelFormat::Gray8 => 8,
            PixelFormat::Gray16 => 16,
            PixelFormat::Yuv420P10 => 15,
            PixelFormat::Yuv420P12 => 18,
            PixelFormat::None => 0,
        }
    }

    /// Number of planes.
    pub fn planes(self) -> usize {
        match self {
            PixelFormat::Yuv420P | PixelFormat::Yuv422P | PixelFormat::Yuv444P
            | PixelFormat::Yuv420P10 | PixelFormat::Yuv420P12 => 3,
            PixelFormat::Nv12 | PixelFormat::Nv21 => 2,
            _ => 1,
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "yuv420p" => Some(PixelFormat::Yuv420P),
            "yuv422p" => Some(PixelFormat::Yuv422P),
            "yuv444p" => Some(PixelFormat::Yuv444P),
            "nv12" => Some(PixelFormat::Nv12),
            "nv21" => Some(PixelFormat::Nv21),
            "rgb24" => Some(PixelFormat::Rgb24),
            "bgr24" => Some(PixelFormat::Bgr24),
            "rgba" => Some(PixelFormat::Rgba),
            "bgra" => Some(PixelFormat::Bgra),
            "argb" => Some(PixelFormat::Argb),
            "gray8" => Some(PixelFormat::Gray8),
            "gray16" => Some(PixelFormat::Gray16),
            _ => None,
        }
    }
}
```

- [ ] **Step 2: Write sample_format.rs**

```rust
use serde::{Deserialize, Serialize};

/// Audio sample format, equivalent to FFmpeg's AVSampleFormat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SampleFormat {
    /// Unsigned 8-bit
    U8,
    /// Signed 16-bit
    S16,
    /// Signed 32-bit
    S32,
    /// 32-bit float
    F32,
    /// 64-bit float
    F64,
    /// Signed 16-bit planar
    S16P,
    /// Signed 32-bit planar
    S32P,
    /// 32-bit float planar
    F32P,
    /// 64-bit float planar
    F64P,
    /// None / unknown
    None,
}

impl SampleFormat {
    pub fn name(self) -> &'static str {
        match self {
            SampleFormat::U8 => "u8",
            SampleFormat::S16 => "s16",
            SampleFormat::S32 => "s32",
            SampleFormat::F32 => "f32",
            SampleFormat::F64 => "f64",
            SampleFormat::S16P => "s16p",
            SampleFormat::S32P => "s32p",
            SampleFormat::F32P => "f32p",
            SampleFormat::F64P => "f64p",
            SampleFormat::None => "none",
        }
    }

    /// Bytes per sample.
    pub fn bytes(self) -> usize {
        match self {
            SampleFormat::U8 => 1,
            SampleFormat::S16 | SampleFormat::S16P => 2,
            SampleFormat::S32 | SampleFormat::F32 | SampleFormat::S32P | SampleFormat::F32P => 4,
            SampleFormat::F64 | SampleFormat::F64P => 8,
            SampleFormat::None => 0,
        }
    }

    /// Whether the format is planar.
    pub fn is_planar(self) -> bool {
        matches!(self, SampleFormat::S16P | SampleFormat::S32P | SampleFormat::F32P | SampleFormat::F64P)
    }
}
```

- [ ] **Step 3: Write channel_layout.rs**

```rust
use serde::{Deserialize, Serialize};

bitflags::bitflags! {
    /// Audio channel layout, equivalent to FFmpeg's AVChannelLayout.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub struct ChannelLayout: u64 {
        const FRONT_LEFT      = 1 << 0;
        const FRONT_RIGHT     = 1 << 1;
        const FRONT_CENTER    = 1 << 2;
        const LOW_FREQUENCY   = 1 << 3;
        const BACK_LEFT       = 1 << 4;
        const BACK_RIGHT      = 1 << 5;
        const FRONT_LEFT_OF_CENTER  = 1 << 6;
        const FRONT_RIGHT_OF_CENTER = 1 << 7;
        const BACK_CENTER     = 1 << 8;
        const SIDE_LEFT       = 1 << 9;
        const SIDE_RIGHT      = 1 << 10;

        const MONO            = Self::FRONT_CENTER.bits();
        const STEREO          = Self::FRONT_LEFT.bits() | Self::FRONT_RIGHT.bits();
        const SURROUND        = Self::STEREO.bits() | Self::FRONT_CENTER.bits();
        const _5POINT1        = Self::SURROUND.bits() | Self::BACK_LEFT.bits()
                              | Self::BACK_RIGHT.bits() | Self::LOW_FREQUENCY.bits();
        const _7POINT1        = Self::_5POINT1.bits() | Self::SIDE_LEFT.bits()
                              | Self::SIDE_RIGHT.bits();
    }
}

impl ChannelLayout {
    pub fn name(self) -> &'static str {
        match self {
            x if x == ChannelLayout::MONO => "mono",
            x if x == ChannelLayout::STEREO => "stereo",
            x if x == ChannelLayout::SURROUND => "surround",
            x if x == ChannelLayout::_5POINT1 => "5.1",
            x if x == ChannelLayout::_7POINT1 => "7.1",
            _ => "unknown",
        }
    }

    /// Number of channels.
    pub fn channels(self) -> usize {
        self.bits().count_ones() as usize
    }
}
```

- [ ] **Step 4: Write dict.rs**

```rust
use std::collections::HashMap;

/// Metadata dictionary, equivalent to FFmpeg's AVDictionary.
#[derive(Debug, Clone, PartialEq)]
pub struct Dict {
    entries: HashMap<String, String>,
}

impl Dict {
    pub fn new() -> Self {
        Dict {
            entries: HashMap::new(),
        }
    }

    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.entries.insert(key.into(), value.into());
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries.get(key).map(|s| s.as_str())
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for Dict {
    fn default() -> Self {
        Dict::new()
    }
}

impl FromIterator<(String, String)> for Dict {
    fn from_iter<I: IntoIterator<Item = (String, String)>>(iter: I) -> Self {
        Dict {
            entries: iter.into_iter().collect(),
        }
    }
}
```

- [ ] **Step 5: Run tests**

Run: `cd D:\rsmpeg && cargo test -p rsmpeg-util`
Expected: All tests pass

### Task 4: Create rsmpeg-codec crate — codec_id, packet, frame, picture_type

**Files:**
- Create: `D:\rsmpeg\rsmpeg-codec\Cargo.toml`
- Create: `D:\rsmpeg\rsmpeg-codec\src\lib.rs`
- Create: `D:\rsmpeg\rsmpeg-codec\src\codec_id.rs`
- Create: `D:\rsmpeg\rsmpeg-codec\src\packet.rs`
- Create: `D:\rsmpeg\rsmpeg-codec\src\frame.rs`
- Create: `D:\rsmpeg\rsmpeg-codec\src\picture_type.rs`

- [ ] **Step 1: Create Cargo.toml**

```toml
[package]
name = "rsmpeg-codec"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true
description = "Codec interface and types (libavcodec equivalent)"

[lints]
workspace = true

[dependencies]
rsmpeg-util = { path = "../rsmpeg-util" }
bytes.workspace = true
bitflags.workspace = true
tracing.workspace = true
serde.workspace = true
```

- [ ] **Step 2: Create lib.rs**

```rust
#![forbid(unsafe_code)]

pub mod codec_id;
pub mod packet;
pub mod frame;
pub mod picture_type;
pub mod codec;
pub mod codec_registry;
pub mod codec_context;
pub mod codec_parameters;

pub use codec_id::CodecId;
pub use packet::Packet;
pub use frame::Frame;
pub use picture_type::PictureType;
pub use codec::Codec;
pub use codec_registry::CodecRegistry;
pub use codec_context::CodecContext;
pub use codec_parameters::CodecParameters;
```

- [ ] **Step 3: Write codec_id.rs**

```rust
use serde::{Deserialize, Serialize};

/// Codec identifier, equivalent to FFmpeg's AVCodecID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CodecId {
    // ── Video ──────────────────────────────────────────────
    Av1,
    Vp9,
    Vp8,
    Theora,
    Mpeg4,
    H263,
    Mjpeg,
    Ffv1,
    JpegXl,
    // ── Audio ──────────────────────────────────────────────
    Opus,
    Vorbis,
    Flac,
    Mp3,
    /// Raw PCM (various formats)
    Pcm,
    Alac,
    // ── Image ──────────────────────────────────────────────
    Png,
    Gif,
    WebP,
    Bmp,
    // ── Subtitle ────────────────────────────────────────────
    Srt,
    WebVtt,
    // ── Unknown ─────────────────────────────────────────────
    Unknown,
}

impl CodecId {
    pub fn name(self) -> &'static str {
        match self {
            CodecId::Av1 => "av1",
            CodecId::Vp9 => "vp9",
            CodecId::Vp8 => "vp8",
            CodecId::Theora => "theora",
            CodecId::Mpeg4 => "mpeg4",
            CodecId::H263 => "h263",
            CodecId::Mjpeg => "mjpeg",
            CodecId::Ffv1 => "ffv1",
            CodecId::JpegXl => "jpegxl",
            CodecId::Opus => "opus",
            CodecId::Vorbis => "vorbis",
            CodecId::Flac => "flac",
            CodecId::Mp3 => "mp3",
            CodecId::Pcm => "pcm",
            CodecId::Alac => "alac",
            CodecId::Png => "png",
            CodecId::Gif => "gif",
            CodecId::WebP => "webp",
            CodecId::Bmp => "bmp",
            CodecId::Srt => "srt",
            CodecId::WebVtt => "webvtt",
            CodecId::Unknown => "unknown",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "av1" => Some(CodecId::Av1),
            "vp9" => Some(CodecId::Vp9),
            "vp8" => Some(CodecId::Vp8),
            "theora" => Some(CodecId::Theora),
            "mpeg4" => Some(CodecId::Mpeg4),
            "h263" => Some(CodecId::H263),
            "mjpeg" => Some(CodecId::Mjpeg),
            "ffv1" => Some(CodecId::Ffv1),
            "jpegxl" => Some(CodecId::JpegXl),
            "opus" => Some(CodecId::Opus),
            "vorbis" => Some(CodecId::Vorbis),
            "flac" => Some(CodecId::Flac),
            "mp3" => Some(CodecId::Mp3),
            "pcm" => Some(CodecId::Pcm),
            "alac" => Some(CodecId::Alac),
            "png" => Some(CodecId::Png),
            "gif" => Some(CodecId::Gif),
            "webp" => Some(CodecId::WebP),
            "bmp" => Some(CodecId::Bmp),
            "srt" => Some(CodecId::Srt),
            "webvtt" => Some(CodecId::WebVtt),
            _ => None,
        }
    }

    /// Codec media type.
    pub fn media_type(self) -> rsmpeg_util::MediaType {
        match self {
            CodecId::Av1 | CodecId::Vp9 | CodecId::Vp8 | CodecId::Theora
            | CodecId::Mpeg4 | CodecId::H263 | CodecId::Mjpeg | CodecId::Ffv1
            | CodecId::JpegXl => rsmpeg_util::MediaType::Video,
            CodecId::Opus | CodecId::Vorbis | CodecId::Flac | CodecId::Mp3
            | CodecId::Pcm | CodecId::Alac => rsmpeg_util::MediaType::Audio,
            CodecId::Png | CodecId::Gif | CodecId::WebP | CodecId::Bmp => rsmpeg_util::MediaType::Video,
            CodecId::Srt | CodecId::WebVtt => rsmpeg_util::MediaType::Subtitle,
            CodecId::Unknown => rsmpeg_util::MediaType::Data,
        }
    }
}
```

- [ ] **Step 4: Write picture_type.rs**

```rust
use serde::{Deserialize, Serialize};

/// Picture type, equivalent to FFmpeg's AVPictureType.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PictureType {
    None,
    /// Intra-coded (I-frame)
    I,
    /// Predictive-coded (P-frame)
    P,
    /// Bi-directionally predictive (B-frame)
    B,
    /// Switching Intra (SI-frame)
    Si,
    /// Switching Predictive (SP-frame)
    Sp,
}

impl PictureType {
    pub fn name(self) -> &'static str {
        match self {
            PictureType::None => "none",
            PictureType::I => "I",
            PictureType::P => "P",
            PictureType::B => "B",
            PictureType::Si => "SI",
            PictureType::Sp => "SP",
        }
    }
}
```

- [ ] **Step 5: Write packet.rs**

```rust
use bytes::Bytes;
use rsmpeg_util::Rational;

bitflags::bitflags! {
    /// Packet flags, equivalent to FFmpeg's AV_PKT_FLAG_*.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct PacketFlags: u8 {
        /// The packet contains a keyframe.
        const KEY = 1 << 0;
        /// The packet content is corrupted.
        const CORRUPT = 1 << 1;
        /// The packet should be discarded.
        const DISCARD = 1 << 2;
        /// The packet data is trusted.
        const TRUSTED = 1 << 3;
    }
}

/// Compressed media data packet, equivalent to FFmpeg's AVPacket.
#[derive(Debug, Clone)]
pub struct Packet {
    /// Compressed data.
    pub data: Bytes,
    /// Presentation timestamp (in stream timebase).
    pub pts: Option<i64>,
    /// Decoding timestamp (in stream timebase).
    pub dts: Option<i64>,
    /// Duration (in stream timebase).
    pub duration: i64,
    /// Stream index this packet belongs to.
    pub stream_index: usize,
    /// Packet flags.
    pub flags: PacketFlags,
    /// Byte position in file, -1 if unknown.
    pub pos: i64,
    /// Stream time base.
    pub time_base: Rational,
}

impl Packet {
    pub fn new(data: Bytes, stream_index: usize) -> Self {
        Packet {
            data,
            pts: None,
            dts: None,
            duration: 0,
            stream_index,
            flags: PacketFlags::empty(),
            pos: -1,
            time_base: Rational::new(1, 1000),
        }
    }

    /// Presentation timestamp in seconds.
    pub fn pts_seconds(&self) -> Option<f64> {
        self.pts.map(|pts| pts as f64 * self.time_base.to_f64())
    }

    /// Duration in seconds.
    pub fn duration_seconds(&self) -> f64 {
        self.duration as f64 * self.time_base.to_f64()
    }

    pub fn is_key(&self) -> bool {
        self.flags.contains(PacketFlags::KEY)
    }
}
```

- [ ] **Step 6: Write frame.rs**

```rust
use rsmpeg_util::{PixelFormat, SampleFormat, Rational};

/// Uncompressed media frame, equivalent to FFmpeg's AVFrame.
#[derive(Debug, Clone)]
pub struct Frame {
    /// Plane data.
    pub data: Vec<Vec<u8>>,
    /// Bytes per line for each plane.
    pub linesize: Vec<usize>,
    /// Width (video).
    pub width: usize,
    /// Height (video).
    pub height: usize,
    /// Pixel format (video).
    pub pixel_format: PixelFormat,
    /// Sample format (audio).
    pub sample_format: SampleFormat,
    /// Sample rate (audio).
    pub sample_rate: u32,
    /// Number of channels (audio).
    pub channels: u16,
    /// Samples per channel (audio).
    pub samples: usize,
    /// Presentation timestamp.
    pub pts: Option<i64>,
    /// Duration in stream timebase.
    pub duration: i64,
    /// Frame time base.
    pub time_base: Rational,
    /// Whether this is a key frame.
    pub key_frame: bool,
    /// Picture type.
    pub pict_type: super::PictureType,
}

impl Frame {
    /// Create a new video frame.
    pub fn new_video(width: usize, height: usize, pix_fmt: PixelFormat) -> Self {
        let planes = pix_fmt.planes();
        let mut data = Vec::with_capacity(planes);
        let mut linesize = Vec::with_capacity(planes);

        // Simple allocation based on pixel format
        let total_pixels = width * height;
        for _ in 0..planes {
            let plane_size = total_pixels; // rough estimate
            data.push(vec![0u8; plane_size]);
            linesize.push(width);
        }

        Frame {
            data,
            linesize,
            width,
            height,
            pixel_format: pix_fmt,
            sample_format: SampleFormat::None,
            sample_rate: 0,
            channels: 0,
            samples: 0,
            pts: None,
            duration: 0,
            time_base: Rational::new(1, 1000),
            key_frame: false,
            pict_type: super::PictureType::None,
        }
    }

    /// Create a new audio frame.
    pub fn new_audio(sample_format: SampleFormat, sample_rate: u32, channels: u16, samples: usize) -> Self {
        let bytes_per_sample = sample_format.bytes();
        let total_bytes = samples as usize * channels as usize * bytes_per_sample;
        let data = vec![vec![0u8; total_bytes]];

        Frame {
            data,
            linesize: vec![total_bytes],
            width: 0,
            height: 0,
            pixel_format: PixelFormat::None,
            sample_format,
            sample_rate,
            channels,
            samples,
            pts: None,
            duration: 0,
            time_base: Rational::new(1, sample_rate as i32),
            key_frame: true,
            pict_type: super::PictureType::I,
        }
    }
}
```

- [ ] **Step 7: Build and test**

Run: `cd D:\rsmpeg && cargo build -p rsmpeg-codec`
Expected: Build succeeds

### Task 5: Implement rsmpeg-codec — Codec trait, CodecRegistry, CodecParameters, CodecContext

**Files:**
- Create: `D:\rsmpeg\rsmpeg-codec\src\codec.rs`
- Create: `D:\rsmpeg\rsmpeg-codec\src\codec_registry.rs`
- Create: `D:\rsmpeg\rsmpeg-codec\src\codec_context.rs`
- Create: `D:\rsmpeg\rsmpeg-codec\src\codec_parameters.rs`

- [ ] **Step 1: Write codec.rs**

```rust
use rsmpeg_util::{MediaType, RsResult};
use crate::codec_id::CodecId;
use crate::frame::Frame;
use crate::packet::Packet;

/// Capabilities of a codec.
#[derive(Debug, Clone)]
pub struct CodecCapabilities {
    /// Whether this codec can decode.
    pub can_decode: bool,
    /// Whether this codec can encode.
    pub can_encode: bool,
    /// Whether this codec is lossless.
    pub lossless: bool,
    /// Whether this codec supports intra-only encoding.
    pub intra_only: bool,
}

/// Codec trait — all codec implementations implement this.
///
/// Equivalent to FFmpeg's AVCodec.
pub trait Codec: Send + Sync {
    /// Unique codec identifier.
    fn id(&self) -> CodecId;
    /// Media type (video, audio, subtitle).
    fn media_type(&self) -> MediaType;
    /// Short name (e.g., "av1").
    fn name(&self) -> &'static str;
    /// Human-readable name (e.g., "AV1 (Alliance for Open Media)").
    fn long_name(&self) -> &'static str;
    /// Codec capabilities.
    fn capabilities(&self) -> CodecCapabilities;
}

/// Decoder trait extends Codec with decode capability.
pub trait Decoder: Codec {
    /// Decode a packet into frames.
    ///
    /// May return multiple frames (e.g., after a keyframe) or zero frames
    /// (if more data is needed).
    fn decode(&mut self, packet: &Packet) -> RsResult<Vec<Frame>>;

    /// Flush remaining frames at end of stream.
    fn flush(&mut self) -> RsResult<Vec<Frame>> {
        Ok(Vec::new())
    }
}

/// Encoder trait extends Codec with encode capability.
pub trait Encoder: Codec {
    /// Encode a frame into packets.
    ///
    /// May return zero or more packets.
    fn encode(&mut self, frame: &Frame) -> RsResult<Vec<Packet>>;

    /// Flush remaining packets at end of stream.
    fn flush(&mut self) -> RsResult<Vec<Packet>> {
        Ok(Vec::new())
    }
}
```

- [ ] **Step 2: Write codec_registry.rs**

```rust
use std::sync::RwLock;
use crate::codec::Codec;
use crate::codec_id::CodecId;

/// Global registry of available codecs.
///
/// Equivalent to FFmpeg's avcodec_register_all().
pub struct CodecRegistry {
    codecs: Vec<Box<dyn Codec>>,
}

impl CodecRegistry {
    pub fn new() -> Self {
        CodecRegistry { codecs: Vec::new() }
    }

    /// Register a codec.
    pub fn register(&mut self, codec: Box<dyn Codec>) {
        tracing::debug!("Registering codec: {}", codec.name());
        self.codecs.push(codec);
    }

    /// Find a codec by its CodecId.
    pub fn find_by_id(&self, id: CodecId) -> Option<&dyn Codec> {
        self.codecs.iter().find(|c| c.id() == id).map(|c| c.as_ref())
    }

    /// Find a codec by name.
    pub fn find_by_name(&self, name: &str) -> Option<&dyn Codec> {
        self.codecs.iter().find(|c| c.name() == name).map(|c| c.as_ref())
    }

    /// List all registered codecs.
    pub fn list(&self) -> Vec<&dyn Codec> {
        self.codecs.iter().map(|c| c.as_ref()).collect()
    }

    /// Number of registered codecs.
    pub fn len(&self) -> usize {
        self.codecs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.codecs.is_empty()
    }
}

impl Default for CodecRegistry {
    fn default() -> Self {
        CodecRegistry::new()
    }
}

use std::sync::OnceLock;

/// Global codec registry (lazily initialized).
pub fn global_codec_registry() -> &'static RwLock<CodecRegistry> {
    static REGISTRY: OnceLock<RwLock<CodecRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(CodecRegistry::new()))
}

/// Initialize the global codec registry with built-in codecs.
pub fn register_builtin_codecs() {
    let mut reg = global_codec_registry().write().unwrap();
    // Built-in codecs will be registered here as they are implemented.
    // For now the registry is empty — codecs can be registered dynamically.
    tracing::info!("Built-in codec registration — no codecs registered yet");
}
```

- [ ] **Step 3: Write codec_parameters.rs**

```rust
use rsmpeg_util::{MediaType, PixelFormat, SampleFormat, Rational};
use crate::codec_id::CodecId;

/// Codec parameters describing a media stream, equivalent to FFmpeg's AVCodecParameters.
#[derive(Debug, Clone)]
pub struct CodecParameters {
    /// Codec identifier.
    pub codec_id: CodecId,
    /// Media type.
    pub media_type: MediaType,
    /// Video width.
    pub width: Option<usize>,
    /// Video height.
    pub height: Option<usize>,
    /// Video pixel format.
    pub pixel_format: Option<PixelFormat>,
    /// Audio sample rate.
    pub sample_rate: Option<u32>,
    /// Audio channels.
    pub channels: Option<u16>,
    /// Audio sample format.
    pub sample_format: Option<SampleFormat>,
    /// Bit rate (bits per second).
    pub bit_rate: Option<u64>,
    /// Codec-specific extradata (e.g., AV1 OBU sequence header).
    pub extradata: Option<Vec<u8>>,
}

impl CodecParameters {
    pub fn new(codec_id: CodecId) -> Self {
        CodecParameters {
            media_type: codec_id.media_type(),
            codec_id,
            width: None,
            height: None,
            pixel_format: None,
            sample_rate: None,
            channels: None,
            sample_format: None,
            bit_rate: None,
            extradata: None,
        }
    }
}
```

- [ ] **Step 4: Write codec_context.rs**

```rust
use rsmpeg_util::{RsResult, RsError, MediaType, PixelFormat, SampleFormat, Rational};
use crate::codec_id::CodecId;
use crate::codec::Codec;
use crate::codec::Decoder;
use crate::codec::Encoder;
use crate::codec_parameters::CodecParameters;
use crate::packet::Packet;
use crate::frame::Frame;
use crate::codec_registry::global_codec_registry;

/// CodecContext — codec instance configuration (Builder pattern).
///
/// Equivalent to FFmpeg's AVCodecContext.
pub struct CodecContext {
    /// Codec ID.
    codec_id: CodecId,
    /// The actual codec implementation (lazily resolved).
    codec: Option<Box<dyn Codec>>,
    /// Video width.
    width: Option<usize>,
    /// Video height.
    height: Option<usize>,
    /// Pixel format.
    pixel_format: Option<PixelFormat>,
    /// Audio sample rate.
    sample_rate: Option<u32>,
    /// Audio channels.
    channels: Option<u16>,
    /// Audio sample format.
    sample_format: Option<SampleFormat>,
    /// Bit rate.
    bit_rate: Option<u64>,
    /// Time base.
    time_base: Rational,
}

impl CodecContext {
    pub fn builder() -> CodecContextBuilder {
        CodecContextBuilder::new()
    }

    /// Open the codec by finding and initializing it.
    pub fn open(&mut self) -> RsResult<()> {
        let registry = global_codec_registry().read().map_err(|_| {
            RsError::Bug("codec registry lock poisoned".into())
        })?;
        let codec = registry.find_by_id(self.codec_id).ok_or_else(|| {
            RsError::NotFound(format!("Codec not found: {:?}", self.codec_id).into())
        })?;
        tracing::debug!("Opening codec: {}", codec.name());
        // In a full implementation, we'd clone/initialize the codec here.
        // For the skeleton, we just acknowledge the codec exists.
        self.codec = None; // Real impl would clone from the registry
        Ok(())
    }

    /// Decode a packet into frames.
    pub fn decode(&mut self, packet: &Packet) -> RsResult<Vec<Frame>> {
        // Skeleton: returns an empty frame list
        // Real implementation would delegate to the Decoder trait
        tracing::trace!("CodecContext::decode (skeleton) — {} bytes, stream {}", 
            packet.data.len(), packet.stream_index);
        Ok(Vec::new())
    }

    /// Encode a frame into packets.
    pub fn encode(&mut self, frame: &Frame) -> RsResult<Vec<Packet>> {
        tracing::trace!("CodecContext::encode (skeleton)");
        Ok(Vec::new())
    }

    // ── Getters ──────────────────────────────────────────────────
    pub fn codec_id(&self) -> CodecId { self.codec_id }
    pub fn width(&self) -> Option<usize> { self.width }
    pub fn height(&self) -> Option<usize> { self.height }
    pub fn pixel_format(&self) -> Option<PixelFormat> { self.pixel_format }
    pub fn sample_rate(&self) -> Option<u32> { self.sample_rate }
    pub fn channels(&self) -> Option<u16> { self.channels }
    pub fn bit_rate(&self) -> Option<u64> { self.bit_rate }
    pub fn time_base(&self) -> Rational { self.time_base }
}

/// Builder for CodecContext.
pub struct CodecContextBuilder {
    codec_id: CodecId,
    width: Option<usize>,
    height: Option<usize>,
    pixel_format: Option<PixelFormat>,
    sample_rate: Option<u32>,
    channels: Option<u16>,
    sample_format: Option<SampleFormat>,
    bit_rate: Option<u64>,
    time_base: Rational,
}

impl CodecContextBuilder {
    pub fn new() -> Self {
        CodecContextBuilder {
            codec_id: CodecId::Unknown,
            width: None,
            height: None,
            pixel_format: None,
            sample_rate: None,
            channels: None,
            sample_format: None,
            bit_rate: None,
            time_base: Rational::new(1, 1000),
        }
    }

    pub fn codec_id(mut self, id: CodecId) -> Self { self.codec_id = id; self }
    pub fn width(mut self, w: usize) -> Self { self.width = Some(w); self }
    pub fn height(mut self, h: usize) -> Self { self.height = Some(h); self }
    pub fn pixel_format(mut self, fmt: PixelFormat) -> Self { self.pixel_format = Some(fmt); self }
    pub fn sample_rate(mut self, sr: u32) -> Self { self.sample_rate = Some(sr); self }
    pub fn channels(mut self, ch: u16) -> Self { self.channels = Some(ch); self }
    pub fn sample_format(mut self, fmt: SampleFormat) -> Self { self.sample_format = Some(fmt); self }
    pub fn bit_rate(mut self, br: u64) -> Self { self.bit_rate = Some(br); self }
    pub fn time_base(mut self, tb: Rational) -> Self { self.time_base = tb; self }

    pub fn build(self) -> CodecContext {
        CodecContext {
            codec_id: self.codec_id,
            codec: None,
            width: self.width,
            height: self.height,
            pixel_format: self.pixel_format,
            sample_rate: self.sample_rate,
            channels: self.channels,
            sample_format: self.sample_format,
            bit_rate: self.bit_rate,
            time_base: self.time_base,
        }
    }
}

impl Default for CodecContextBuilder {
    fn default() -> Self { Self::new() }
}
```

- [ ] **Step 5: Build and test**

Run: `cd D:\rsmpeg && cargo build -p rsmpeg-codec`
Expected: Build succeeds

### Task 6: Create rsmpeg-format crate — IOContext, Stream, ProbeScore, format traits

**Files:**
- Create: `D:\rsmpeg\rsmpeg-format\Cargo.toml`
- Create: `D:\rsmpeg\rsmpeg-format\src\lib.rs`
- Create: `D:\rsmpeg\rsmpeg-format\src\io_context.rs`
- Create: `D:\rsmpeg\rsmpeg-format\src\stream.rs`
- Create: `D:\rsmpeg\rsmpeg-format\src\probe.rs`
- Create: `D:\rsmpeg\rsmpeg-format\src\format.rs`
- Create: `D:\rsmpeg\rsmpeg-format\src\format_registry.rs`
- Create: `D:\rsmpeg\rsmpeg-format\src\format_context.rs`

- [ ] **Step 1: Create Cargo.toml**

```toml
[package]
name = "rsmpeg-format"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true
description = "Container format demuxing/muxing (libavformat equivalent)"

[lints]
workspace = true

[dependencies]
rsmpeg-util = { path = "../rsmpeg-util" }
rsmpeg-codec = { path = "../rsmpeg-codec" }
bytes.workspace = true
tracing.workspace = true
serde.workspace = true
serde_json.workspace = true
```

- [ ] **Step 2: Create lib.rs**

```rust
#![forbid(unsafe_code)]

pub mod io_context;
pub mod stream;
pub mod probe;
pub mod format;
pub mod format_registry;
pub mod format_context;

pub use io_context::IOContext;
pub use stream::Stream;
pub use probe::{ProbeScore, probe_format};
pub use format::{InputFormat, OutputFormat};
pub use format_registry::FormatRegistry;
pub use format_context::FormatContext;
```

- [ ] **Step 3: Write io_context.rs**

```rust
use std::io::{Read, Seek, SeekFrom};
use std::fs::File;
use std::path::Path;
use rsmpeg_util::RsResult;

/// I/O abstraction for media file access, equivalent to FFmpeg's AVIOContext.
pub enum IOContext {
    File(File),
    /// Memory buffer
    Buffer(std::io::Cursor<Vec<u8>>),
}

impl IOContext {
    /// Open a file for reading.
    pub fn open_file(path: impl AsRef<Path>) -> RsResult<Self> {
        let file = File::open(path.as_ref())?;
        Ok(IOContext::File(file))
    }

    /// Create from byte buffer.
    pub fn from_buffer(data: Vec<u8>) -> Self {
        IOContext::Buffer(std::io::Cursor::new(data))
    }

    /// Read bytes into buffer.
    pub fn read_exact(&mut self, buf: &mut [u8]) -> RsResult<()> {
        match self {
            IOContext::File(f) => f.read_exact(buf)?,
            IOContext::Buffer(c) => c.read_exact(buf)?,
        }
        Ok(())
    }

    /// Read a single byte.
    pub fn read_u8(&mut self) -> RsResult<u8> {
        let mut buf = [0u8; 1];
        self.read_exact(&mut buf)?;
        Ok(buf[0])
    }

    /// Read a big-endian 16-bit unsigned integer.
    pub fn read_u16_be(&mut self) -> RsResult<u16> {
        let mut buf = [0u8; 2];
        self.read_exact(&mut buf)?;
        Ok(u16::from_be_bytes(buf))
    }

    /// Read a big-endian 32-bit unsigned integer.
    pub fn read_u32_be(&mut self) -> RsResult<u32> {
        let mut buf = [0u8; 4];
        self.read_exact(&mut buf)?;
        Ok(u32::from_be_bytes(buf))
    }

    /// Read a 64-bit big-endian integer.
    pub fn read_u64_be(&mut self) -> RsResult<u64> {
        let mut buf = [0u8; 8];
        self.read_exact(&mut buf)?;
        Ok(u64::from_be_bytes(buf))
    }

    /// Read a 32-bit little-endian integer.
    pub fn read_u32_le(&mut self) -> RsResult<u32> {
        let mut buf = [0u8; 4];
        self.read_exact(&mut buf)?;
        Ok(u32::from_le_bytes(buf))
    }

    /// Seek to a position.
    pub fn seek(&mut self, pos: SeekFrom) -> RsResult<u64> {
        match self {
            IOContext::File(f) => Ok(f.seek(pos)?),
            IOContext::Buffer(c) => Ok(c.seek(pos)?),
        }
    }

    /// Get current position.
    pub fn tell(&mut self) -> RsResult<u64> {
        self.seek(SeekFrom::Current(0))
    }

    /// Read a fixed number of bytes into a vector.
    pub fn read_bytes(&mut self, len: usize) -> RsResult<Vec<u8>> {
        let mut buf = vec![0u8; len];
        self.read_exact(&mut buf)?;
        Ok(buf)
    }

    /// Peek at bytes without advancing position.
    pub fn peek(&mut self, len: usize) -> RsResult<Vec<u8>> {
        let pos = self.tell()?;
        let buf = self.read_bytes(len)?;
        self.seek(SeekFrom::Start(pos))?;
        Ok(buf)
    }
}
```

- [ ] **Step 4: Write stream.rs**

```rust
use rsmpeg_util::{MediaType, Rational, Dict};
use rsmpeg_codec::{CodecId, CodecParameters};

/// Media stream descriptor, equivalent to FFmpeg's AVStream.
#[derive(Debug, Clone)]
pub struct Stream {
    /// Stream index in the file.
    pub index: usize,
    /// Codec used by this stream.
    pub codec_id: CodecId,
    /// Media type.
    pub media_type: MediaType,
    /// Codec parameters.
    pub codec_params: CodecParameters,
    /// Stream time base.
    pub time_base: Rational,
    /// Duration in stream time base units.
    pub duration: i64,
    /// Metadata.
    pub metadata: Dict,
    /// Average frame rate.
    pub avg_frame_rate: Rational,
    /// Real base frame rate.
    pub r_frame_rate: Rational,
}

impl Stream {
    pub fn new(index: usize, codec_id: CodecId) -> Self {
        Stream {
            index,
            codec_id,
            media_type: codec_id.media_type(),
            codec_params: CodecParameters::new(codec_id),
            time_base: Rational::new(1, 1000),
            duration: 0,
            metadata: Dict::new(),
            avg_frame_rate: Rational::new(0, 1),
            r_frame_rate: Rational::new(0, 1),
        }
    }

    /// Duration in seconds.
    pub fn duration_seconds(&self) -> f64 {
        self.duration as f64 * self.time_base.to_f64()
    }
}
```

- [ ] **Step 5: Write probe.rs**

```rust
use rsmpeg_util::RsResult;

/// Format detection confidence score.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProbeScore {
    /// No match.
    NoMatch = 0,
    /// Weak signal.
    Possible = 25,
    /// Reasonable confidence.
    Likely = 50,
    /// High confidence.
    VeryLikely = 75,
    /// Certain match (magic bytes matched).
    Certain = 100,
}

/// Result of format probing.
#[derive(Debug, Clone)]
pub struct ProbeResult {
    /// Format short name (e.g., "mp4", "mkv").
    pub format_name: &'static str,
    /// Human-readable format description.
    pub description: &'static str,
    /// Confidence score.
    pub score: ProbeScore,
    /// File extension hint.
    pub extension: &'static str,
}

/// Detect container format from a buffer of initial bytes.
pub fn probe_format(buf: &[u8]) -> Vec<ProbeResult> {
    let mut results = Vec::new();

    // MP4/ISOBMFF: ftyp box
    if buf.len() >= 8 && &buf[4..8] == b"ftyp" {
        let brand = if buf.len() >= 12 { &buf[8..12] } else { b"" };
        let desc = match brand {
            b"isom" => "ISO Base Media (MP4)",
            b"mp42" => "MP4 v2",
            b"avc1" => "MP4 (H.264)",
            _ => "MP4/ISOBMFF",
        };
        results.push(ProbeResult {
            format_name: "mp4",
            description: desc,
            score: ProbeScore::Certain,
            extension: "mp4",
        });
    }

    // MKV/WebM: EBML header
    if buf.len() >= 4 && buf[0..4] == [0x1A, 0x45, 0xDF, 0xA3] {
        let is_webm = buf.len() > 20 && (
            buf[..20].windows(4).any(|w| w == b"webm")
        );
        if is_webm {
            results.push(ProbeResult {
                format_name: "webm",
                description: "WebM",
                score: ProbeScore::Certain,
                extension: "webm",
            });
        } else {
            results.push(ProbeResult {
                format_name: "mkv",
                description: "Matroska",
                score: ProbeScore::Certain,
                extension: "mkv",
            });
        }
    }

    // AVI: RIFF header
    if buf.len() >= 12 && &buf[0..4] == b"RIFF" && &buf[8..12] == b"AVI " {
        results.push(ProbeResult {
            format_name: "avi",
            description: "AVI (Audio Video Interleave)",
            score: ProbeScore::Certain,
            extension: "avi",
        });
    }

    // MPEG-TS: sync byte 0x47
    if buf.len() >= 192 {
        let sync_count = buf[..192].iter().filter(|&&b| b == 0x47).count();
        if sync_count > 5 {
            results.push(ProbeResult {
                format_name: "mpegts",
                description: "MPEG-TS (Transport Stream)",
                score: ProbeScore::VeryLikely,
                extension: "ts",
            });
        }
    }

    // OGG: capture pattern
    if buf.len() >= 4 && &buf[0..4] == b"OggS" {
        results.push(ProbeResult {
            format_name: "ogg",
            description: "OGG",
            score: ProbeScore::Certain,
            extension: "ogg",
        });
    }

    // FLAC: fLaC marker
    if buf.len() >= 4 && &buf[0..4] == b"fLaC" {
        results.push(ProbeResult {
            format_name: "flac",
            description: "FLAC (Free Lossless Audio Codec)",
            score: ProbeScore::Certain,
            extension: "flac",
        });
    }

    // WAV: RIFF WAVE
    if buf.len() >= 12 && &buf[0..4] == b"RIFF" && &buf[8..12] == b"WAVE" {
        results.push(ProbeResult {
            format_name: "wav",
            description: "WAV (Waveform Audio)",
            score: ProbeScore::Certain,
            extension: "wav",
        });
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_probe_mp4() {
        let mut buf = vec![0u8; 16];
        buf[4..8].copy_from_slice(b"ftyp");
        buf[8..12].copy_from_slice(b"isom");
        let results = probe_format(&buf);
        assert!(results.iter().any(|r| r.format_name == "mp4"));
    }

    #[test]
    fn test_probe_mkv() {
        let buf = vec![0x1A, 0x45, 0xDF, 0xA3, 0x00, 0x00, 0x00, 0x00];
        let results = probe_format(&buf);
        assert!(results.iter().any(|r| r.format_name == "mkv"));
    }

    #[test]
    fn test_probe_avi() {
        let mut buf = vec![0u8; 12];
        buf[0..4].copy_from_slice(b"RIFF");
        buf[8..12].copy_from_slice(b"AVI ");
        let results = probe_format(&buf);
        assert!(results.iter().any(|r| r.format_name == "avi"));
    }

    #[test]
    fn test_probe_unknown() {
        let buf = vec![0x00, 0x01, 0x02, 0x03];
        let results = probe_format(&buf);
        assert!(results.is_empty());
    }
}
```

- [ ] **Step 6: Write format.rs**

```rust
use rsmpeg_util::RsResult;
use crate::probe::ProbeScore;
use crate::format_context::FormatContext;
use crate::stream::Stream;
use rsmpeg_codec::Packet;

/// Demuxer trait, equivalent to FFmpeg's AVInputFormat.
pub trait InputFormat: Send + Sync {
    /// Short format name (e.g., "mp4", "mkv").
    fn name(&self) -> &'static str;
    /// Human-readable description.
    fn description(&self) -> &'static str;
    /// Common file extensions.
    fn extensions(&self) -> &'static [&'static str];
    /// Probe confidence for a byte buffer.
    fn probe(&self, buf: &[u8]) -> ProbeScore;
    /// Read the file header and populate stream information.
    fn read_header(&mut self, ctx: &mut FormatContext) -> RsResult<()>;
    /// Read the next packet from the file.
    fn read_frame(&mut self, ctx: &mut FormatContext) -> RsResult<Option<Packet>>;
    /// Seek to a timestamp.
    fn seek(&mut self, ctx: &mut FormatContext, timestamp: i64) -> RsResult<()>;
}

/// Muxer trait, equivalent to FFmpeg's AVOutputFormat.
pub trait OutputFormat: Send + Sync {
    /// Short format name.
    fn name(&self) -> &'static str;
    /// Human-readable description.
    fn description(&self) -> &'static str;
    /// Common file extensions.
    fn extensions(&self) -> &'static [&'static str];
    /// Write header.
    fn write_header(&mut self, ctx: &mut FormatContext) -> RsResult<()>;
    /// Write a packet.
    fn write_frame(&mut self, ctx: &mut FormatContext, packet: &Packet) -> RsResult<()>;
    /// Finalize the file.
    fn write_trailer(&mut self, ctx: &mut FormatContext) -> RsResult<()>;
}
```

- [ ] **Step 7: Write format_registry.rs**

```rust
use std::sync::OnceLock;
use std::sync::RwLock;
use crate::format::{InputFormat, OutputFormat};

/// Global registry of container formats.
pub struct FormatRegistry {
    demuxers: Vec<Box<dyn InputFormat>>,
    muxers: Vec<Box<dyn OutputFormat>>,
}

impl FormatRegistry {
    pub fn new() -> Self {
        FormatRegistry {
            demuxers: Vec::new(),
            muxers: Vec::new(),
        }
    }

    pub fn register_demuxer(&mut self, fmt: Box<dyn InputFormat>) {
        tracing::debug!("Registering demuxer: {}", fmt.name());
        self.demuxers.push(fmt);
    }

    pub fn register_muxer(&mut self, fmt: Box<dyn OutputFormat>) {
        tracing::debug!("Registering muxer: {}", fmt.name());
        self.muxers.push(fmt);
    }

    pub fn find_demuxer(&self, name: &str) -> Option<&dyn InputFormat> {
        self.demuxers.iter().find(|d| d.name() == name).map(|d| d.as_ref())
    }

    pub fn find_muxer(&self, name: &str) -> Option<&dyn OutputFormat> {
        self.muxers.iter().find(|m| m.name() == name).map(|m| m.as_ref())
    }

    pub fn demuxers(&self) -> Vec<&dyn InputFormat> {
        self.demuxers.iter().map(|d| d.as_ref()).collect()
    }

    pub fn muxers(&self) -> Vec<&dyn OutputFormat> {
        self.muxers.iter().map(|m| m.as_ref()).collect()
    }

    /// Find the best demuxer for a byte buffer via probing.
    pub fn probe_demuxer(&self, buf: &[u8]) -> Option<&dyn InputFormat> {
        self.demuxers.iter()
            .filter_map(|d| {
                let score = d.probe(buf);
                if score as u8 > 0 { Some((score, d.as_ref())) } else { None }
            })
            .max_by_key(|(score, _)| *score)
            .map(|(_, d)| d)
    }
}

impl Default for FormatRegistry {
    fn default() -> Self { Self::new() }
}

/// Global format registry.
pub fn global_format_registry() -> &'static RwLock<FormatRegistry> {
    static REGISTRY: OnceLock<RwLock<FormatRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(FormatRegistry::new()))
}

/// Register built-in format handlers.
pub fn register_builtin_formats() {
    let mut reg = global_format_registry().write().unwrap();
    tracing::info!("Built-in format registration — no formats registered yet");
}
```

- [ ] **Step 8: Write format_context.rs**

```rust
use rsmpeg_util::{RsResult, Dict};
use crate::io_context::IOContext;
use crate::stream::Stream;
use crate::format::{InputFormat, OutputFormat};
use crate::format_registry::global_format_registry;
use crate::probe::probe_format;
use rsmpeg_codec::{Packet, CodecId};
use std::path::Path;

/// FormatContext — demuxing/muxing context, equivalent to FFmpeg's AVFormatContext.
pub struct FormatContext {
    /// Input format (demuxer), if opened for input.
    pub input: Option<Box<dyn InputFormat>>,
    /// Output format (muxer), if opened for output.
    pub output: Option<Box<dyn OutputFormat>>,
    /// Media streams.
    pub streams: Vec<Stream>,
    /// I/O context.
    pub io: Option<IOContext>,
    /// File metadata.
    pub metadata: Dict,
    /// Duration in AV_TIME_BASE units (microseconds).
    pub duration: i64,
    /// File bit rate.
    pub bit_rate: u64,
    /// File path.
    pub filename: Option<String>,
    /// Format name.
    pub format_name: Option<String>,
}

impl FormatContext {
    /// Open a file for input (demuxing).
    pub fn open_input(path: impl AsRef<Path>) -> RsResult<Self> {
        let path = path.as_ref();
        let mut io = IOContext::open_file(path)?;

        // Peek initial bytes for format detection
        let probe_buf = io.peek(2048)?;
        let mut ctx = FormatContext {
            input: None,
            output: None,
            streams: Vec::new(),
            io: Some(io),
            metadata: Dict::new(),
            duration: 0,
            bit_rate: 0,
            filename: Some(path.to_string_lossy().to_string()),
            format_name: None,
        };

        // Try to find a matching demuxer
        let registry = global_format_registry().read()
            .map_err(|_| rsmpeg_util::RsError::Bug("format registry lock poisoned".into()))?;
        
        if let Some(demuxer) = registry.probe_demuxer(&probe_buf) {
            ctx.format_name = Some(demuxer.name().to_string());
            // Note: real impl would clone the demuxer
            ctx.input = None;
        }

        // Also probe via magic bytes for format name
        let probe_results = probe_format(&probe_buf);
        if let Some(best) = probe_results.first() {
            if ctx.format_name.is_none() {
                ctx.format_name = Some(best.format_name.to_string());
            }
        }

        Ok(ctx)
    }

    /// Open a file for output (muxing).
    pub fn open_output(path: impl AsRef<Path>) -> RsResult<Self> {
        let path = path.as_ref();
        let ext = path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        let registry = global_format_registry().read()
            .map_err(|_| rsmpeg_util::RsError::Bug("format registry lock poisoned".into()))?;

        let muxer = registry.find_muxer(&ext);

        Ok(FormatContext {
            input: None,
            output: None,
            streams: Vec::new(),
            io: None,
            metadata: Dict::new(),
            duration: 0,
            bit_rate: 0,
            filename: Some(path.to_string_lossy().to_string()),
            format_name: muxer.map(|m| m.name().to_string()),
        })
    }

    /// Read the next packet.
    pub fn read_frame(&mut self) -> RsResult<Option<Packet>> {
        if let Some(ref mut input) = self.input {
            input.read_frame(self)
        } else {
            // Skeleton: return None (no demuxer loaded)
            tracing::warn!("read_frame called but no input format is loaded");
            Ok(None)
        }
    }

    /// Write a packet.
    pub fn write_frame(&mut self, packet: &Packet) -> RsResult<()> {
        if let Some(ref mut output) = self.output {
            output.write_frame(self, packet)
        } else {
            tracing::warn!("write_frame called but no output format is loaded");
            Ok(())
        }
    }

    /// Add a stream.
    pub fn add_stream(&mut self, stream: Stream) {
        self.streams.push(stream);
    }

    /// Get number of streams.
    pub fn nb_streams(&self) -> usize {
        self.streams.len()
    }

    /// Find the best stream of a given media type.
    pub fn find_best_stream(&self, media_type: rsmpeg_util::MediaType) -> Option<usize> {
        self.streams.iter().position(|s| s.media_type == media_type)
    }
}
```

- [ ] **Step 9: Build and test**

Run: `cd D:\rsmpeg && cargo build -p rsmpeg-format`
Expected: Build succeeds

### Task 7: Create rsmpeg-filter crate

**Files:**
- Create: `D:\rsmpeg\rsmpeg-filter\Cargo.toml`
- Create: `D:\rsmpeg\rsmpeg-filter\src\lib.rs`
- Create: `D:\rsmpeg\rsmpeg-filter\src\filter.rs`
- Create: `D:\rsmpeg\rsmpeg-filter\src\pad.rs`
- Create: `D:\rsmpeg\rsmpeg-filter\src\filter_graph.rs`

- [ ] **Step 1: Create Cargo.toml**

```toml
[package]
name = "rsmpeg-filter"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true
description = "Filter graph pipeline (libavfilter equivalent)"

[lints]
workspace = true

[dependencies]
rsmpeg-util = { path = "../rsmpeg-util" }
rsmpeg-codec = { path = "../rsmpeg-codec" }
tracing.workspace = true
```

- [ ] **Step 2: Create lib.rs**

```rust
#![forbid(unsafe_code)]

pub mod filter;
pub mod pad;
pub mod filter_graph;

pub use filter::{Filter, FilterParams};
pub use pad::{FilterPad, BufferSrc, BufferSink};
pub use filter_graph::{FilterGraph, FilterNode, FilterEdge};
```

- [ ] **Step 3: Write filter.rs**

```rust
use rsmpeg_util::RsResult;
use rsmpeg_codec::Frame;

/// Parameters for a filter operation.
#[derive(Debug, Clone, Default)]
pub struct FilterParams {
    pub width: Option<usize>,
    pub height: Option<usize>,
    pub pixel_format: Option<rsmpeg_util::PixelFormat>,
    pub sample_rate: Option<u32>,
}

/// Filter trait — all filters implement this.
pub trait Filter: Send + Sync {
    /// Filter name (e.g., "scale", "trim", "volume").
    fn name(&self) -> &'static str;
    /// Human-readable description.
    fn description(&self) -> &'static str;
    /// Number of input pads.
    fn inputs(&self) -> usize { 1 }
    /// Number of output pads.
    fn outputs(&self) -> usize { 1 }
    /// Process input frames and produce output frames.
    fn process(&mut self, inputs: &[&Frame], params: &FilterParams) -> RsResult<Vec<Frame>>;
}

/// Scale filter — resize video frames.
pub struct ScaleFilter;

impl Filter for ScaleFilter {
    fn name(&self) -> &'static str { "scale" }
    fn description(&self) -> &'static str { "Resize video frames" }
    fn process(&mut self, inputs: &[&Frame], params: &FilterParams) -> RsResult<Vec<Frame>> {
        let src = inputs[0];
        let dst_w = params.width.unwrap_or(src.width);
        let dst_h = params.height.unwrap_or(src.height);

        if dst_w == src.width && dst_h == src.height {
            return Ok(vec![src.clone()]);
        }

        // Skeleton: return a blank frame at new size
        let mut frame = Frame::new_video(dst_w, dst_h, src.pixel_format);
        frame.pts = src.pts;
        frame.duration = src.duration;
        frame.time_base = src.time_base;
        Ok(vec![frame])
    }
}

/// Trim filter — select a segment of the timeline.
pub struct TrimFilter {
    pub start: f64,
    pub duration: f64,
    pts_offset: i64,
}

impl TrimFilter {
    pub fn new(start: f64, duration: f64) -> Self {
        TrimFilter { start, duration, pts_offset: 0 }
    }
}

impl Filter for TrimFilter {
    fn name(&self) -> &'static str { "trim" }
    fn description(&self) -> &'static str { "Select a segment of the timeline" }
    fn process(&mut self, inputs: &[&Frame], _params: &FilterParams) -> RsResult<Vec<Frame>> {
        let frame = inputs[0];
        let pts_s = frame.pts.unwrap_or(0) as f64 * frame.time_base.to_f64();
        let end = self.start + self.duration;

        if pts_s < self.start || pts_s >= end {
            return Ok(Vec::new()); // Drop this frame
        }

        let mut out = frame.clone();
        let adjusted = pts_s - self.start;
        if frame.time_base.den > 0 {
            out.pts = Some((adjusted / frame.time_base.to_f64()) as i64);
        }
        Ok(vec![out])
    }
}
```

- [ ] **Step 4: Write pad.rs**

```rust
use rsmpeg_util::MediaType;

/// Filter pad direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadDirection {
    Input,
    Output,
}

/// Description of a filter pad.
#[derive(Debug, Clone)]
pub struct FilterPad {
    pub name: &'static str,
    pub direction: PadDirection,
    pub media_type: MediaType,
}

/// Buffer source — injects frames into a filter graph (equivalent to FFmpeg's buffer source).
pub struct BufferSrc {
    pub name: String,
    pub media_type: MediaType,
}

/// Buffer sink — extracts frames from a filter graph (equivalent to FFmpeg's buffersink).
pub struct BufferSink {
    pub name: String,
    pub media_type: MediaType,
    pub frames: Vec<rsmpeg_codec::Frame>,
}

impl BufferSink {
    pub fn new(name: String, media_type: MediaType) -> Self {
        BufferSink { name, media_type, frames: Vec::new() }
    }

    /// Get all buffered frames.
    pub fn frames(&self) -> &[rsmpeg_codec::Frame] { &self.frames }

    /// Clear buffered frames.
    pub fn clear(&mut self) { self.frames.clear(); }
}
```

- [ ] **Step 5: Write filter_graph.rs**

```rust
use std::collections::HashMap;
use rsmpeg_util::RsResult;
use rsmpeg_codec::Frame;
use crate::filter::{Filter, FilterParams};
use crate::pad::BufferSink;

/// A node in the filter graph.
pub struct FilterNode {
    pub name: String,
    pub filter: Box<dyn Filter>,
    pub params: FilterParams,
}

/// An edge connecting filter pads.
pub struct FilterEdge {
    pub from_node: usize,
    pub from_pad: usize,
    pub to_node: usize,
    pub to_pad: usize,
}

/// FilterGraph — DAG of connected filters, equivalent to FFmpeg's AVFilterGraph.
pub struct FilterGraph {
    nodes: Vec<FilterNode>,
    edges: Vec<FilterEdge>,
    sinks: Vec<BufferSink>,
}

impl FilterGraph {
    pub fn new() -> Self {
        FilterGraph {
            nodes: Vec::new(),
            edges: Vec::new(),
            sinks: Vec::new(),
        }
    }

    /// Add a filter node to the graph.
    pub fn add_filter(&mut self, name: impl Into<String>, filter: Box<dyn Filter>, params: FilterParams) -> usize {
        let idx = self.nodes.len();
        self.nodes.push(FilterNode {
            name: name.into(),
            filter,
            params,
        });
        idx
    }

    /// Connect two filter nodes.
    pub fn connect(&mut self, from: usize, from_pad: usize, to: usize, to_pad: usize) {
        self.edges.push(FilterEdge { from_node: from, from_pad, to_node: to, to_pad });
    }

    /// Add a buffer sink to collect output.
    pub fn add_sink(&mut self, sink: BufferSink) -> usize {
        let idx = self.sinks.len();
        self.sinks.push(sink);
        idx
    }

    /// Get a mutable reference to a sink.
    pub fn sink_mut(&mut self, idx: usize) -> Option<&mut BufferSink> {
        self.sinks.get_mut(idx)
    }

    /// Get an immutable reference to a sink.
    pub fn sink(&self, idx: usize) -> Option<&BufferSink> {
        self.sinks.get(idx)
    }

    /// Process a frame through the filter graph.
    ///
    /// In a full implementation, this would perform topological sort
    /// and process DAG. For the skeleton, it passes through with optional scaling.
    pub fn process(&mut self, frame: &Frame, input_name: &str) -> RsResult<()> {
        // Simple linear chain processing
        let mut current = frame.clone();

        for node in self.nodes.iter_mut() {
            let inputs = vec![&current];
            let outputs = node.filter.process(&inputs, &node.params)?;
            if let Some(out) = outputs.into_iter().next() {
                current = out;
            }
        }

        // Push to all sinks
        for sink in self.sinks.iter_mut() {
            if sink.media_type == rsmpeg_util::MediaType::Video {
                sink.frames.push(current.clone());
            }
        }

        Ok(())
    }

    /// Get number of nodes.
    pub fn len(&self) -> usize { self.nodes.len() }
    pub fn is_empty(&self) -> bool { self.nodes.is_empty() }
}

impl Default for FilterGraph {
    fn default() -> Self { Self::new() }
}
```

### Task 8: Create rsmpeg-scale and rsmpeg-resample crates

**Files:**
- Create: `D:\rsmpeg\rsmpeg-scale\Cargo.toml`
- Create: `D:\rsmpeg\rsmpeg-scale\src\lib.rs`
- Create: `D:\rsmpeg\rsmpeg-scale\src\colorspace.rs`
- Create: `D:\rsmpeg\rsmpeg-scale\src\sws_context.rs`
- Create: `D:\rsmpeg\rsmpeg-resample\Cargo.toml`
- Create: `D:\rsmpeg\rsmpeg-resample\src\lib.rs`

- [ ] **Step 1: Create rsmpeg-scale Cargo.toml**

```toml
[package]
name = "rsmpeg-scale"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true
description = "Image scaling and pixel format conversion (libswscale equivalent)"

[lints]
workspace = true

[dependencies]
rsmpeg-util = { path = "../rsmpeg-util" }
rsmpeg-codec = { path = "../rsmpeg-codec" }
tracing.workspace = true
```

- [ ] **Step 2: Create rsmpeg-scale/src/lib.rs**

```rust
#![forbid(unsafe_code)]

pub mod colorspace;
pub mod sws_context;

pub use colorspace::Colorspace;
pub use sws_context::SwsContext;
```

- [ ] **Step 3: Write colorspace.rs**

```rust
/// Color space definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colorspace {
    /// ITU-R BT.601 (SDTV)
    Bt601,
    /// ITU-R BT.709 (HDTV)
    Bt709,
    /// ITU-R BT.2020 (UHDTV)
    Bt2020,
    /// SMPTE ST 2085 (DCI-P3)
    DciP3,
    /// Unspecified
    Unspecified,
}

impl Colorspace {
    pub fn name(self) -> &'static str {
        match self {
            Colorspace::Bt601 => "bt.601",
            Colorspace::Bt709 => "bt.709",
            Colorspace::Bt2020 => "bt.2020",
            Colorspace::DciP3 => "dci-p3",
            Colorspace::Unspecified => "unspecified",
        }
    }
}
```

- [ ] **Step 4: Write sws_context.rs**

```rust
use rsmpeg_util::{RsResult, PixelFormat};
use rsmpeg_codec::Frame;

/// Scaling flags (algorithm selection).
#[derive(Debug, Clone, Copy)]
pub enum SwsFlags {
    /// Fast bilinear interpolation.
    FastBilinear,
    /// Bilinear interpolation.
    Bilinear,
    /// Bicubic interpolation.
    Bicubic,
    /// Lanczos interpolation.
    Lanczos,
}

impl Default for SwsFlags {
    fn default() -> Self { SwsFlags::Bilinear }
}

/// Software scaling context, equivalent to FFmpeg's SwsContext.
pub struct SwsContext {
    pub src_format: PixelFormat,
    pub dst_format: PixelFormat,
    pub src_width: usize,
    pub src_height: usize,
    pub dst_width: usize,
    pub dst_height: usize,
    pub flags: SwsFlags,
    pub colorspace: crate::colorspace::Colorspace,
}

impl SwsContext {
    pub fn new(
        src_w: usize, src_h: usize, src_fmt: PixelFormat,
        dst_w: usize, dst_h: usize, dst_fmt: PixelFormat,
        flags: SwsFlags,
    ) -> Self {
        SwsContext {
            src_format: src_fmt,
            dst_format: dst_fmt,
            src_width: src_w,
            src_height: src_h,
            dst_width: dst_w,
            dst_height: dst_h,
            flags,
            colorspace: crate::colorspace::Colorspace::Unspecified,
        }
    }

    /// Scale a frame. Skeleton — returns a blank frame at the target size.
    pub fn scale(&self, src: &Frame) -> RsResult<Frame> {
        let mut dst = Frame::new_video(self.dst_width, self.dst_height, self.dst_format);
        dst.pts = src.pts;
        dst.duration = src.duration;
        dst.time_base = src.time_base;
        tracing::trace!("SwsContext::scale (skeleton): {}x{} -> {}x{}",
            self.src_width, self.src_height, self.dst_width, self.dst_height);
        Ok(dst)
    }
}
```

- [ ] **Step 5: Create rsmpeg-resample Cargo.toml**

```toml
[package]
name = "rsmpeg-resample"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true
description = "Audio resampling and format conversion (libswresample equivalent)"

[lints]
workspace = true

[dependencies]
rsmpeg-util = { path = "../rsmpeg-util" }
rsmpeg-codec = { path = "../rsmpeg-codec" }
tracing.workspace = true
```

- [ ] **Step 6: Create rsmpeg-resample/src/lib.rs**

```rust
#![forbid(unsafe_code)]

use rsmpeg_util::{RsResult, SampleFormat};
use rsmpeg_codec::Frame;

/// Audio resampling context, equivalent to FFmpeg's SwrContext.
pub struct SwrContext {
    pub src_sample_format: SampleFormat,
    pub dst_sample_format: SampleFormat,
    pub src_sample_rate: u32,
    pub dst_sample_rate: u32,
    pub src_channels: u16,
    pub dst_channels: u16,
}

impl SwrContext {
    pub fn new(
        src_fmt: SampleFormat, dst_fmt: SampleFormat,
        src_rate: u32, dst_rate: u32,
        src_ch: u16, dst_ch: u16,
    ) -> Self {
        SwrContext {
            src_sample_format: src_fmt,
            dst_sample_format: dst_fmt,
            src_sample_rate: src_rate,
            dst_sample_rate: dst_rate,
            src_channels: src_ch,
            dst_channels: dst_ch,
        }
    }

    /// Resample an audio frame. Skeleton — returns a blank frame at target format.
    pub fn resample(&self, src: &Frame) -> RsResult<Frame> {
        let mut dst = Frame::new_audio(self.dst_sample_format, self.dst_sample_rate, self.dst_channels, src.samples);
        dst.pts = src.pts;
        tracing::trace!("SwrContext::resample (skeleton): {}Hz/{}ch -> {}Hz/{}ch",
            self.src_sample_rate, self.src_channels,
            self.dst_sample_rate, self.dst_channels);
        Ok(dst)
    }
}
```

- [ ] **Step 7: Build**

Run: `cd D:\rsmpeg && cargo build -p rsmpeg-scale -p rsmpeg-resample`
Expected: Build succeeds

### Task 9: Create rsmpeg facade crate

**Files:**
- Create: `D:\rsmpeg\rsmpeg\Cargo.toml`
- Create: `D:\rsmpeg\rsmpeg\src\lib.rs`

- [ ] **Step 1: Create Cargo.toml**

```toml
[package]
name = "rsmpeg"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true
description = "Pure Rust FFmpeg clone — unified facade crate"

[lints]
workspace = true

[dependencies]
rsmpeg-util = { path = "../rsmpeg-util" }
rsmpeg-codec = { path = "../rsmpeg-codec" }
rsmpeg-format = { path = "../rsmpeg-format" }
rsmpeg-filter = { path = "../rsmpeg-filter" }
rsmpeg-scale = { path = "../rsmpeg-scale" }
rsmpeg-resample = { path = "../rsmpeg-resample" }
```

- [ ] **Step 2: Write lib.rs**

```rust
//! rsmpeg — Pure Rust FFmpeg clone
//!
//! rsmpeg is a clean-room, pure Rust reconstruction of FFmpeg — a memory-safe,
//! patent-free multimedia processing framework.
//!
//! # Architecture
//!
//! - [`rsmpeg_util`] — Core utility types (Rational, PixelFormat, etc.)
//! - [`rsmpeg_codec`] — Codec interface and types
//! - [`rsmpeg_format`] — Container format demuxing/muxing
//! - [`rsmpeg_filter`] — Filter graph pipeline
//! - [`rsmpeg_scale`] — Image scaling and pixel format conversion
//! - [`rsmpeg_resample`] — Audio resampling

pub use rsmpeg_util as util;
pub use rsmpeg_codec as codec;
pub use rsmpeg_format as format;
pub use rsmpeg_filter as filter;
pub use rsmpeg_scale as scale;
pub use rsmpeg_resample as resample;

/// Prelude module — re-exports commonly used types.
pub mod prelude {
    pub use rsmpeg_util::{RsError, RsResult, Rational, MediaType, PixelFormat, SampleFormat};
    pub use rsmpeg_codec::{CodecId, Codec, CodecRegistry, CodecContext, Packet, Frame, PictureType};
    pub use rsmpeg_format::{FormatContext, Stream, InputFormat, OutputFormat, IOContext, probe_format};
    pub use rsmpeg_filter::{Filter, FilterGraph};
    pub use rsmpeg_scale::SwsContext;
    pub use rsmpeg_resample::SwrContext;
}
```

### Task 10: Create rsmpeg-cli — main, subcommand dispatch

**Files:**
- Create: `D:\rsmpeg\rsmpeg-cli\Cargo.toml`
- Create: `D:\rsmpeg\rsmpeg-cli\src\main.rs`
- Create: `D:\rsmpeg\rsmpeg-cli\src\probe_cmd.rs`
- Create: `D:\rsmpeg\rsmpeg-cli\src\transcode_cmd.rs`
- Create: `D:\rsmpeg\rsmpeg-cli\src\play_cmd.rs`

- [ ] **Step 1: Create Cargo.toml**

```toml
[package]
name = "rsmpeg-cli"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true
description = "rsmpeg CLI — ffmpeg/ffprobe/ffplay equivalent tools"

[[bin]]
name = "rsmpeg"
path = "src/main.rs"

[dependencies]
rsmpeg-util = { path = "../rsmpeg-util" }
rsmpeg-codec = { path = "../rsmpeg-codec" }
rsmpeg-format = { path = "../rsmpeg-format" }
rsmpeg-filter = { path = "../rsmpeg-filter" }
rsmpeg-scale = { path = "../rsmpeg-scale" }
rsmpeg-resample = { path = "../rsmpeg-resample" }
clap = { workspace = true, features = ["derive"] }
serde_json.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
anyhow = "1"
```

- [ ] **Step 2: Write main.rs — subcommand dispatch**

```rust
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "rsmpeg", version, about = "Pure Rust FFmpeg clone")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Probe a media file and show information (ffprobe equivalent)
    Probe(probe_cmd::ProbeArgs),
    /// Transcode a media file (ffmpeg equivalent)
    Transcode(transcode_cmd::TranscodeArgs),
    /// Play a media file (ffplay equivalent)
    Play(play_cmd::PlayArgs),
}

mod probe_cmd;
mod transcode_cmd;
mod play_cmd;

fn main() {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Probe(args) => {
            if let Err(e) = probe_cmd::run_probe(&args) {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
        }
        Commands::Transcode(args) => {
            if let Err(e) = transcode_cmd::run_transcode(&args) {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
        }
        Commands::Play(args) => {
            if let Err(e) = play_cmd::run_play(&args) {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
        }
    }
}
```

- [ ] **Step 3: Write probe_cmd.rs (ffprobe equivalent)**

```rust
use clap::Args;
use rsmpeg_format::probe_format;
use rsmpeg_format::format_context::FormatContext;
use anyhow::Result;

#[derive(Args)]
pub struct ProbeArgs {
    /// Input file path
    pub input: String,

    /// Output format (default: human-readable)
    #[arg(short = 'o', long, default_value = "text")]
    pub output: String,

    /// Show packets
    #[arg(short = 'p', long)]
    pub show_packets: bool,

    /// Show format only
    #[arg(long)]
    pub show_format: bool,

    /// Show streams only
    #[arg(long)]
    pub show_streams: bool,
}

pub fn run_probe(args: &ProbeArgs) -> Result<()> {
    // Read initial bytes for format detection
    let path = std::path::Path::new(&args.input);
    if !path.exists() {
        anyhow::bail!("File not found: {}", args.input);
    }

    let file_len = std::fs::metadata(path)?.len();
    let mut file = std::fs::File::open(path)?;

    // Probe from magic bytes
    use std::io::Read;
    let mut probe_buf = vec![0u8; 2048];
    let n = file.read(&mut probe_buf)?;
    probe_buf.truncate(n);
    drop(file);

    let probe_results = probe_format(&probe_buf);

    if args.output == "json" {
        // JSON output (ffprobe-like)
        let mut json = serde_json::json!({
            "format": {
                "filename": args.input,
                "nb_streams": 0,
                "format_name": probe_results.first().map(|r| r.format_name).unwrap_or("unknown"),
                "format_long_name": probe_results.first().map(|r| r.description).unwrap_or("Unknown"),
                "size": file_len.to_string(),
            },
            "streams": []
        });
        println!("{}", serde_json::to_string_pretty(&json)?);
    } else {
        // Human-readable text output
        println!("[ffprobe] File: {}", args.input);
        println!("[ffprobe] Size: {} bytes", file_len);
        println!();
        println!("Format detection:");
        if probe_results.is_empty() {
            println!("  Unknown format (no magic bytes matched)");
        } else {
            for r in &probe_results {
                println!("  {} [{:?}] — {}", r.format_name, r.score, r.description);
            }
        }

        // Try to open for more detailed probe
        if let Ok(ctx) = FormatContext::open_input(path) {
            println!();
            println!("Streams:");
            if ctx.streams.is_empty() {
                println!("  (no streams — demuxer skeleton loaded)");
            }
            for stream in &ctx.streams {
                println!("  Stream #{}: {} ({:?})", stream.index, stream.codec_id.name(), stream.codec_id);
            }
            if let Some(fmt) = &ctx.format_name {
                println!();
                println!("Format: {}", fmt);
            }
        }
    }

    Ok(())
}
```

- [ ] **Step 4: Write transcode_cmd.rs (ffmpeg equivalent)**

```rust
use clap::Args;
use anyhow::Result;

#[derive(Args)]
pub struct TranscodeArgs {
    /// Input file
    pub input: String,
    /// Output file
    pub output: String,
    /// Video codec
    #[arg(short = 'c', long = "codec")]
    pub codec: Option<String>,
    /// Video bitrate (e.g., "1M")
    #[arg(short = 'b', long = "bitrate")]
    pub bitrate: Option<String>,
    /// Output width
    #[arg(long)]
    pub width: Option<usize>,
    /// Output height
    #[arg(long)]
    pub height: Option<usize>,
}

pub fn run_transcode(args: &TranscodeArgs) -> Result<()> {
    println!("[ffmpeg] Input: {}", args.input);
    println!("[ffmpeg] Output: {}", args.output);
    println!("[ffmpeg] Transcoding pipeline skeleton");

    if let Some(codec) = &args.codec {
        println!("[ffmpeg]   Video codec: {codec}");
    }
    if let Some(br) = &args.bitrate {
        println!("[ffmpeg]   Bitrate: {br}");
    }
    if let Some(w) = args.width {
        if let Some(h) = args.height {
            println!("[ffmpeg]   Scale: {w}x{h}");
        }
    }

    // Skeleton transcode pipeline
    println!();
    println!("Pipeline structure:");
    println!("  1. Demuxer  → reading {}", args.input);
    println!("  2. Decoder  → decoding frames");
    if args.width.is_some() || args.height.is_some() {
        println!("  3. Scale    → rescaling");
    }
    println!("  4. Encoder  → encoding frames");
    println!("  5. Muxer    → writing {}", args.output);
    println!();
    println!("[ffmpeg] Transcode complete (skeleton — no actual processing)");

    Ok(())
}
```

- [ ] **Step 5: Write play_cmd.rs (ffplay equivalent)**

```rust
use clap::Args;
use anyhow::Result;

#[derive(Args)]
pub struct PlayArgs {
    /// Input file
    pub input: String,
    /// Volume (0-100)
    #[arg(short = 'v', long, default_value = "100")]
    pub volume: u8,
    /// Start position (seconds)
    #[arg(short = 's', long)]
    pub start: Option<f64>,
    /// Loop
    #[arg(short = 'l', long)]
    pub loop_playback: bool,
}

pub fn run_play(args: &PlayArgs) -> Result<()> {
    println!("[ffplay] File: {}", args.input);
    println!("[ffplay] Volume: {}%", args.volume);
    if let Some(s) = args.start {
        println!("[ffplay] Start position: {s}s");
    }
    println!("[ffplay] Loop: {}", if args.loop_playback { "yes" } else { "no" });
    println!();
    println!("[ffplay] Playback skeleton — no actual playback implemented");
    println!("[ffplay] Pipeline: File → Demuxer → Decoder → Audio/Video Output");
    println!();
    println!("[ffplay] Use 'rsmpeg probe {}' to see file information", args.input);

    Ok(())
}
```

- [ ] **Step 6: Build CLI**

Run: `cd D:\rsmpeg && cargo build -p rsmpeg-cli`
Expected: Build succeeds, binary available at `target/debug/rsmpeg.exe`

### Task 11: Create examples

**Files:**
- Create: `D:\rsmpeg\examples\probe_file.rs`
- Create: `D:\rsmpeg\examples\simple_transcode.rs`

- [ ] **Step 1: Write examples/probe_file.rs**

```rust
/// Example: probe a media file using rsmpeg library.
///
/// Usage: cargo run --example probe_file -- <path-to-media-file>
use rsmpeg::format::probe_format;
use std::env;
use std::io::Read;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <media-file>", args[0]);
        std::process::exit(1);
    }

    let path = &args[1];
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Error opening '{}': {e}", path);
            std::process::exit(1);
        }
    };

    let mut buf = vec![0u8; 2048];
    let n = file.read(&mut buf).unwrap_or(0);
    buf.truncate(n);

    println!("Probing: {}", path);
    println!("File size: {} bytes", std::fs::metadata(path).map(|m| m.len()).unwrap_or(0));
    println!();

    let results = probe_format(&buf);
    if results.is_empty() {
        println!("Unknown format");
    } else {
        println!("Detected formats:");
        for r in &results {
            println!("  {} ({}) — {:?}", r.format_name, r.description, r.score);
        }
    }
}
```

- [ ] **Step 2: Write examples/simple_transcode.rs**

```rust
/// Example: transcode pipeline skeleton using rsmpeg library.
use rsmpeg::codec::{CodecContext, CodecId};
use rsmpeg::format::{FormatContext, Stream};
use rsmpeg::util::{PixelFormat, Rational};

fn main() {
    println!("rsmpeg transcode pipeline example");
    println!("=================================\n");

    // Demonstrate creating a format context
    let input = "input.mp4";
    let output = "output.mkv";

    println!("Input:  {input}");
    println!("Output: {output}\n");

    // Create codec context (builder pattern)
    let codec_ctx = CodecContext::builder()
        .codec_id(CodecId::Av1)
        .width(1920)
        .height(1080)
        .pixel_format(PixelFormat::Yuv420P)
        .bit_rate(2_000_000)
        .time_base(Rational::new(1, 30))
        .build();

    println!("Decoder context:");
    println!("  Codec:  {:?}", codec_ctx.codec_id());
    println!("  Size:   {}x{}", codec_ctx.width().unwrap_or(0), codec_ctx.height().unwrap_or(0));
    println!("  Format: {:?}", codec_ctx.pixel_format());
    println!("  Bitrate: {} bps", codec_ctx.bit_rate().unwrap_or(0));
    println!();

    // Demonstrate format context
    match FormatContext::open_input(input) {
        Ok(ctx) => {
            println!("Format context opened successfully");
            println!("  Format: {:?}", ctx.format_name);
            println!("  Streams: {}", ctx.nb_streams());
        }
        Err(e) => {
            println!("Format context: {e} (expected — 'input.mp4' doesn't exist)");
        }
    }

    println!();
    println!("Transcode pipeline: Input → Demuxer → Decoder → Encoder → Muxer → Output");
}
```

- [ ] **Step 3: Build examples**

Run: `cd D:\rsmpeg && cargo build --examples`
Expected: Build succeeds

### Task 12: Final workspace build and verification

- [ ] **Step 1: Build entire workspace**

Run: `cd D:\rsmpeg && cargo build --workspace`
Expected: Clean build, no warnings, zero errors

- [ ] **Step 2: Run all tests**

Run: `cd D:\rsmpeg && cargo test --workspace`
Expected: All tests pass (rational arithmetic, probe detection, etc.)

- [ ] **Step 3: Verify CLI works**

```bash
cd D:\rsmpeg
cargo run -- probe --help
cargo run -- transcode --help
cargo run -- play --help
```

Expected: Each command shows usage information

```bash
cargo run -- probe Cargo.toml
```

Expected: Shows probe output (unrecognized format since Cargo.toml is not a media file)
