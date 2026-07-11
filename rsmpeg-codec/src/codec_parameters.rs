use crate::codec_id::CodecId;
use rsmpeg_util::{MediaType, PixelFormat, SampleFormat};

/// How H.264 NAL units are framed in elementary stream samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum H264BitstreamFormat {
    /// Length-prefixed NAL units (MP4 / ISO BMFF avcC).
    Avcc {
        /// Byte width of the length prefix: 1, 2, or 4.
        nal_length_size: u8,
    },
    /// Start-code prefixed NAL units (Annex B / MPEG-TS / raw).
    AnnexB,
    /// Format not yet determined.
    #[default]
    Unknown,
}

#[derive(Debug, Clone)]
pub struct CodecParameters {
    pub codec_id: CodecId,
    pub media_type: MediaType,
    pub width: Option<usize>,
    pub height: Option<usize>,
    pub pixel_format: Option<PixelFormat>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
    pub sample_format: Option<SampleFormat>,
    pub bit_rate: Option<u64>,
    pub extradata: Option<Vec<u8>>,
    /// H.264-only: AVCC vs Annex B (ignored for other codecs).
    pub h264_bitstream_format: H264BitstreamFormat,
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
            h264_bitstream_format: H264BitstreamFormat::Unknown,
        }
    }

    /// Build video `CodecParameters` with sensible defaults.
    ///
    /// Sets `width`/`height`/`codec_id` (and derives `media_type`), while all
    /// audio-related fields stay `None`/zero and `h264_bitstream_format` defaults
    /// to `Unknown`. Mirrors how a player typically constructs video params.
    pub fn for_video(width: usize, height: usize, codec_id: CodecId) -> Self {
        CodecParameters {
            codec_id,
            media_type: codec_id.media_type(),
            width: Some(width),
            height: Some(height),
            pixel_format: None,
            sample_rate: None,
            channels: None,
            sample_format: None,
            bit_rate: None,
            extradata: None,
            h264_bitstream_format: H264BitstreamFormat::Unknown,
        }
    }

    /// Build audio `CodecParameters` with sensible defaults.
    ///
    /// Sets `sample_rate`/`channels`/`codec_id` (and derives `media_type`),
    /// while all video-related fields stay `None`/zero and
    /// `h264_bitstream_format` defaults to `Unknown`. Mirrors how a player
    /// typically constructs audio params.
    pub fn for_audio(sample_rate: u32, channels: u16, codec_id: CodecId) -> Self {
        CodecParameters {
            codec_id,
            media_type: codec_id.media_type(),
            width: None,
            height: None,
            pixel_format: None,
            sample_rate: Some(sample_rate),
            channels: Some(channels),
            sample_format: None,
            bit_rate: None,
            extradata: None,
            h264_bitstream_format: H264BitstreamFormat::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec_id::CodecId;

    #[test]
    fn codec_parameters_builders() {
        let video = CodecParameters::for_video(1920, 1080, CodecId::H264);
        assert_eq!(video.codec_id, CodecId::H264);
        assert_eq!(video.media_type, rsmpeg_util::MediaType::Video);
        assert_eq!(video.width, Some(1920));
        assert_eq!(video.height, Some(1080));
        assert_eq!(video.sample_rate, None);
        assert_eq!(video.channels, None);
        assert_eq!(video.pixel_format, None);
        assert_eq!(video.bit_rate, None);
        assert_eq!(video.extradata, None);
        assert_eq!(video.h264_bitstream_format, H264BitstreamFormat::Unknown);

        let audio = CodecParameters::for_audio(44100, 2, CodecId::Aac);
        assert_eq!(audio.codec_id, CodecId::Aac);
        assert_eq!(audio.media_type, rsmpeg_util::MediaType::Audio);
        assert_eq!(audio.sample_rate, Some(44100));
        assert_eq!(audio.channels, Some(2));
        assert_eq!(audio.width, None);
        assert_eq!(audio.height, None);
        assert_eq!(audio.sample_format, None);
        assert_eq!(audio.bit_rate, None);
        assert_eq!(audio.extradata, None);
        assert_eq!(audio.h264_bitstream_format, H264BitstreamFormat::Unknown);
    }
}
