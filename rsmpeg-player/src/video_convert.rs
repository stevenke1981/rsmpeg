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

#[cfg(test)]
mod tests {
    use super::*;
    use rsmpeg_codec::PictureType;
    use rsmpeg_util::{Rational, SampleFormat};

    fn solid_yuv420p(w: usize, h: usize, y: u8, u: u8, v: u8) -> Frame {
        let cw = (w + 1) / 2;
        let ch = (h + 1) / 2;
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
}
