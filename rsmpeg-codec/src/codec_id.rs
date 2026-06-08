use serde::{Deserialize, Serialize};

/// Codec identifier, equivalent to FFmpeg's AVCodecID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CodecId {
    // ── Video ──
    Av1,
    Vp9,
    Vp8,
    Theora,
    Mpeg4,
    H263,
    Mjpeg,
    Ffv1,
    JpegXl,
    ProRes,
    DnxHd,
    // ── Audio ──
    Opus,
    Vorbis,
    Flac,
    Mp3,
    Pcm,
    Alac,
    // ── Image ──
    Png,
    Gif,
    WebP,
    Bmp,
    // ── Subtitle ──
    Srt,
    WebVtt,
    // ── Unknown ──
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
            CodecId::ProRes => "prores",
            CodecId::DnxHd => "dnxhd",
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
            "prores" => Some(CodecId::ProRes),
            "dnxhd" => Some(CodecId::DnxHd),
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

    pub fn media_type(self) -> rsmpeg_util::MediaType {
        use rsmpeg_util::MediaType;
        match self {
            CodecId::Av1
            | CodecId::Vp9
            | CodecId::Vp8
            | CodecId::Theora
            | CodecId::Mpeg4
            | CodecId::H263
            | CodecId::Mjpeg
            | CodecId::Ffv1
            | CodecId::JpegXl
            | CodecId::ProRes
            | CodecId::DnxHd
            | CodecId::Png
            | CodecId::Gif
            | CodecId::WebP
            | CodecId::Bmp => MediaType::Video,
            CodecId::Opus
            | CodecId::Vorbis
            | CodecId::Flac
            | CodecId::Mp3
            | CodecId::Pcm
            | CodecId::Alac => MediaType::Audio,
            CodecId::Srt | CodecId::WebVtt => MediaType::Subtitle,
            CodecId::Unknown => MediaType::Data,
        }
    }
}
