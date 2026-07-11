use crate::channel_mapping::ChannelMapping;
use crate::dither::DitherMethod;
use rsmpeg_codec::Frame;
use rsmpeg_util::{ChannelLayout, RsError, RsResult, SampleFormat};

/// Resampler control flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResamplerFlags {
    pub linear: bool,
    pub cubic: bool,
    pub sinc: bool,
    pub accurate_rnd: bool,
    pub full_rnd: bool,
}

impl ResamplerFlags {
    pub const NONE: ResamplerFlags = ResamplerFlags {
        linear: false,
        cubic: false,
        sinc: false,
        accurate_rnd: false,
        full_rnd: false,
    };

    fn _sinc_flags() -> Self {
        ResamplerFlags {
            linear: false,
            cubic: false,
            sinc: true,
            accurate_rnd: false,
            full_rnd: false,
        }
    }
}

/// Configure an audio resampler.
#[derive(Debug, Clone)]
pub struct ResamplerConfig {
    pub src_sample_rate: u32,
    pub dst_sample_rate: u32,
    pub src_format: SampleFormat,
    pub dst_format: SampleFormat,
    pub src_channel_layout: ChannelLayout,
    pub dst_channel_layout: ChannelLayout,
    pub dither_method: DitherMethod,
    pub flags: ResamplerFlags,
    pub linear_interp: bool,
    pub cutoff: f64, // 0.0..1.0
    pub nb_output_samples: Option<usize>,
}

impl ResamplerConfig {
    /// Simple stereo-to-stereo resample (common case).
    pub fn new(
        src_sample_rate: u32,
        dst_sample_rate: u32,
        src_format: SampleFormat,
        dst_format: SampleFormat,
    ) -> Self {
        ResamplerConfig {
            src_sample_rate,
            dst_sample_rate,
            src_format,
            dst_format,
            src_channel_layout: ChannelLayout::STEREO,
            dst_channel_layout: ChannelLayout::STEREO,
            dither_method: DitherMethod::None,
            flags: ResamplerFlags::_sinc_flags(),
            linear_interp: false,
            cutoff: 0.0, // auto
            nb_output_samples: None,
        }
    }

    pub fn with_channel_layouts(mut self, src: ChannelLayout, dst: ChannelLayout) -> Self {
        self.src_channel_layout = src;
        self.dst_channel_layout = dst;
        self
    }

    pub fn with_dither(mut self, method: DitherMethod) -> Self {
        self.dither_method = method;
        self
    }

    /// Estimate the number of output samples for a given number of input samples.
    ///
    /// Uses integer arithmetic to avoid floating-point precision issues.
    pub fn estimate_output_samples(&self, nb_input: usize) -> usize {
        if self.src_sample_rate == 0 {
            return nb_input;
        }
        // ceiling of integer division: (nb_input * dst_rate + src_rate - 1) / src_rate
        let num = nb_input as u64 * self.dst_sample_rate as u64;
        let den = self.src_sample_rate as u64;
        num.div_ceil(den) as usize
    }
}

/// Audio resampler — converts sample rate, format, and channel layout.
///
/// Equivalent to FFmpeg's SwrContext.
pub struct Resampler {
    config: ResamplerConfig,
    channel_mapping: Option<ChannelMapping>,
    delay: usize,
}

impl Resampler {
    pub fn new(config: ResamplerConfig) -> RsResult<Self> {
        let channel_mapping = if config.src_channel_layout != config.dst_channel_layout {
            Some(ChannelMapping::new(
                config.src_channel_layout,
                config.dst_channel_layout,
            ))
        } else {
            None
        };

        tracing::debug!(
            "Resampler: {}Hz/{:?} ({}) → {}Hz/{:?} ({}) [dither={}]",
            config.src_sample_rate,
            config.src_format,
            config.src_channel_layout.name(),
            config.dst_sample_rate,
            config.dst_format,
            config.dst_channel_layout.name(),
            config.dither_method.name(),
        );

        Ok(Resampler {
            config,
            channel_mapping,
            delay: 0,
        })
    }

    pub fn config(&self) -> &ResamplerConfig {
        &self.config
    }

    /// Resample (and optionally remix/convert format) an audio frame.
    ///
    /// Returns a new frame in the destination format/layout/rate.
    ///
    /// The conversion is implemented with per-channel linear interpolation in
    /// normalized `[-1, 1)` space, followed by encoding into the destination
    /// sample format. Input is decoded from the first (interleaved) data plane
    /// for `S16` and `F32` sample formats.
    pub fn resample(&self, frame: &Frame) -> RsResult<Frame> {
        let nb_in = frame.samples;
        let src_ch = frame.channels as usize;
        let dst_ch = self.config.dst_channel_layout.channels();
        let dst_rate = self.config.dst_sample_rate;
        let src_rate = self.config.src_sample_rate;
        let dst_format = self.config.dst_format;

        if src_ch == 0 || dst_ch == 0 {
            return Err(RsError::InvalidData("Resampler: zero channel count".into()));
        }

        // Empty input → empty output frame.
        if nb_in == 0 {
            return Ok(Frame::new_audio(dst_format, dst_rate, dst_ch as u16, 0));
        }

        let plane = frame.data.first().map(|d| d.as_slice()).unwrap_or(&[]);
        let bytes_per = frame.sample_format.bytes();
        let expected = nb_in * src_ch * bytes_per;
        if plane.len() < expected {
            return Err(RsError::InvalidData(
                format!(
                    "Resampler: input plane too short (have {}, need {})",
                    plane.len(),
                    expected
                )
                .into(),
            ));
        }

        // Fast path: identical rate/format/layout (non-planar) → copy bytes.
        if src_rate == dst_rate
            && frame.sample_format == dst_format
            && !frame.sample_format.is_planar()
            && self.config.src_channel_layout == self.config.dst_channel_layout
        {
            let mut out = Frame::new_audio(dst_format, dst_rate, dst_ch as u16, nb_in);
            out.data[0] = plane.to_vec();
            out.linesize[0] = plane.len();
            out.pts = frame.pts;
            out.time_base = frame.time_base;
            out.duration = frame.duration;
            return Ok(out);
        }

        // Decode the interleaved input plane into deinterleaved f32 per source
        // channel, normalized to [-1, 1).
        let mut decoded: Vec<Vec<f32>> = vec![vec![0.0_f32; nb_in]; src_ch];
        match frame.sample_format {
            SampleFormat::S16 =>
            {
                #[allow(clippy::needless_range_loop)]
                for i in 0..nb_in {
                    for c in 0..src_ch {
                        let off = (i * src_ch + c) * 2;
                        let v = i16::from_le_bytes([plane[off], plane[off + 1]]);
                        decoded[c][i] = v as f32 / 32768.0;
                    }
                }
            }
            SampleFormat::F32 =>
            {
                #[allow(clippy::needless_range_loop)]
                for i in 0..nb_in {
                    for c in 0..src_ch {
                        let off = (i * src_ch + c) * 4;
                        let v = f32::from_le_bytes([
                            plane[off],
                            plane[off + 1],
                            plane[off + 2],
                            plane[off + 3],
                        ]);
                        decoded[c][i] = v;
                    }
                }
            }
            other => {
                return Err(RsError::Unsupported(
                    format!("Resampler: unsupported source sample format {:?}", other).into(),
                ));
            }
        }

        let dst_samples = self.config.estimate_output_samples(nb_in);
        // src samples per dst sample.
        let ratio = src_rate as f64 / dst_rate as f64;
        let mut resampled: Vec<Vec<f32>> = vec![vec![0.0_f32; dst_samples]; dst_ch];
        let last = nb_in - 1;

        #[allow(clippy::needless_range_loop)]
        for o in 0..dst_ch {
            let src_idx = self
                .src_channel_for_dst(o)
                .min(decoded.len().saturating_sub(1));
            let src = &decoded[src_idx];
            let dst = &mut resampled[o];
            for j in 0..dst_samples {
                let src_pos = j as f64 * ratio;
                let i = src_pos.floor() as usize;
                let frac = src_pos - i as f64;
                let i0 = i.min(last);
                let i1 = (i + 1).min(last);
                let frac32 = frac as f32;
                dst[j] = src[i0] * (1.0 - frac32) + src[i1] * frac32;
            }
        }

        // Encode the interleaved output in the destination sample format.
        let dst_bytes = dst_format.bytes();
        let mut encoded: Vec<u8> = Vec::with_capacity(dst_samples * dst_ch * dst_bytes);
        #[allow(clippy::needless_range_loop)]
        for j in 0..dst_samples {
            for o in 0..dst_ch {
                let s = resampled[o][j];
                match dst_format {
                    SampleFormat::S16 => {
                        let clamped = s.clamp(-1.0, 1.0);
                        let v = (clamped * 32768.0).round() as i32;
                        let v = v.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
                        encoded.extend_from_slice(&v.to_le_bytes());
                    }
                    SampleFormat::F32 => {
                        encoded.extend_from_slice(&(s as f32).to_le_bytes());
                    }
                    other => {
                        return Err(RsError::Unsupported(
                            format!(
                                "Resampler: unsupported destination sample format {:?}",
                                other
                            )
                            .into(),
                        ));
                    }
                }
            }
        }

        let encoded_len = encoded.len();
        let mut out = Frame::new_audio(dst_format, dst_rate, dst_ch as u16, dst_samples);
        out.data[0] = encoded;
        out.linesize[0] = encoded_len;
        out.pts = frame.pts;
        out.time_base = frame.time_base;
        out.duration = frame.duration;
        Ok(out)
    }

    /// Map a destination channel index to the source channel index that should
    /// feed it.
    ///
    /// - Equal layouts: identity mapping.
    /// - Different layouts: use the dominant coefficient in the remixing matrix
    ///   built by `ChannelMapping`. If the row has no non-zero weight, fall back
    ///   to taking the first source channels (or duplicating the last one when
    ///   upmixing).
    fn src_channel_for_dst(&self, dst_idx: usize) -> usize {
        let src_ch = self.config.src_channel_layout.channels();

        if self.config.src_channel_layout == self.config.dst_channel_layout {
            return dst_idx.min(src_ch.saturating_sub(1));
        }

        match &self.channel_mapping {
            Some(cm) => {
                let row_idx = dst_idx.min(cm.matrix.len().saturating_sub(1));
                let row = &cm.matrix[row_idx];
                let mut best = 0usize;
                let mut best_w = 0.0_f64;
                let mut any = false;
                for (in_ch, &w) in row.iter().enumerate() {
                    if w != 0.0 {
                        any = true;
                        let aw = w.abs();
                        if aw > best_w {
                            best_w = aw;
                            best = in_ch;
                        }
                    }
                }
                if any {
                    best.min(src_ch.saturating_sub(1))
                } else {
                    // No weight: downmix takes first channel, upmix duplicates it.
                    0.min(src_ch.saturating_sub(1))
                }
            }
            None => dst_idx.min(src_ch.saturating_sub(1)),
        }
    }

    /// Get the number of delayed samples remaining in the resampler.
    pub fn delay(&self) -> usize {
        self.delay
    }

    /// Flush remaining samples.
    pub fn flush(&mut self) -> RsResult<Option<Frame>> {
        if self.delay > 0 {
            let frame = Frame::new_audio(
                self.config.dst_format,
                self.config.dst_sample_rate,
                self.config.dst_channel_layout.channels() as u16,
                self.delay,
            );
            self.delay = 0;
            Ok(Some(frame))
        } else {
            Ok(None)
        }
    }

    /// Get the compression ratio (dst_samples / src_samples).
    pub fn compression_ratio(&self) -> f64 {
        if self.config.src_sample_rate == 0 {
            return 1.0;
        }
        self.config.dst_sample_rate as f64 / self.config.src_sample_rate as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resampler_creation() {
        let config = ResamplerConfig::new(44100, 48000, SampleFormat::F32, SampleFormat::F32);
        let resampler = Resampler::new(config).unwrap();
        assert_eq!(resampler.config().src_sample_rate, 44100);
        assert_eq!(resampler.config().dst_sample_rate, 48000);
    }

    #[test]
    fn test_resampler_with_options() {
        let config = ResamplerConfig::new(48000, 44100, SampleFormat::S16, SampleFormat::F32)
            .with_channel_layouts(ChannelLayout::STEREO, ChannelLayout::MONO)
            .with_dither(DitherMethod::Triangular);
        let resampler = Resampler::new(config).unwrap();
        assert_eq!(resampler.config().dither_method, DitherMethod::Triangular);
        assert_eq!(resampler.config().dst_channel_layout, ChannelLayout::MONO);
    }

    #[test]
    fn test_output_estimate() {
        let config = ResamplerConfig::new(44100, 48000, SampleFormat::F32, SampleFormat::F32);
        assert_eq!(config.estimate_output_samples(44100), 48000);
        assert_eq!(config.estimate_output_samples(0), 0);
    }

    #[test]
    fn test_compression_ratio() {
        let config = ResamplerConfig::new(44100, 22050, SampleFormat::S16, SampleFormat::S16);
        let resampler = Resampler::new(config).unwrap();
        assert!((resampler.compression_ratio() - 0.5).abs() < 1e-10);
    }

    #[test]
    fn test_flush() {
        let config = ResamplerConfig::new(44100, 44100, SampleFormat::F32, SampleFormat::F32);
        let mut resampler = Resampler::new(config).unwrap();
        assert!(resampler.flush().unwrap().is_none());
    }

    fn s16_stereo_frame(rate: u32, samples: &[i16]) -> Frame {
        assert_eq!(samples.len() % 2, 0);
        let nb = samples.len() / 2;
        let mut data = Vec::with_capacity(samples.len() * 2);
        for s in samples {
            data.extend_from_slice(&s.to_le_bytes());
        }
        Frame {
            data: vec![data],
            linesize: vec![samples.len() * 2],
            width: 0,
            height: 0,
            pixel_format: rsmpeg_util::PixelFormat::None,
            sample_format: SampleFormat::S16,
            sample_rate: rate,
            channels: 2,
            samples: nb,
            pts: Some(0),
            duration: nb as i64,
            time_base: rsmpeg_util::Rational::new(1, rate as i32),
            key_frame: true,
            pict_type: rsmpeg_codec::PictureType::I,
        }
    }

    #[test]
    fn test_resampler_non_silent_s16_stereo_upsample() {
        // Stereo S16 frame at 44100 with a non-zero ramp, resample 44100 → 48000.
        let nb = 441usize;
        let mut interleaved: Vec<i16> = Vec::with_capacity(nb * 2);
        for i in 0..nb {
            interleaved.push((i as i16).wrapping_mul(3));
            interleaved.push(-(i as i16).wrapping_mul(3));
        }
        let frame = s16_stereo_frame(44_100, &interleaved);

        let config = ResamplerConfig::new(44_100, 48_000, SampleFormat::S16, SampleFormat::S16)
            .with_channel_layouts(ChannelLayout::STEREO, ChannelLayout::STEREO);
        let resampler = Resampler::new(config).unwrap();
        let out = resampler.resample(&frame).unwrap();

        // Length: ceil(441 * 48000 / 44100) = 480 samples * 2 channels.
        assert_eq!(out.samples, 480);
        assert_eq!(out.channels, 2);
        assert_eq!(out.sample_format, SampleFormat::S16);

        let plane = &out.data[0];
        assert_eq!(plane.len(), 480 * 2 * 2);

        // Output must be non-silent.
        let mut any_nonzero = false;
        for c in plane.chunks_exact(2) {
            if i16::from_le_bytes([c[0], c[1]]) != 0 {
                any_nonzero = true;
                break;
            }
        }
        assert!(any_nonzero, "resampler must produce non-silent output");
    }

    #[test]
    fn test_resampler_dc_input_constant_output() {
        // A constant (DC) input should map to a constant non-zero output that
        // preserves the level when format/rate are unchanged.
        let nb = 100usize;
        let dc: Vec<i16> = vec![1000; nb * 2];
        let frame = s16_stereo_frame(44_100, &dc);

        // Same rate/format/layout → fast path copies bytes.
        let config = ResamplerConfig::new(44_100, 44_100, SampleFormat::S16, SampleFormat::S16)
            .with_channel_layouts(ChannelLayout::STEREO, ChannelLayout::STEREO);
        let resampler = Resampler::new(config).unwrap();
        let out = resampler.resample(&frame).unwrap();

        assert_eq!(out.samples, nb);
        assert_eq!(out.data[0], frame.data[0]);
    }

    #[test]
    fn test_resampler_f32_to_s16() {
        // F32 stereo input → S16 stereo output at the same rate.
        let nb = 200usize;
        let mut interleaved: Vec<u8> = Vec::with_capacity(nb * 2 * 4);
        for i in 0..nb {
            let l = (i as f32) / (nb as f32); // ramp in [0,1)
            let r = -l;
            interleaved.extend_from_slice(&l.to_le_bytes());
            interleaved.extend_from_slice(&r.to_le_bytes());
        }
        let frame = Frame {
            data: vec![interleaved],
            linesize: vec![nb * 2 * 4],
            width: 0,
            height: 0,
            pixel_format: rsmpeg_util::PixelFormat::None,
            sample_format: SampleFormat::F32,
            sample_rate: 48_000,
            channels: 2,
            samples: nb,
            pts: Some(0),
            duration: nb as i64,
            time_base: rsmpeg_util::Rational::new(1, 48_000),
            key_frame: true,
            pict_type: rsmpeg_codec::PictureType::I,
        };

        let config = ResamplerConfig::new(48_000, 48_000, SampleFormat::F32, SampleFormat::S16)
            .with_channel_layouts(ChannelLayout::STEREO, ChannelLayout::STEREO);
        let resampler = Resampler::new(config).unwrap();
        let out = resampler.resample(&frame).unwrap();

        assert_eq!(out.samples, nb);
        assert_eq!(out.sample_format, SampleFormat::S16);

        let plane = &out.data[0];
        assert_eq!(plane.len(), nb * 2 * 2);
        let mut any_nonzero = false;
        for c in plane.chunks_exact(2) {
            if i16::from_le_bytes([c[0], c[1]]) != 0 {
                any_nonzero = true;
                break;
            }
        }
        assert!(
            any_nonzero,
            "F32→S16 resampler must produce non-silent output"
        );
    }

    #[test]
    fn test_resampler_short_plane_errors() {
        // Declared 10 samples but only 4 bytes (one S16 sample) present.
        let frame = Frame {
            data: vec![vec![0u8; 4]],
            linesize: vec![4],
            width: 0,
            height: 0,
            pixel_format: rsmpeg_util::PixelFormat::None,
            sample_format: SampleFormat::S16,
            sample_rate: 44_100,
            channels: 2,
            samples: 10,
            pts: Some(0),
            duration: 10,
            time_base: rsmpeg_util::Rational::new(1, 44_100),
            key_frame: true,
            pict_type: rsmpeg_codec::PictureType::I,
        };
        let config = ResamplerConfig::new(44_100, 48_000, SampleFormat::S16, SampleFormat::S16)
            .with_channel_layouts(ChannelLayout::STEREO, ChannelLayout::STEREO);
        let resampler = Resampler::new(config).unwrap();
        assert!(resampler.resample(&frame).is_err());
    }
}
