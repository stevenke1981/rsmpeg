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

fn validate_packed_plane(frame: &Frame, row_bytes: usize) -> RsResult<(&[u8], usize)> {
    let h = frame.height;
    let plane =
        frame.data.first().map(|d| d.as_slice()).ok_or_else(|| {
            RsError::InvalidData("frame_to_rgba_cached: missing pixel plane".into())
        })?;
    let stride = frame
        .linesize
        .first()
        .copied()
        .filter(|&ls| ls >= row_bytes)
        .unwrap_or(row_bytes);
    let required = (h - 1)
        .checked_mul(stride)
        .and_then(|v| v.checked_add(row_bytes))
        .ok_or_else(|| {
            RsError::InvalidData("frame_to_rgba_cached: plane byte size overflow".into())
        })?;
    if plane.len() < required {
        return Err(RsError::InvalidData(
            format!(
                "frame_to_rgba_cached: plane too short (have {}, need {})",
                plane.len(),
                required
            )
            .into(),
        ));
    }
    Ok((plane, stride))
}

/// Convert any supported video [`Frame`] into a tightly packed RGBA buffer (`w * h * 4`).
///
/// Supports `Rgba`, `Rgb24`, `Gray8`, `Yuv420P` (using the cached per-resolution scaler),
/// `Yuv422P`, and `Nv12`.
pub fn frame_to_rgba_cached(frame: &Frame) -> RsResult<Vec<u8>> {
    if frame.width == 0 || frame.height == 0 {
        return Err(RsError::InvalidData(
            "frame_to_rgba_cached requires non-zero dimensions".into(),
        ));
    }
    let w = frame.width;
    let h = frame.height;
    let out_len = w
        .checked_mul(h)
        .and_then(|v| v.checked_mul(4))
        .ok_or_else(|| {
            RsError::InvalidData("frame_to_rgba_cached: output buffer size overflow".into())
        })?;

    match frame.pixel_format {
        PixelFormat::Rgba => {
            let row_bytes = w * 4;
            let (plane, stride) = validate_packed_plane(frame, row_bytes)?;
            if stride == row_bytes {
                return Ok(plane[..out_len].to_vec());
            }
            let mut out = Vec::with_capacity(out_len);
            for row in 0..h {
                let start = row * stride;
                out.extend_from_slice(&plane[start..start + row_bytes]);
            }
            Ok(out)
        }
        PixelFormat::Rgb24 => {
            let row_bytes = w.checked_mul(3).ok_or_else(|| {
                RsError::InvalidData("frame_to_rgba_cached: width overflow".into())
            })?;
            let (plane, stride) = validate_packed_plane(frame, row_bytes)?;
            let mut out = Vec::with_capacity(out_len);
            for row in 0..h {
                let start = row * stride;
                for px in plane[start..start + row_bytes].chunks_exact(3) {
                    out.extend_from_slice(&[px[0], px[1], px[2], 255]);
                }
            }
            Ok(out)
        }
        PixelFormat::Gray8 => {
            let (plane, stride) = validate_packed_plane(frame, w)?;
            let mut out = Vec::with_capacity(out_len);
            for row in 0..h {
                let start = row * stride;
                for &y in &plane[start..start + w] {
                    out.extend_from_slice(&[y, y, y, 255]);
                }
            }
            Ok(out)
        }
        PixelFormat::Yuv420P => crate::scaler_cache::yuv420p_to_rgba_cached(frame),
        PixelFormat::Yuv422P | PixelFormat::Nv12 => {
            let wu = u32::try_from(w).map_err(|_| {
                RsError::InvalidData("frame_to_rgba_cached: width exceeds u32".into())
            })?;
            let hu = u32::try_from(h).map_err(|_| {
                RsError::InvalidData("frame_to_rgba_cached: height exceeds u32".into())
            })?;
            let config = ScalerConfig::new(wu, hu, frame.pixel_format, wu, hu, PixelFormat::Rgba);
            let scaler = Scaler::new(config)?;
            let out = scaler.scale(frame)?;
            out.data
                .into_iter()
                .next()
                .ok_or_else(|| RsError::Bug("scaler produced empty RGBA plane list".into()))
        }
        other => Err(RsError::Unsupported(
            format!("frame_to_rgba_cached: unsupported pixel format {other:?}").into(),
        )),
    }
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

    fn packed_frame(w: usize, h: usize, fmt: PixelFormat, stride: usize, data: Vec<u8>) -> Frame {
        Frame {
            data: vec![data],
            linesize: vec![stride],
            width: w,
            height: h,
            pixel_format: fmt,
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
    fn frame_to_rgba_cached_supports_rgb24_rgba_gray8_and_yuv420p() {
        // Rgb24 with padded stride (width 2 -> 6 bytes, stride 8)
        let rgb_data = vec![
            10, 20, 30, 40, 50, 60, 99, 99, // row 0 + padding
            70, 80, 90, 100, 110, 120, 88, 88, // row 1 + padding
        ];
        let rgb_frame = packed_frame(2, 2, PixelFormat::Rgb24, 8, rgb_data);
        assert_eq!(
            frame_to_rgba_cached(&rgb_frame).unwrap(),
            vec![
                10, 20, 30, 255, 40, 50, 60, 255, //
                70, 80, 90, 255, 100, 110, 120, 255,
            ]
        );

        // Rgba with padded stride (width 2 -> 8 bytes, stride 12)
        let rgba_data = vec![
            1, 2, 3, 4, 5, 6, 7, 8, 99, 99, 99, 99, // row 0
            9, 10, 11, 12, 13, 14, 15, 16, 77, 77, 77, 77, // row 1
        ];
        let rgba_frame = packed_frame(2, 2, PixelFormat::Rgba, 12, rgba_data);
        assert_eq!(
            frame_to_rgba_cached(&rgba_frame).unwrap(),
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
        );

        // Gray8 with padded stride (width 2, stride 4)
        let gray_data = vec![12, 34, 99, 99, 56, 78, 88, 88];
        let gray_frame = packed_frame(2, 2, PixelFormat::Gray8, 4, gray_data);
        assert_eq!(
            frame_to_rgba_cached(&gray_frame).unwrap(),
            vec![
                12, 12, 12, 255, 34, 34, 34, 255, //
                56, 56, 56, 255, 78, 78, 78, 255,
            ]
        );

        // Yuv420P delegates cleanly to cached scaler
        let yuv = solid_yuv420p(16, 10, 16, 128, 128);
        assert_eq!(
            frame_to_rgba_cached(&yuv).unwrap(),
            yuv420p_frame_to_rgba_cached(&yuv).unwrap()
        );

        // Zero dimensions & unsupported formats rejected cleanly
        let zero = Frame::new_video(0, 4, PixelFormat::Rgb24);
        assert!(frame_to_rgba_cached(&zero).is_err());
        let unsupported = Frame::new_video(4, 4, PixelFormat::Gray16);
        assert!(matches!(
            frame_to_rgba_cached(&unsupported),
            Err(RsError::Unsupported(_))
        ));
    }
}
