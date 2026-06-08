#![forbid(unsafe_code)]

//! rsmpeg — Pure Rust multimedia framework.
//!
//! A complete FFmpeg-equivalent media processing library written in Rust.
//! All code is safe Rust (`#![forbid(unsafe_code)]`).

pub use rsmpeg_codec as codec;
pub use rsmpeg_filter as filter;
pub use rsmpeg_format as format;
pub use rsmpeg_resample as resample;
pub use rsmpeg_scale as scale;
/// Re-export all sub-crates as public modules.
pub use rsmpeg_util as util;

pub use rsmpeg_codec::{
    Codec, CodecContext, CodecId, CodecParameters, CodecRegistry, Frame, Packet, PacketFlags,
    PictureType,
};
pub use rsmpeg_filter::{Filter, FilterContext, FilterGraph, Pad, PadDirection};
pub use rsmpeg_format::{
    FormatContext, FormatRegistry, IOContext, InputFormat, OutputFormat, Stream,
};
pub use rsmpeg_resample::{DitherMethod, Resampler, ResamplerConfig};
pub use rsmpeg_scale::{ColorRange, ColorSpace, InterpolationMethod, Scaler, ScalerConfig};
/// Convenience re-exports of the most commonly used types.
pub use rsmpeg_util::{
    ChannelLayout, Dict, MediaType, PixelFormat, Rational, RsError, RsResult, SampleFormat,
};

/// Version information.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Print version and component info.
pub fn version_info() -> String {
    format!(
        "rsmpeg v{} — Pure Rust multimedia framework\n\
         Components:\n\
         \x20 rsmpeg-util   — Common utilities\n\
         \x20 rsmpeg-codec  — Codec support\n\
         \x20 rsmpeg-format — Container format I/O\n\
         \x20 rsmpeg-filter — Filter graph\n\
         \x20 rsmpeg-scale  — Video scaling\n\
         \x20 rsmpeg-resample — Audio resampling",
        VERSION
    )
}
