use rsmpeg_codec::{CodecId, CodecParameters};
use rsmpeg_util::{Dict, MediaType, Rational};

/// Media stream descriptor, equivalent to FFmpeg's AVStream.
#[derive(Debug, Clone)]
pub struct Stream {
    pub index: usize,
    pub codec_id: CodecId,
    pub media_type: MediaType,
    pub codec_params: CodecParameters,
    pub time_base: Rational,
    pub duration: i64,
    pub metadata: Dict,
    pub avg_frame_rate: Rational,
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

    pub fn duration_seconds(&self) -> f64 {
        self.duration as f64 * self.time_base.to_f64()
    }
}
