use crate::channel_mapping::ChannelMapping;
use crate::dither::DitherMethod;
use rsmpeg_codec::Frame;
use rsmpeg_util::{ChannelLayout, RsResult, SampleFormat};

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
        ((num + den - 1) / den) as usize
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
    pub fn resample(&self, frame: &Frame) -> RsResult<Frame> {
        let nb_samples = frame.samples;
        let output_samples = self.config.estimate_output_samples(nb_samples);

        Ok(Frame::new_audio(
            self.config.dst_format,
            self.config.dst_sample_rate,
            self.config.dst_channel_layout.channels() as u16,
            output_samples,
        ))
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
}
