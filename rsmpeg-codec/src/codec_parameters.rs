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
}
