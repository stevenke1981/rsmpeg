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
    /// The input format is taken from the frame: interleaved `U8`, `S16`, `S32`,
    /// `F32` and `F64` are read from the first data plane, and planar `S16P`,
    /// `S32P`, `F32P` and `F64P` from one data plane per channel. The output is
    /// interleaved `U8`, `S16`, `S32`, `F32` or `F64`.
    ///
    /// Samples are processed as `f64`, normalised so that integer full scale is
    /// `[-1, 1)`. When the channel layouts differ, every output channel is the
    /// weighted sum given by its [`ChannelMapping`] row (a row without any
    /// weight copies the first source channel). The sample rate is then
    /// converted by per-channel linear interpolation. Integer outputs are
    /// rounded and clamped; floating-point outputs are not clamped.
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

        let planes = input_planes(frame, src_ch)?;

        // Fast path: identical rate/format/layout (non-planar) → copy bytes.
        if src_rate == dst_rate
            && frame.sample_format == dst_format
            && !frame.sample_format.is_planar()
            && self.config.src_channel_layout == self.config.dst_channel_layout
            && src_ch == dst_ch
        {
            let plane = frame.data.first().map(|d| d.as_slice()).unwrap_or(&[]);
            let mut out = Frame::new_audio(dst_format, dst_rate, dst_ch as u16, nb_in);
            out.data[0] = plane.to_vec();
            out.linesize[0] = plane.len();
            out.pts = frame.pts;
            out.time_base = frame.time_base;
            out.duration = frame.duration;
            return Ok(out);
        }

        let decode = sample_decoder(frame.sample_format).ok_or_else(|| {
            RsError::Unsupported(
                format!(
                    "Resampler: unsupported source sample format {:?}",
                    frame.sample_format
                )
                .into(),
            )
        })?;
        let encode = sample_encoder(dst_format).ok_or_else(|| {
            RsError::Unsupported(
                format!("Resampler: unsupported destination sample format {dst_format:?}").into(),
            )
        })?;

        // Deinterleave into one normalised buffer per source channel.
        let bytes_per = frame.sample_format.bytes();
        let decoded: Vec<Vec<f64>> = if frame.sample_format.is_planar() {
            planes
                .iter()
                .map(|plane| plane.chunks_exact(bytes_per).map(decode).collect())
                .collect()
        } else {
            (0..src_ch)
                .map(|c| {
                    planes[0]
                        .chunks_exact(src_ch * bytes_per)
                        .map(|sample| decode(&sample[c * bytes_per..]))
                        .collect()
                })
                .collect()
        };

        // Remix into one buffer per destination channel; without a layout or
        // channel-count change the decoded buffers are used as they are.
        let mixed: Vec<Vec<f64>> = if self.channel_mapping.is_none() && src_ch == dst_ch {
            decoded
        } else {
            (0..dst_ch)
                .map(|o| {
                    let terms = self.mix_terms(o, src_ch);
                    (0..nb_in)
                        .map(|i| terms.iter().map(|&(c, w)| w * decoded[c][i]).sum())
                        .collect()
                })
                .collect()
        };

        let dst_samples = self.config.estimate_output_samples(nb_in);
        // src samples per dst sample.
        let ratio = src_rate as f64 / dst_rate as f64;
        let last = nb_in - 1;

        // Interpolate and encode the interleaved output.
        let mut encoded: Vec<u8> = Vec::with_capacity(dst_samples * dst_ch * dst_format.bytes());
        for j in 0..dst_samples {
            let src_pos = j as f64 * ratio;
            let i = src_pos.floor() as usize;
            let frac = src_pos - i as f64;
            let i0 = i.min(last);
            let i1 = (i + 1).min(last);
            for channel in &mixed {
                // On a source sample the neighbour must not contribute at all:
                // `0.0 * NaN` would otherwise spread a bad sample backwards.
                let sample = if frac == 0.0 {
                    channel[i0]
                } else {
                    channel[i0] * (1.0 - frac) + channel[i1] * frac
                };
                encode(sample, &mut encoded);
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

    /// The `(source channel, weight)` terms that make up one destination channel
    /// of a frame with `src_ch` (non-zero) channels.
    ///
    /// - Equal layouts: identity mapping.
    /// - Different layouts: every non-zero weight of the [`ChannelMapping`] row.
    ///   If the row has none, the first source channel is copied.
    fn mix_terms(&self, dst_idx: usize, src_ch: usize) -> Vec<(usize, f64)> {
        let Some(mapping) = &self.channel_mapping else {
            return vec![(dst_idx.min(src_ch - 1), 1.0)];
        };
        let terms: Vec<(usize, f64)> = mapping
            .matrix
            .get(dst_idx)
            .into_iter()
            .flat_map(|row| row.iter().copied().take(src_ch).enumerate())
            .filter(|&(_, weight)| weight != 0.0)
            .collect();
        if terms.is_empty() {
            vec![(0, 1.0)]
        } else {
            terms
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

/// The sample bytes of a non-empty `frame` with `channels` channels: one slice
/// for interleaved audio, or one per channel for planar audio. Each slice is
/// trimmed to exactly the declared sample count.
fn input_planes(frame: &Frame, channels: usize) -> RsResult<Vec<&[u8]>> {
    let planar = frame.sample_format.is_planar();
    let (count, per_sample) = if planar { (channels, 1) } else { (1, channels) };
    if planar && frame.data.len() < count {
        return Err(RsError::InvalidData(
            format!(
                "Resampler: planar input has {} plane(s), need {count}",
                frame.data.len()
            )
            .into(),
        ));
    }
    let expected = frame
        .samples
        .checked_mul(per_sample)
        .and_then(|n| n.checked_mul(frame.sample_format.bytes()))
        .ok_or_else(|| RsError::InvalidData("Resampler: input size overflow".into()))?;

    (0..count)
        .map(|index| {
            let plane = frame.data.get(index).map(|d| d.as_slice()).unwrap_or(&[]);
            plane.get(..expected).ok_or_else(|| {
                RsError::InvalidData(
                    format!(
                        "Resampler: input plane {index} too short (have {}, need {expected})",
                        plane.len()
                    )
                    .into(),
                )
            })
        })
        .collect()
}

/// Reads one little-endian sample (at the start of the slice) as normalised `f64`.
type SampleDecoder = fn(&[u8]) -> f64;

/// Appends one normalised sample to an output buffer, little-endian.
type SampleEncoder = fn(f64, &mut Vec<u8>);

/// Full-scale magnitude of signed 32-bit audio.
const S32_SCALE: f64 = 2_147_483_648.0;

fn sample_decoder(format: SampleFormat) -> Option<SampleDecoder> {
    let decode: SampleDecoder = match format {
        SampleFormat::U8 => |b| (f64::from(b[0]) - 128.0) / 128.0,
        SampleFormat::S16 | SampleFormat::S16P => {
            |b| f64::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0
        }
        SampleFormat::S32 | SampleFormat::S32P => {
            |b| f64::from(i32::from_le_bytes([b[0], b[1], b[2], b[3]])) / S32_SCALE
        }
        SampleFormat::F32 | SampleFormat::F32P => {
            |b| f64::from(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        }
        SampleFormat::F64 | SampleFormat::F64P => {
            |b| f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
        }
        SampleFormat::None => return None,
    };
    Some(decode)
}

/// Encoders exist for the interleaved formats only. Float → integer casts
/// saturate, and map NaN to silence.
fn sample_encoder(format: SampleFormat) -> Option<SampleEncoder> {
    let encode: SampleEncoder = match format {
        SampleFormat::U8 => |s, out| {
            let v = ((s * 128.0).round() as i32).saturating_add(128);
            out.push(v.clamp(0, 255) as u8);
        },
        SampleFormat::S16 => |s, out| {
            let v = (s * 32768.0).round() as i16;
            out.extend_from_slice(&v.to_le_bytes());
        },
        SampleFormat::S32 => |s, out| {
            let v = (s * S32_SCALE).round() as i32;
            out.extend_from_slice(&v.to_le_bytes());
        },
        SampleFormat::F32 => |s, out| out.extend_from_slice(&(s as f32).to_le_bytes()),
        SampleFormat::F64 => |s, out| out.extend_from_slice(&s.to_le_bytes()),
        SampleFormat::S16P
        | SampleFormat::S32P
        | SampleFormat::F32P
        | SampleFormat::F64P
        | SampleFormat::None => return None,
    };
    Some(encode)
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

    #[test]
    fn estimate_output_samples_is_exact_at_integer_ratios() {
        let down = ResamplerConfig::new(48_000, 24_000, SampleFormat::S16, SampleFormat::S16);
        assert_eq!(down.estimate_output_samples(480), 240);
        assert_eq!(down.estimate_output_samples(0), 0);

        let up = ResamplerConfig::new(24_000, 48_000, SampleFormat::S16, SampleFormat::S16);
        assert_eq!(up.estimate_output_samples(240), 480);
    }

    #[test]
    fn estimate_output_samples_rounds_up_for_partial_samples() {
        // 441 * 48000 / 44100 = 480 exactly; 1 * 48000 / 44100 = 1.088 → 2.
        let cfg = ResamplerConfig::new(44_100, 48_000, SampleFormat::S16, SampleFormat::S16);
        assert_eq!(cfg.estimate_output_samples(441), 480);
        assert_eq!(cfg.estimate_output_samples(1), 2);
    }

    #[test]
    fn estimate_output_samples_with_zero_source_rate_passes_through() {
        let mut cfg = ResamplerConfig::new(44_100, 48_000, SampleFormat::S16, SampleFormat::S16);
        cfg.src_sample_rate = 0;
        assert_eq!(cfg.estimate_output_samples(123), 123);
    }

    #[test]
    fn empty_input_yields_empty_output_without_error() {
        let frame = Frame::new_audio(SampleFormat::S16, 44_100, 2, 0);
        let cfg = ResamplerConfig::new(44_100, 48_000, SampleFormat::S16, SampleFormat::S16)
            .with_channel_layouts(ChannelLayout::STEREO, ChannelLayout::STEREO);
        let out = Resampler::new(cfg).unwrap().resample(&frame).unwrap();
        assert_eq!(out.samples, 0);
        assert_eq!(out.sample_rate, 48_000);
        assert_eq!(out.channels, 2);
    }

    #[test]
    fn zero_channel_count_is_rejected() {
        let mut frame = Frame::new_audio(SampleFormat::S16, 44_100, 2, 8);
        frame.channels = 0;
        let cfg = ResamplerConfig::new(44_100, 48_000, SampleFormat::S16, SampleFormat::S16);
        assert!(Resampler::new(cfg).unwrap().resample(&frame).is_err());
    }

    #[test]
    fn unsupported_source_format_is_rejected() {
        let mut frame = Frame::new_audio(SampleFormat::S16, 44_100, 2, 8);
        // `None` names no sample encoding, so there is nothing to decode.
        frame.sample_format = SampleFormat::None;
        let cfg = ResamplerConfig::new(44_100, 48_000, SampleFormat::None, SampleFormat::S16)
            .with_channel_layouts(ChannelLayout::STEREO, ChannelLayout::STEREO);
        let err = Resampler::new(cfg).unwrap().resample(&frame).unwrap_err();
        assert!(
            matches!(err, RsError::Unsupported(_)),
            "expected Unsupported, got {err:?}"
        );
    }

    #[test]
    fn fast_path_preserves_pts_and_time_base() {
        let frame = s16_stereo_frame(48_000, &[1, 2, 3, 4]);
        let cfg = ResamplerConfig::new(48_000, 48_000, SampleFormat::S16, SampleFormat::S16)
            .with_channel_layouts(ChannelLayout::STEREO, ChannelLayout::STEREO);
        let out = Resampler::new(cfg).unwrap().resample(&frame).unwrap();
        assert_eq!(out.pts, frame.pts);
        assert_eq!(out.time_base, frame.time_base);
        assert_eq!(out.samples, frame.samples);
        assert_eq!(out.data[0], frame.data[0]);
    }

    #[test]
    fn stereo_to_mono_drops_to_one_channel() {
        let nb = 100usize;
        let mut interleaved = Vec::with_capacity(nb * 2);
        for i in 0..nb {
            interleaved.push(1000i16 + i as i16);
            interleaved.push(1000i16 + i as i16);
        }
        let frame = s16_stereo_frame(48_000, &interleaved);
        let cfg = ResamplerConfig::new(48_000, 48_000, SampleFormat::S16, SampleFormat::S16)
            .with_channel_layouts(ChannelLayout::STEREO, ChannelLayout::MONO);
        let out = Resampler::new(cfg).unwrap().resample(&frame).unwrap();
        assert_eq!(out.channels, 1);
        assert_eq!(out.samples, nb);
        // One plane of nb mono S16 samples.
        assert_eq!(out.data[0].len(), nb * 2);
        assert!(out.data[0].chunks_exact(2).any(|c| c[0] != 0));
    }

    #[test]
    fn compression_ratio_handles_zero_source_rate() {
        let mut cfg = ResamplerConfig::new(48_000, 24_000, SampleFormat::S16, SampleFormat::S16);
        cfg.src_sample_rate = 0;
        let resampler = Resampler::new(cfg).unwrap();
        assert_eq!(resampler.compression_ratio(), 1.0);
    }

    #[test]
    fn flush_reports_no_delay_by_default() {
        let cfg = ResamplerConfig::new(48_000, 48_000, SampleFormat::S16, SampleFormat::S16);
        let mut resampler = Resampler::new(cfg).unwrap();
        assert_eq!(resampler.delay(), 0);
        assert!(resampler.flush().unwrap().is_none());
    }

    /// A 48 kHz frame holding `data` planes of `format` samples.
    fn audio_frame(format: SampleFormat, channels: u16, data: Vec<Vec<u8>>) -> Frame {
        let per_plane = if format.is_planar() {
            1
        } else {
            usize::from(channels)
        };
        let samples = data[0].len() / (per_plane * format.bytes());
        let mut frame = Frame::new_audio(format, 48_000, channels, 0);
        frame.samples = samples;
        frame.linesize = data.iter().map(Vec::len).collect();
        frame.data = data;
        frame.pts = Some(7);
        frame
    }

    fn bytes_of<T: Copy, const N: usize>(values: &[T], to_le: fn(T) -> [u8; N]) -> Vec<u8> {
        values.iter().flat_map(|&v| to_le(v)).collect()
    }

    /// Convert `frame` at 48 kHz between equally sized layouts (or `layouts`).
    fn convert_with(
        frame: &Frame,
        dst: SampleFormat,
        layouts: (ChannelLayout, ChannelLayout),
    ) -> Frame {
        let cfg = ResamplerConfig::new(48_000, 48_000, frame.sample_format, dst)
            .with_channel_layouts(layouts.0, layouts.1);
        Resampler::new(cfg).unwrap().resample(frame).unwrap()
    }

    fn convert(frame: &Frame, dst: SampleFormat) -> Frame {
        let layout = if frame.channels == 1 {
            ChannelLayout::MONO
        } else {
            ChannelLayout::STEREO
        };
        convert_with(frame, dst, (layout, layout))
    }

    fn s16_samples(frame: &Frame) -> Vec<i16> {
        frame.data[0]
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect()
    }

    #[test]
    fn f64_input_converts_to_s16_with_rounding_and_clamping() {
        let values = [0.0f64, 0.5, -0.5, 1.0, -1.0, 2.0, -2.0, f64::NAN];
        let frame = audio_frame(
            SampleFormat::F64,
            2,
            vec![bytes_of(&values, f64::to_le_bytes)],
        );
        let out = convert(&frame, SampleFormat::S16);
        assert_eq!((out.samples, out.channels, out.pts), (4, 2, Some(7)));
        assert_eq!(
            s16_samples(&out),
            vec![0, 16384, -16384, i16::MAX, i16::MIN, i16::MAX, i16::MIN, 0]
        );
    }

    #[test]
    fn u8_input_and_output_are_centred_on_128() {
        let frame = audio_frame(SampleFormat::U8, 2, vec![vec![0, 128, 255, 64]]);
        assert_eq!(
            s16_samples(&convert(&frame, SampleFormat::S16)),
            vec![i16::MIN, 0, 32512, -16384]
        );

        let values = [0.0f32, 1.0, -1.0, f32::NAN];
        let frame = audio_frame(
            SampleFormat::F32,
            2,
            vec![bytes_of(&values, f32::to_le_bytes)],
        );
        assert_eq!(
            convert(&frame, SampleFormat::U8).data[0],
            vec![128, 255, 0, 128]
        );
    }

    #[test]
    fn s32_round_trips_bit_exactly_through_f64() {
        let values = [i32::MIN, i32::MAX, -1, 1, 0x1234_5678, -0x1234_5678];
        let frame = audio_frame(
            SampleFormat::S32,
            2,
            vec![bytes_of(&values, i32::to_le_bytes)],
        );
        let wide = convert(&frame, SampleFormat::F64);
        assert_eq!(wide.sample_format, SampleFormat::F64);
        let back = convert(&wide, SampleFormat::S32);
        assert_eq!(back.data[0], frame.data[0]);
        assert_eq!(back.linesize, vec![values.len() * 4]);
    }

    #[test]
    fn planar_s16_and_s32_are_interleaved() {
        let left = [1i16, 2, 3];
        let right = [-1i16, -2, -3];
        let frame = audio_frame(
            SampleFormat::S16P,
            2,
            vec![
                bytes_of(&left, i16::to_le_bytes),
                bytes_of(&right, i16::to_le_bytes),
            ],
        );
        let out = convert(&frame, SampleFormat::S16);
        assert_eq!(out.sample_format, SampleFormat::S16);
        assert_eq!(s16_samples(&out), vec![1, -1, 2, -2, 3, -3]);

        let frame = audio_frame(
            SampleFormat::S32P,
            2,
            vec![
                bytes_of(&[1i32 << 16, 2 << 16], i32::to_le_bytes),
                bytes_of(&[-1i32 << 16, -2 << 16], i32::to_le_bytes),
            ],
        );
        assert_eq!(
            s16_samples(&convert(&frame, SampleFormat::S16)),
            vec![1, -1, 2, -2]
        );
    }

    #[test]
    fn planar_float_inputs_match_their_interleaved_form() {
        let interleaved = [0.25f32, -0.25, 0.5, -0.5];
        let frame = audio_frame(
            SampleFormat::F32P,
            2,
            vec![
                bytes_of(&[0.25f32, 0.5], f32::to_le_bytes),
                bytes_of(&[-0.25f32, -0.5], f32::to_le_bytes),
            ],
        );
        assert_eq!(
            convert(&frame, SampleFormat::F32).data[0],
            bytes_of(&interleaved, f32::to_le_bytes)
        );

        let frame = audio_frame(
            SampleFormat::F64P,
            2,
            vec![
                bytes_of(&[0.25f64, 0.5], f64::to_le_bytes),
                bytes_of(&[-0.25f64, -0.5], f64::to_le_bytes),
            ],
        );
        assert_eq!(
            convert(&frame, SampleFormat::F32).data[0],
            bytes_of(&interleaved, f32::to_le_bytes)
        );
    }

    #[test]
    fn planar_input_is_resampled_per_channel() {
        let frame = audio_frame(
            SampleFormat::S16P,
            2,
            vec![
                bytes_of(&[0i16, 100], i16::to_le_bytes),
                bytes_of(&[0i16, -100], i16::to_le_bytes),
            ],
        );
        let cfg = ResamplerConfig::new(48_000, 96_000, SampleFormat::S16P, SampleFormat::S16);
        let out = Resampler::new(cfg).unwrap().resample(&frame).unwrap();
        assert_eq!(out.samples, 4);
        assert_eq!(s16_samples(&out), vec![0, 0, 50, -50, 100, -100, 100, -100]);
    }

    #[test]
    fn planar_input_with_missing_or_short_planes_errors() {
        let cfg = ResamplerConfig::new(48_000, 44_100, SampleFormat::S16P, SampleFormat::S16);
        let resampler = Resampler::new(cfg).unwrap();
        let good = audio_frame(SampleFormat::S16P, 2, vec![vec![0; 8], vec![0; 8]]);
        assert!(resampler.resample(&good).is_ok());

        let mut one_plane = good.clone();
        one_plane.data.pop();
        assert!(matches!(
            resampler.resample(&one_plane),
            Err(RsError::InvalidData(_))
        ));

        let mut short = good;
        short.data[1].truncate(7);
        assert!(matches!(
            resampler.resample(&short),
            Err(RsError::InvalidData(_))
        ));
    }

    #[test]
    fn planar_destination_formats_are_rejected() {
        let frame = s16_stereo_frame(48_000, &[1, 2, 3, 4]);
        for dst in [SampleFormat::S16P, SampleFormat::F32P, SampleFormat::None] {
            let cfg = ResamplerConfig::new(48_000, 44_100, SampleFormat::S16, dst);
            assert!(matches!(
                Resampler::new(cfg).unwrap().resample(&frame),
                Err(RsError::Unsupported(_))
            ));
        }
    }

    #[test]
    fn stereo_to_mono_is_the_average_of_both_channels() {
        let frame = s16_stereo_frame(48_000, &[1000, 3000, -400, 200, i16::MIN, i16::MIN]);
        let out = convert_with(
            &frame,
            SampleFormat::S16,
            (ChannelLayout::STEREO, ChannelLayout::MONO),
        );
        assert_eq!(out.channels, 1);
        assert_eq!(s16_samples(&out), vec![2000, -100, i16::MIN]);
    }

    #[test]
    fn mono_upmix_duplicates_the_channel() {
        let frame = audio_frame(
            SampleFormat::S16,
            1,
            vec![bytes_of(&[5i16, -9], i16::to_le_bytes)],
        );
        let out = convert_with(
            &frame,
            SampleFormat::S16,
            (ChannelLayout::MONO, ChannelLayout::STEREO),
        );
        assert_eq!(s16_samples(&out), vec![5, 5, -9, -9]);
    }

    #[test]
    fn five_point_one_downmixes_to_stereo_with_weights() {
        // FL, FR, FC, LFE, BL, BR
        let input = [0.1f64, 0.2, 0.3, 0.9, 0.4, 0.5];
        let frame = audio_frame(
            SampleFormat::F64,
            6,
            vec![bytes_of(&input, f64::to_le_bytes)],
        );
        let out = convert_with(
            &frame,
            SampleFormat::F64,
            (ChannelLayout::_5POINT1, ChannelLayout::STEREO),
        );
        assert_eq!((out.channels, out.samples), (2, 1));
        let got: Vec<f64> = out.data[0]
            .chunks_exact(8)
            .map(|c| f64::from_le_bytes(c.try_into().unwrap()))
            .collect();
        let k = std::f64::consts::FRAC_1_SQRT_2;
        let norm = 1.0 + 2.0 * k;
        let want = [
            (0.1 + k * 0.3 + k * 0.4) / norm,
            (0.2 + k * 0.3 + k * 0.5) / norm,
        ];
        for (got, want) in got.iter().zip(want) {
            assert!((got - want).abs() < 1e-12, "{got} vs {want}");
        }
    }

    #[test]
    fn full_scale_five_point_one_downmix_does_not_clip() {
        let frame = audio_frame(
            SampleFormat::S16,
            6,
            vec![bytes_of(&[i16::MAX; 6], i16::to_le_bytes)],
        );
        let out = convert_with(
            &frame,
            SampleFormat::S16,
            (ChannelLayout::_5POINT1, ChannelLayout::STEREO),
        );
        assert_eq!(s16_samples(&out), vec![i16::MAX, i16::MAX]);
    }

    #[test]
    fn all_zero_matrix_rows_copy_the_first_channel() {
        let frame = s16_stereo_frame(48_000, &[11, 22]);
        let out = convert_with(
            &frame,
            SampleFormat::S16,
            (ChannelLayout::STEREO, ChannelLayout::_5POINT1),
        );
        assert_eq!(s16_samples(&out), vec![11, 22, 11, 11, 11, 11]);
    }

    #[test]
    fn fast_path_covers_every_interleaved_format() {
        for format in [
            SampleFormat::U8,
            SampleFormat::S32,
            SampleFormat::F32,
            SampleFormat::F64,
        ] {
            let bytes: Vec<u8> = (0..4 * format.bytes()).map(|i| i as u8).collect();
            let frame = audio_frame(format, 2, vec![bytes.clone()]);
            assert_eq!(convert(&frame, format).data[0], bytes, "{format:?}");
        }
    }
}
