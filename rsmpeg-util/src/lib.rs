//! rsmpeg-util — Core utility types (libavutil equivalent)
//!
//! Provides foundational types shared across all rsmpeg crates:
//! error handling, rational arithmetic, media types, pixel/sample formats.

#![forbid(unsafe_code)]

pub mod channel_layout;
pub mod dict;
pub mod error;
pub mod media_type;
pub mod pixel_format;
pub mod rational;
pub mod sample_format;

pub use channel_layout::ChannelLayout;
pub use dict::Dict;
pub use error::{RsError, RsResult};
pub use media_type::MediaType;
pub use pixel_format::PixelFormat;
pub use rational::Rational;
pub use sample_format::SampleFormat;
