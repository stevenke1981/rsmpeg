use rsmpeg_codec::Frame;
use rsmpeg_util::{unsupported, PixelFormat, Rational, RsError, RsResult};

use crate::colorspace::{ColorConversion, ColorRange, ColorSpace};

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
/// Equivalent to FFmpeg's SwsContext. Reuses the stored [`ScalerConfig`] across
/// frames; construct a new [`Scaler`] only when dimensions or formats change.
pub struct Scaler {
    config: ScalerConfig,
    /// Effective color conversion (default BT.601 limited → full RGB).
    color: ColorConversion,
}

impl Scaler {
    pub fn new(config: ScalerConfig) -> RsResult<Self> {
        if config.src_width == 0 || config.src_height == 0 {
            return Err(RsError::InvalidData(
                "scaler source size must be non-zero".into(),
            ));
        }
        if config.dst_width == 0 || config.dst_height == 0 {
            return Err(RsError::InvalidData(
                "scaler destination size must be non-zero".into(),
            ));
        }

        let color = config.color_conversion.clone().unwrap_or_else(|| {
            ColorConversion::new(
                ColorSpace::BT601,
                ColorSpace::RGB,
                ColorRange::Limited,
                ColorRange::Full,
            )
        });

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
        Ok(Scaler { config, color })
    }

    pub fn config(&self) -> &ScalerConfig {
        &self.config
    }

    /// Scale a frame from source to destination format/dimensions.
    ///
    /// Currently implements real conversion for:
    /// - `Yuv420P` → `Rgba` / `Rgb24` (BT.601 limited range by default)
    ///
    /// Dimension changes use nearest-neighbor sampling of the source.
    /// Returns a new frame with correctly sized packed planes.
    pub fn scale(&self, frame: &Frame) -> RsResult<Frame> {
        tracing::debug!(
            "Scaling frame {}x{} -> {}x{}",
            frame.width,
            frame.height,
            self.config.dst_width,
            self.config.dst_height,
        );

        match (self.config.src_format, self.config.dst_format) {
            (PixelFormat::Yuv420P, PixelFormat::Rgba) => self.scale_yuv420p_to_packed(frame, 4),
            (PixelFormat::Yuv420P, PixelFormat::Rgb24) => self.scale_yuv420p_to_packed(frame, 3),
            (src, dst)
                if src == dst
                    && self.config.src_width == self.config.dst_width
                    && self.config.src_height == self.config.dst_height =>
            {
                // Same format / size: shallow-ish copy of planes.
                Ok(clone_frame_metadata(
                    frame,
                    frame.data.clone(),
                    frame.linesize.clone(),
                ))
            }
            (src, dst) => Err(unsupported!(
                "scale conversion {:?} -> {:?} is not implemented yet",
                src,
                dst
            )),
        }
    }

    fn scale_yuv420p_to_packed(&self, frame: &Frame, bpp: usize) -> RsResult<Frame> {
        if frame.data.len() < 3 || frame.linesize.len() < 3 {
            return Err(RsError::InvalidData(
                "YUV420P frame requires 3 planes (Y, U, V)".into(),
            ));
        }

        let src_w = self.config.src_width as usize;
        let src_h = self.config.src_height as usize;
        let dst_w = self.config.dst_width as usize;
        let dst_h = self.config.dst_height as usize;

        // Prefer configured size; fall back to frame metadata when they match.
        let (use_w, use_h) = if frame.width == src_w && frame.height == src_h {
            (src_w, src_h)
        } else if frame.width > 0 && frame.height > 0 {
            // Allow slight mismatch if the frame carries the true size.
            (frame.width, frame.height)
        } else {
            (src_w, src_h)
        };

        let y_plane = &frame.data[0];
        let u_plane = &frame.data[1];
        let v_plane = &frame.data[2];
        let y_stride = frame.linesize[0];
        let u_stride = frame.linesize[1];
        let v_stride = frame.linesize[2];

        // Chroma dimensions for 4:2:0.
        let chroma_w = (use_w + 1) / 2;
        let chroma_h = (use_h + 1) / 2;

        // Validate plane capacity (allow extra padding in linesize).
        let y_need = y_stride
            .checked_mul(use_h.saturating_sub(1))
            .and_then(|o| o.checked_add(use_w));
        let u_need = u_stride
            .checked_mul(chroma_h.saturating_sub(1))
            .and_then(|o| o.checked_add(chroma_w));
        let v_need = v_stride
            .checked_mul(chroma_h.saturating_sub(1))
            .and_then(|o| o.checked_add(chroma_w));

        match (y_need, u_need, v_need) {
            (Some(yn), Some(un), Some(vn))
                if y_plane.len() >= yn && u_plane.len() >= un && v_plane.len() >= vn => {}
            _ => {
                return Err(RsError::InvalidData(
                    "YUV420P plane buffers are too small for declared size/linesize".into(),
                ));
            }
        }

        let limited = self.color.src_range == ColorRange::Limited;
        let mut out = vec![0u8; dst_w * dst_h * bpp];
        let out_stride = dst_w * bpp;

        // Nearest-neighbor map destination → source.
        for dy in 0..dst_h {
            let sy = if dst_h == use_h {
                dy
            } else {
                (dy as u64 * use_h as u64 / dst_h as u64) as usize
            };
            let sy = sy.min(use_h - 1);
            let cy = sy / 2;

            for dx in 0..dst_w {
                let sx = if dst_w == use_w {
                    dx
                } else {
                    (dx as u64 * use_w as u64 / dst_w as u64) as usize
                };
                let sx = sx.min(use_w - 1);
                let cx = sx / 2;

                let y = y_plane[sy * y_stride + sx];
                let u = u_plane[cy * u_stride + cx];
                let v = v_plane[cy * v_stride + cx];

                let (r, g, b) = yuv_to_rgb_bt601(y, u, v, limited);
                let off = dy * out_stride + dx * bpp;
                out[off] = r;
                out[off + 1] = g;
                out[off + 2] = b;
                if bpp == 4 {
                    out[off + 3] = 255;
                }
            }
        }

        let mut result = Frame {
            data: vec![out],
            linesize: vec![out_stride],
            width: dst_w,
            height: dst_h,
            pixel_format: self.config.dst_format,
            sample_format: frame.sample_format,
            sample_rate: 0,
            channels: 0,
            samples: 0,
            pts: frame.pts,
            duration: frame.duration,
            time_base: frame.time_base,
            key_frame: frame.key_frame,
            pict_type: frame.pict_type,
        };
        // Keep audio fields zeroed for video output; ensure time_base is valid.
        if result.time_base.den == 0 {
            result.time_base = Rational::new(1, 1000);
        }
        Ok(result)
    }
}

/// BT.601 YUV → RGB conversion.
///
/// Limited range uses studio swing (Y 16–235, UV 16–240).
/// Full range treats Y/U/V as 0–255 with neutral chroma at 128.
fn yuv_to_rgb_bt601(y: u8, u: u8, v: u8, limited: bool) -> (u8, u8, u8) {
    let (yf, uf, vf) = if limited {
        let yf = ((y as f32) - 16.0).max(0.0) * (255.0 / 219.0);
        let uf = ((u as f32) - 128.0) * (255.0 / 224.0);
        let vf = ((v as f32) - 128.0) * (255.0 / 224.0);
        (yf, uf, vf)
    } else {
        (y as f32, (u as f32) - 128.0, (v as f32) - 128.0)
    };

    // BT.601 inverse matrix (Kr=0.299, Kb=0.114).
    let r = yf + 1.402 * vf;
    let g = yf - 0.344136 * uf - 0.714136 * vf;
    let b = yf + 1.772 * uf;

    (clamp_u8(r), clamp_u8(g), clamp_u8(b))
}

fn clamp_u8(v: f32) -> u8 {
    if v <= 0.0 {
        0
    } else if v >= 255.0 {
        255
    } else {
        (v + 0.5) as u8
    }
}

fn clone_frame_metadata(frame: &Frame, data: Vec<Vec<u8>>, linesize: Vec<usize>) -> Frame {
    Frame {
        data,
        linesize,
        width: frame.width,
        height: frame.height,
        pixel_format: frame.pixel_format,
        sample_format: frame.sample_format,
        sample_rate: frame.sample_rate,
        channels: frame.channels,
        samples: frame.samples,
        pts: frame.pts,
        duration: frame.duration,
        time_base: frame.time_base,
        key_frame: frame.key_frame,
        pict_type: frame.pict_type,
    }
}

/// Build a correctly sized synthetic YUV420P frame (not using broken `Frame::new_video` plane sizes).
#[cfg(test)]
pub(crate) fn make_yuv420p_frame(width: usize, height: usize, y: u8, u: u8, v: u8) -> Frame {
    let y_size = width * height;
    let chroma_w = (width + 1) / 2;
    let chroma_h = (height + 1) / 2;
    let c_size = chroma_w * chroma_h;
    Frame {
        data: vec![vec![y; y_size], vec![u; c_size], vec![v; c_size]],
        linesize: vec![width, chroma_w, chroma_w],
        width,
        height,
        pixel_format: PixelFormat::Yuv420P,
        sample_format: rsmpeg_util::SampleFormat::None,
        sample_rate: 0,
        channels: 0,
        samples: 0,
        pts: Some(0),
        duration: 1,
        time_base: Rational::new(1, 25),
        key_frame: true,
        pict_type: rsmpeg_codec::PictureType::I,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::colorspace::{ColorConversion, ColorRange, ColorSpace};

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
    fn test_scaler_scale_output_rgb24() {
        let config =
            ScalerConfig::new(640, 480, PixelFormat::Yuv420P, 320, 240, PixelFormat::Rgb24);
        let scaler = Scaler::new(config).unwrap();
        // Manual planes — Frame::new_video sizes are wrong for YUV420P chroma.
        let frame = make_yuv420p_frame(640, 480, 16, 128, 128);
        let out = scaler.scale(&frame).unwrap();
        assert_eq!(out.width, 320);
        assert_eq!(out.height, 240);
        assert_eq!(out.pixel_format, PixelFormat::Rgb24);
        assert_eq!(out.data[0].len(), 320 * 240 * 3);
        assert_eq!(out.linesize[0], 320 * 3);
    }

    #[test]
    fn test_yuv420p_to_rgba_buffer_len_and_black() {
        let w = 8usize;
        let h = 6usize;
        let config = ScalerConfig::new(
            w as u32,
            h as u32,
            PixelFormat::Yuv420P,
            w as u32,
            h as u32,
            PixelFormat::Rgba,
        );
        let scaler = Scaler::new(config).unwrap();
        // Limited-range black: Y=16, U=V=128
        let frame = make_yuv420p_frame(w, h, 16, 128, 128);
        let out = scaler.scale(&frame).unwrap();
        assert_eq!(out.data[0].len(), w * h * 4);
        // First pixel ≈ black
        assert!(out.data[0][0] <= 2, "R={}", out.data[0][0]);
        assert!(out.data[0][1] <= 2, "G={}", out.data[0][1]);
        assert!(out.data[0][2] <= 2, "B={}", out.data[0][2]);
        assert_eq!(out.data[0][3], 255);
    }

    #[test]
    fn test_yuv420p_to_rgba_approx_red() {
        let w = 4usize;
        let h = 4usize;
        let config = ScalerConfig::new(
            w as u32,
            h as u32,
            PixelFormat::Yuv420P,
            w as u32,
            h as u32,
            PixelFormat::Rgba,
        );
        let scaler = Scaler::new(config).unwrap();
        // Classic BT.601 limited red ≈ Y=81, U=90, V=240
        let frame = make_yuv420p_frame(w, h, 81, 90, 240);
        let out = scaler.scale(&frame).unwrap();
        let r = out.data[0][0];
        let g = out.data[0][1];
        let b = out.data[0][2];
        assert!(r > 200, "expected strong red, got R={r}");
        assert!(g < 40, "expected low green, got G={g}");
        assert!(b < 40, "expected low blue, got B={b}");
    }

    #[test]
    fn test_yuv420p_gradient_rgba_len() {
        let w = 16usize;
        let h = 8usize;
        let chroma_w = (w + 1) / 2;
        let chroma_h = (h + 1) / 2;
        let mut y = vec![0u8; w * h];
        for row in 0..h {
            for col in 0..w {
                y[row * w + col] = (16 + (col * 219 / (w - 1))) as u8;
            }
        }
        let frame = Frame {
            data: vec![
                y,
                vec![128u8; chroma_w * chroma_h],
                vec![128u8; chroma_w * chroma_h],
            ],
            linesize: vec![w, chroma_w, chroma_w],
            width: w,
            height: h,
            pixel_format: PixelFormat::Yuv420P,
            sample_format: rsmpeg_util::SampleFormat::None,
            sample_rate: 0,
            channels: 0,
            samples: 0,
            pts: None,
            duration: 0,
            time_base: Rational::new(1, 1000),
            key_frame: false,
            pict_type: rsmpeg_codec::PictureType::None,
        };
        let config = ScalerConfig::new(
            w as u32,
            h as u32,
            PixelFormat::Yuv420P,
            w as u32,
            h as u32,
            PixelFormat::Rgba,
        );
        let scaler = Scaler::new(config).unwrap();
        let out = scaler.scale(&frame).unwrap();
        assert_eq!(out.data[0].len(), w * h * 4);
        // Left ≈ dark, right ≈ bright (luma gradient, neutral chroma)
        let left = out.data[0][0];
        let right = out.data[0][(w - 1) * 4];
        assert!(right > left + 100, "left={left} right={right}");
    }

    #[test]
    fn test_full_range_white() {
        let w = 2usize;
        let h = 2usize;
        let config = ScalerConfig::new(
            w as u32,
            h as u32,
            PixelFormat::Yuv420P,
            w as u32,
            h as u32,
            PixelFormat::Rgba,
        )
        .with_color_conversion(ColorConversion::new(
            ColorSpace::BT601,
            ColorSpace::RGB,
            ColorRange::Full,
            ColorRange::Full,
        ));
        let scaler = Scaler::new(config).unwrap();
        let frame = make_yuv420p_frame(w, h, 255, 128, 128);
        let out = scaler.scale(&frame).unwrap();
        assert!(out.data[0][0] >= 250);
        assert!(out.data[0][1] >= 250);
        assert!(out.data[0][2] >= 250);
    }

    #[test]
    fn test_reuses_config_across_frames() {
        let config = ScalerConfig::new(4, 4, PixelFormat::Yuv420P, 4, 4, PixelFormat::Rgba);
        let scaler = Scaler::new(config).unwrap();
        let f1 = make_yuv420p_frame(4, 4, 16, 128, 128);
        let f2 = make_yuv420p_frame(4, 4, 235, 128, 128);
        let o1 = scaler.scale(&f1).unwrap();
        let o2 = scaler.scale(&f2).unwrap();
        assert_eq!(o1.data[0].len(), 4 * 4 * 4);
        assert_eq!(o2.data[0].len(), 4 * 4 * 4);
        assert!(o2.data[0][0] > o1.data[0][0]);
    }
}
