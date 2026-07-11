//! Thin helpers that convert decoded video frames via [`rsmpeg_scale::Scaler`].

use rsmpeg_codec::Frame;
use rsmpeg_scale::{Scaler, ScalerConfig};
use rsmpeg_util::{PixelFormat, RsError, RsResult};

/// Convert a YUV420P [`Frame`] to a tightly packed RGBA buffer (`w * h * 4`).
///
/// Constructs a reusable-style [`Scaler`] for the frame's dimensions (caller may
/// later cache a Scaler themselves for hot paths). Plane sizes are read from
/// the frame; they must be large enough for true YUV420P layout
/// (`Y: w*h`, `U/V: ceil(w/2)*ceil(h/2)` with matching linesizes).
pub fn yuv420p_frame_to_rgba(frame: &Frame) -> RsResult<Vec<u8>> {
    if frame.pixel_format != PixelFormat::Yuv420P {
        return Err(RsError::InvalidData(
            "yuv420p_frame_to_rgba requires PixelFormat::Yuv420P".into(),
        ));
    }
    if frame.width == 0 || frame.height == 0 {
        return Err(RsError::InvalidData(
            "yuv420p_frame_to_rgba requires non-zero dimensions".into(),
        ));
    }

    let config = ScalerConfig::new(
        frame.width as u32,
        frame.height as u32,
        PixelFormat::Yuv420P,
        frame.width as u32,
        frame.height as u32,
        PixelFormat::Rgba,
    );
    let scaler = Scaler::new(config)?;
    let out = scaler.scale(frame)?;
    out.data
        .into_iter()
        .next()
        .ok_or_else(|| RsError::Bug("scaler produced empty RGBA plane list".into()))
}

/// Cached variant of [`yuv420p_frame_to_rgba`] that reuses a per-resolution
/// [`rsmpeg_scale::Scaler`] (see [`crate::scaler_cache`]). Byte-identical
/// output to [`yuv420p_frame_to_rgba`] for the same input frame.
pub fn yuv420p_frame_to_rgba_cached(frame: &Frame) -> RsResult<Vec<u8>> {
    crate::scaler_cache::yuv420p_to_rgba_cached(frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame_pool::FramePool;
    use rsmpeg_codec::PictureType;
    use rsmpeg_util::{Rational, SampleFormat};

    fn solid_yuv420p(w: usize, h: usize, y: u8, u: u8, v: u8) -> Frame {
        let cw = w.div_ceil(2);
        let ch = h.div_ceil(2);
        Frame {
            data: vec![vec![y; w * h], vec![u; cw * ch], vec![v; cw * ch]],
            linesize: vec![w, cw, cw],
            width: w,
            height: h,
            pixel_format: PixelFormat::Yuv420P,
            sample_format: SampleFormat::None,
            sample_rate: 0,
            channels: 0,
            samples: 0,
            pts: Some(0),
            duration: 1,
            time_base: Rational::new(1, 30),
            key_frame: true,
            pict_type: PictureType::I,
        }
    }

    #[test]
    fn converts_to_rgba_len() {
        let frame = solid_yuv420p(16, 10, 16, 128, 128);
        let rgba = yuv420p_frame_to_rgba(&frame).unwrap();
        assert_eq!(rgba.len(), 16 * 10 * 4);
        // limited black
        assert!(rgba[0] <= 2);
        assert_eq!(rgba[3], 255);
    }

    #[test]
    fn rejects_non_yuv() {
        let mut frame = solid_yuv420p(4, 4, 16, 128, 128);
        frame.pixel_format = PixelFormat::Rgb24;
        assert!(yuv420p_frame_to_rgba(&frame).is_err());
    }

    #[test]
    fn cached_convert_to_rgba_len() {
        let frame = solid_yuv420p(16, 10, 16, 128, 128);
        let rgba = yuv420p_frame_to_rgba_cached(&frame).unwrap();
        assert_eq!(rgba.len(), 16 * 10 * 4);
    }

    #[test]
    fn pool_backed_convert_preserves_pixels() {
        let frame = solid_yuv420p(16, 10, 16, 128, 128);
        let converted = yuv420p_frame_to_rgba_cached(&frame).unwrap();
        // Mirror the native pipeline's pool-backed path: copy the verified
        // conversion into a recycled scratch buffer and confirm pixel content.
        let pool = FramePool::new(64 * 1024 * 1024);
        let needed_len = converted.len();
        let mut scratch = pool.get(needed_len);
        scratch.extend_from_slice(&converted);
        assert_eq!(scratch.len(), 16 * 10 * 4);
        // limited black
        assert!(scratch[0] <= 2);
        assert_eq!(scratch[3], 255);
        // Byte-identical to the non-pooled conversion.
        assert_eq!(scratch, converted);
        pool.recycle(scratch);
    }
}
