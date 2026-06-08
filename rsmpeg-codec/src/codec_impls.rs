//! Built-in codec implementations.
//!
//! These implement the `Codec` + `Decoder`/`Encoder` traits for common formats
//! that can be handled without FFmpeg: raw video, PCM audio, etc.

mod pcm_audio;
mod raw_video;

pub use pcm_audio::PCMAudioCodec;
pub use raw_video::RawVideoCodec;
