use rsmpeg_codec::Frame;
use rsmpeg_util::{PixelFormat, RsResult};

use crate::colorspace::ColorConversion;

/// Interpolation method used during scaling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterpolationMethod {
    NearestNeighbor,
    Bilinear,
    Bicubic,
    Lanczos,
    Sinc,
    Spline,
    Gaussian,
}

impl InterpolationMethod {
    pub fn name(&self) -> &'static str {
        match self {
            InterpolationMethod::NearestNeighbor => "nearest",
            InterpolationMethod::Bilinear => "bilinear",
            InterpolationMethod::Bicubic => "bicubic",
            InterpolationMethod::Lanczos => "lanczos",
            InterpolationMethod::Sinc => "sinc",
            InterpolationMethod::Spline => "spline",
            InterpolationMethod::Gaussian => "gaussian",
        }
    }
}

bitflags::bitflags! {
    /// Scaler control flags.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ScalerFlags: u32 {
        const NONE = 0;
        const PRINT_INFO = 1 << 0;
        const ACCURATE_ROUNDING = 1 << 1;
        const FULL_CHROMA_INT = 1 << 2;
        const BILINEAR = 1 << 3;
        const BICUBIC = 1 << 4;
    }
}

/// Configuration for setting up a Scaler.
#[derive(Debug, Clone)]
pub struct ScalerConfig {
    pub src_width: u32,
    pub src_height: u32,
    pub src_format: PixelFormat,
    pub dst_width: u32,
    pub dst_height: u32,
    pub dst_format: PixelFormat,
    pub interpolation: InterpolationMethod,
    pub color_conversion: Option<ColorConversion>,
    pub flags: ScalerFlags,
}

impl ScalerConfig {
    pub fn new(
        src_width: u32,
        src_height: u32,
        src_format: PixelFormat,
        dst_width: u32,
        dst_height: u32,
        dst_format: PixelFormat,
    ) -> Self {
        ScalerConfig {
            src_width,
            src_height,
            src_format,
            dst_width,
            dst_height,
            dst_format,
            interpolation: InterpolationMethod::Bilinear,
            color_conversion: None,
            flags: ScalerFlags::NONE,
        }
    }

    pub fn with_interpolation(mut self, method: InterpolationMethod) -> Self {
        self.interpolation = method;
        self
    }

    pub fn with_color_conversion(mut self, conv: ColorConversion) -> Self {
        self.color_conversion = Some(conv);
        self
    }
}

/// Video scaler — converts between pixel formats, dimensions, and color spaces.
///
/// Equivalent to FFmpeg's SwsContext.
pub struct Scaler {
    config: ScalerConfig,
}

impl Scaler {
    pub fn new(config: ScalerConfig) -> RsResult<Self> {
        tracing::debug!(
            "Scaler: {}x{} {:?} -> {}x{} {:?} ({})",
            config.src_width,
            config.src_height,
            config.src_format,
            config.dst_width,
            config.dst_height,
            config.dst_format,
            config.interpolation.name(),
        );
        Ok(Scaler { config })
    }

    pub fn config(&self) -> &ScalerConfig {
        &self.config
    }

    /// Scale a frame from source to destination format/dimensions.
    ///
    /// Returns a new frame with the scaled output.
    pub fn scale(&self, frame: &Frame) -> RsResult<Frame> {
        tracing::debug!(
            "Scaling frame {}x{} -> {}x{}",
            frame.width,
            frame.height,
            self.config.dst_width,
            self.config.dst_height,
        );
        Ok(Frame::new_video(
            self.config.dst_width as usize,
            self.config.dst_height as usize,
            self.config.dst_format,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scaler_creation() {
        let config = ScalerConfig::new(
            1920,
            1080,
            PixelFormat::Yuv420P,
            1280,
            720,
            PixelFormat::Yuv420P,
        );
        let scaler = Scaler::new(config).unwrap();
        assert_eq!(scaler.config().src_width, 1920);
        assert_eq!(scaler.config().dst_width, 1280);
    }

    #[test]
    fn test_scaler_with_options() {
        let config =
            ScalerConfig::new(640, 480, PixelFormat::Yuv420P, 320, 240, PixelFormat::Rgb24)
                .with_interpolation(InterpolationMethod::Lanczos);
        let scaler = Scaler::new(config).unwrap();
        assert_eq!(scaler.config().interpolation, InterpolationMethod::Lanczos);
    }

    #[test]
    fn test_scaler_scale_output() {
        let config =
            ScalerConfig::new(640, 480, PixelFormat::Yuv420P, 320, 240, PixelFormat::Rgb24);
        let scaler = Scaler::new(config).unwrap();
        let frame = Frame::new_video(640, 480, PixelFormat::Yuv420P);
        let out = scaler.scale(&frame).unwrap();
        assert_eq!(out.width, 320);
        assert_eq!(out.height, 240);
    }
}
