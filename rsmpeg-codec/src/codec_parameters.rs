use crate::codec_id::CodecId;
use rsmpeg_util::{MediaType, PixelFormat, SampleFormat};

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
