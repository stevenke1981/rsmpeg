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
