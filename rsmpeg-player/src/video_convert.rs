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

/// Cached variant of [`yuv420p_frame_to_rgba`] that reuses a cached
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

/// Write one RGBA pixel per `src_bpp`-byte source pixel of a validated packed
/// `plane` into `out`, a tightly packed `width * height * 4` buffer.
fn expand_to_rgba(
    plane: &[u8],
    stride: usize,
    width: usize,
    src_bpp: usize,
    out: &mut [u8],
    to_rgba: impl Fn(&[u8]) -> [u8; 4],
) {
    for (row, out_row) in out.chunks_exact_mut(width * 4).enumerate() {
        let src_row = &plane[row * stride..][..width * src_bpp];
        let pixels = out_row.chunks_exact_mut(4);
        for (px, source) in pixels.zip(src_row.chunks_exact(src_bpp)) {
            px.copy_from_slice(&to_rgba(source));
        }
    }
}

/// Convert any supported video [`Frame`] into a tightly packed RGBA buffer (`w * h * 4`).
///
/// Supports `Rgba`, `Bgra`, `Argb`, `Rgb24`, `Bgr24` and `Gray8`, plus `Yuv420P`,
/// `Yuv422P`, `Yuv444P`, `Nv12` and `Nv21` through the cached scaler.
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
        format @ (PixelFormat::Bgra
        | PixelFormat::Argb
        | PixelFormat::Rgb24
        | PixelFormat::Bgr24
        | PixelFormat::Gray8) => {
            let src_bpp = format.channels();
            let row_bytes = w.checked_mul(src_bpp).ok_or_else(|| {
                RsError::InvalidData("frame_to_rgba_cached: width overflow".into())
            })?;
            let (plane, stride) = validate_packed_plane(frame, row_bytes)?;
            let mut out = vec![0u8; out_len];
            // One closure per format, so each loop is specialised.
            match format {
                PixelFormat::Bgra => {
                    expand_to_rgba(plane, stride, w, 4, &mut out, |p| [p[2], p[1], p[0], p[3]])
                }
                PixelFormat::Argb => {
                    expand_to_rgba(plane, stride, w, 4, &mut out, |p| [p[1], p[2], p[3], p[0]])
                }
                PixelFormat::Rgb24 => {
                    expand_to_rgba(plane, stride, w, 3, &mut out, |p| [p[0], p[1], p[2], 255])
                }
                PixelFormat::Bgr24 => {
                    expand_to_rgba(plane, stride, w, 3, &mut out, |p| [p[2], p[1], p[0], 255])
                }
                _ => expand_to_rgba(plane, stride, w, 1, &mut out, |p| [p[0], p[0], p[0], 255]),
            }
            Ok(out)
        }
        PixelFormat::Yuv420P
        | PixelFormat::Yuv422P
        | PixelFormat::Yuv444P
        | PixelFormat::Nv12
        | PixelFormat::Nv21 => crate::scaler_cache::yuv_to_rgba_cached(frame),
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

    #[test]
    fn frame_to_rgba_cached_swaps_bgr24_channels_with_padded_stride() {
        // Width 2 -> 6 bytes per row, stride 8.
        let data = vec![
            30, 20, 10, 60, 50, 40, 99, 99, // row 0 + padding
            90, 80, 70, 120, 110, 100, 88, 88, // row 1 + padding
        ];
        let frame = packed_frame(2, 2, PixelFormat::Bgr24, 8, data);
        assert_eq!(
            frame_to_rgba_cached(&frame).unwrap(),
            vec![
                10, 20, 30, 255, 40, 50, 60, 255, //
                70, 80, 90, 255, 100, 110, 120, 255,
            ]
        );
    }

    #[test]
    fn frame_to_rgba_cached_swaps_bgra_channels_and_keeps_alpha() {
        // Width 2 -> 8 bytes per row, stride 12; the last row is unpadded.
        let data = vec![
            3, 2, 1, 4, 7, 6, 5, 8, 99, 99, 99, 99, // row 0 + padding
            11, 10, 9, 0, 15, 14, 13, 128, // row 1
        ];
        let frame = packed_frame(2, 2, PixelFormat::Bgra, 12, data);
        assert_eq!(
            frame_to_rgba_cached(&frame).unwrap(),
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 0, 13, 14, 15, 128]
        );
    }

    #[test]
    fn frame_to_rgba_cached_reorders_argb_with_padded_stride() {
        // Width 2 -> 8 bytes per row, stride 12; the last row is unpadded.
        let data = vec![
            4, 1, 2, 3, 8, 5, 6, 7, 99, 99, 99, 99, // row 0 + padding
            0, 9, 10, 11, 128, 13, 14, 15, // row 1
        ];
        let frame = packed_frame(2, 2, PixelFormat::Argb, 12, data);
        assert_eq!(
            frame_to_rgba_cached(&frame).unwrap(),
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 0, 13, 14, 15, 128]
        );
    }

    #[test]
    fn frame_to_rgba_cached_rejects_truncated_bgr_planes() {
        for (format, full) in [
            (PixelFormat::Bgr24, 12),
            (PixelFormat::Bgra, 16),
            (PixelFormat::Argb, 16),
            (PixelFormat::Rgb24, 12),
            (PixelFormat::Gray8, 4),
        ] {
            let frame = packed_frame(2, 2, format, full / 2, vec![0; full - 1]);
            assert!(frame_to_rgba_cached(&frame).is_err(), "{format:?}");
        }
    }

    /// Lay `rows` out at `stride`, filling the padding with `0xEE`.
    fn padded(rows: &[&[u8]], stride: usize) -> Vec<u8> {
        rows.iter()
            .flat_map(|row| {
                let mut line = row.to_vec();
                line.resize(stride, 0xEE);
                line
            })
            .collect()
    }

    /// RGBA of one BT.601 limited-range pixel, via a tight 1×1 4:2:0 frame.
    fn pixel_rgba(y: u8, u: u8, v: u8) -> Vec<u8> {
        yuv420p_frame_to_rgba(&solid_yuv420p(1, 1, y, u, v)).unwrap()
    }

    const LUMA: [[u8; 4]; 2] = [[40, 70, 100, 130], [160, 190, 210, 230]];

    /// Expected RGBA for the 4×2 `LUMA` image given the chroma of each pixel.
    fn expected_rgba(chroma: impl Fn(usize, usize) -> (u8, u8)) -> Vec<u8> {
        let mut out = Vec::new();
        for (y, row) in LUMA.iter().enumerate() {
            for (x, &luma) in row.iter().enumerate() {
                let (u, v) = chroma(x, y);
                out.extend(pixel_rgba(luma, u, v));
            }
        }
        out
    }

    fn yuv_frame(format: PixelFormat, data: Vec<Vec<u8>>, linesize: Vec<usize>) -> Frame {
        let mut frame = packed_frame(4, 2, format, 0, Vec::new());
        frame.data = data;
        frame.linesize = linesize;
        frame
    }

    #[test]
    fn frame_to_rgba_cached_converts_padded_yuv422p() {
        let frame = yuv_frame(
            PixelFormat::Yuv422P,
            vec![
                padded(&[&LUMA[0], &LUMA[1]], 7),
                padded(&[&[60, 200], &[90, 160]], 3),
                padded(&[&[210, 70], &[150, 110]], 5),
            ],
            vec![7, 3, 5],
        );
        let u = [[60, 200], [90, 160]];
        let v = [[210, 70], [150, 110]];
        assert_eq!(
            frame_to_rgba_cached(&frame).unwrap(),
            expected_rgba(|x, y| (u[y][x / 2], v[y][x / 2]))
        );
    }

    #[test]
    fn frame_to_rgba_cached_converts_padded_yuv444p() {
        let u: [[u8; 4]; 2] = [[60, 90, 120, 150], [180, 200, 80, 100]];
        let v: [[u8; 4]; 2] = [[200, 170, 140, 110], [90, 70, 220, 130]];
        let frame = yuv_frame(
            PixelFormat::Yuv444P,
            vec![
                padded(&[&LUMA[0], &LUMA[1]], 6),
                padded(&[&u[0], &u[1]], 5),
                padded(&[&v[0], &v[1]], 9),
            ],
            vec![6, 5, 9],
        );
        assert_eq!(
            frame_to_rgba_cached(&frame).unwrap(),
            expected_rgba(|x, y| (u[y][x], v[y][x]))
        );
    }

    #[test]
    fn frame_to_rgba_cached_converts_padded_nv21() {
        // Same picture as the NV12 case, with each chroma pair stored V first.
        let frame = yuv_frame(
            PixelFormat::Nv21,
            vec![
                padded(&[&LUMA[0], &LUMA[1]], 6),
                padded(&[&[210, 60, 70, 200]], 8),
            ],
            vec![6, 8],
        );
        assert_eq!(
            frame_to_rgba_cached(&frame).unwrap(),
            expected_rgba(|x, _| if x < 2 { (60, 210) } else { (200, 70) })
        );
    }

    #[test]
    fn frame_to_rgba_cached_converts_padded_nv12() {
        let frame = yuv_frame(
            PixelFormat::Nv12,
            vec![
                padded(&[&LUMA[0], &LUMA[1]], 6),
                padded(&[&[60, 210, 200, 70]], 8),
            ],
            vec![6, 8],
        );
        assert_eq!(
            frame_to_rgba_cached(&frame).unwrap(),
            expected_rgba(|x, _| if x < 2 { (60, 210) } else { (200, 70) })
        );
    }
}
