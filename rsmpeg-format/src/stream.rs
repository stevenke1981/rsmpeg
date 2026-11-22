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
    /// Stream duration **in `time_base` ticks**, exactly like FFmpeg's
    /// `AVStream.duration` ("in stream time base"). Demuxers that only know a
    /// millisecond figure must convert it, or keep `time_base` at 1/1000.
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

    /// Duration in seconds (`duration × time_base`).
    pub fn duration_seconds(&self) -> f64 {
        if self.time_base.den == 0 {
            return 0.0;
        }
        self.duration as f64 * self.time_base.to_f64()
    }

    /// Set the duration from a duration expressed **in milliseconds**.
    ///
    /// Converts into the stream's own `time_base`, so `duration_seconds()` stays
    /// correct no matter which time base the container dictates. Returns the
    /// tick count that was stored.
    pub fn set_duration_ms(&mut self, duration_ms: i64) -> i64 {
        if self.time_base.den == 0 {
            return 0;
        }
        // ticks = ms * 1000 / time_base... i.e. ms * den / (num * 1000)
        let num = self.time_base.num as i128;
        let den = self.time_base.den as i128;
        if num == 0 {
            return 0;
        }
        let ticks = (duration_ms as i128 * den) / (num * 1000);
        let ticks = ticks.clamp(i64::MIN as i128, i64::MAX as i128) as i64;
        self.duration = ticks;
        ticks
    }
}
