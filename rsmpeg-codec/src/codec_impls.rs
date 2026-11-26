//! Built-in codec implementations.
//!
//! These implement the `Codec` + `Decoder`/`Encoder` traits for common formats
//! that can be handled without FFmpeg: raw video, PCM audio, FLAC, MJPEG, and subtitles.

mod flac_decoder;
mod mjpeg_decoder;
mod pcm_audio;
mod raw_video;
mod subtitle;

pub use flac_decoder::{FlacCodec, FlacDecoder};
pub use mjpeg_decoder::{MjpegCodec, MjpegDecoder};
pub use pcm_audio::{PCMAudioCodec, PCMAudioDecoder, PCMAudioEncoder};
pub use raw_video::{RawVideoCodec, RawVideoDecoder, RawVideoEncoder};
pub use subtitle::{
    parse_srt, parse_subtitle_file, parse_webvtt, SrtCodec, SubtitleCue, SubtitleDecoder,
    SubtitleTrack, WebVttCodec,
};
