use crate::filter::Filter;
use crate::pad::Pad;
use rsmpeg_codec::Frame;

/// Grayscale filter — converts an RGBA `Frame` to luminance (R=G=B), preserving alpha.
///
/// This follows the crate's `Filter` trait exactly (name/description/inputs/outputs/
/// as_any), and additionally exposes `apply` for the actual pixel transformation.
pub struct GrayscaleFilter;

impl GrayscaleFilter {
    pub fn new() -> Self {
        Self
    }

    /// Convert the packed RGBA plane of `frame` to grayscale.
    ///
    /// Each pixel is replaced with its luma `Y = round(0.299*R + 0.587*G + 0.114*B)`;
    /// the alpha channel is left untouched. For RGBA, `frame.data[0]` is a single
    /// tightly-packed plane with a 4-byte stride (R, G, B, A).
    pub fn apply(&self, frame: &mut Frame) {
        if frame.pixel_format != rsmpeg_util::PixelFormat::Rgba {
            return;
        }
        let w = frame.width;
        let h = frame.height;
        if w == 0 || h == 0 {
            return;
        }
        let Some(row_bytes) = w.checked_mul(4) else {
            return;
        };
        let src_stride = frame
            .linesize
            .first()
            .copied()
            .filter(|&ls| ls >= row_bytes)
            .unwrap_or(row_bytes);
        let Some(needed) = h
            .checked_sub(1)
            .and_then(|r| r.checked_mul(src_stride))
            .and_then(|o| o.checked_add(row_bytes))
        else {
            return;
        };
        // A frame with no plane or truncated plane is a no-op (previously panicked).
        let Some(buf) = frame.data.first_mut() else {
            return;
        };
        if buf.len() < needed {
            return;
        }
        for y in 0..h {
            let row_start = y * src_stride;
            for px in buf[row_start..row_start + row_bytes].chunks_exact_mut(4) {
                let r = px[0] as f32;
                let g = px[1] as f32;
                let b = px[2] as f32;
                let lum = (0.299 * r + 0.587 * g + 0.114 * b).round() as u8;
                px[0] = lum;
                px[1] = lum;
                px[2] = lum;
                // px[3] (alpha) is intentionally preserved.
            }
        }
    }
}

impl Default for GrayscaleFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl Filter for GrayscaleFilter {
    fn name(&self) -> &'static str {
        "grayscale"
    }
    fn description(&self) -> &'static str {
        "Convert RGBA video frames to luminance (grayscale), preserving alpha"
    }
    fn inputs(&self) -> Vec<Pad> {
        vec![Pad::input("default", rsmpeg_util::MediaType::Video)]
    }
    fn outputs(&self) -> Vec<Pad> {
        vec![Pad::output("default", rsmpeg_util::MediaType::Video)]
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsmpeg_util::PixelFormat;

    fn rgba_frame(pixels: &[(u8, u8, u8, u8)]) -> Frame {
        let n = pixels.len();
        let mut data = vec![0u8; n * 4];
        for (idx, &(r, g, b, a)) in pixels.iter().enumerate() {
            data[idx * 4] = r;
            data[idx * 4 + 1] = g;
            data[idx * 4 + 2] = b;
            data[idx * 4 + 3] = a;
        }
        let mut frame = Frame::new_video(n, 1, PixelFormat::Rgba);
        frame.data[0] = data;
        frame.linesize[0] = n * 4;
        frame
    }

    #[test]
    fn grayscale_makes_grey() {
        let mut frame = rgba_frame(&[(200, 100, 50, 255)]);
        let filter = GrayscaleFilter::new();
        filter.apply(&mut frame);
        let p = &frame.data[0];
        assert_eq!(p[0], p[1]);
        assert_eq!(p[1], p[2]);
        // round(0.299*200 + 0.587*100 + 0.114*50) = round(124.2) = 124
        assert_eq!(p[0], 124);
        assert_eq!(p[3], 255);
    }

    #[test]
    fn grayscale_preserves_alpha() {
        let mut frame = rgba_frame(&[(10, 20, 30, 255), (200, 200, 200, 128), (0, 0, 0, 0)]);
        let filter = GrayscaleFilter::new();
        filter.apply(&mut frame);
        let p = &frame.data[0];
        assert_eq!(p[3], 255);
        assert_eq!(p[7], 128);
        assert_eq!(p[11], 0);
        // Within each pixel, R == G == B.
        for px in 0..3 {
            let base = px * 4;
            assert_eq!(p[base], p[base + 1]);
            assert_eq!(p[base + 1], p[base + 2]);
        }
    }

    #[test]
    fn grayscale_registered_as_filter() {
        let filter = GrayscaleFilter::new();
        assert_eq!(filter.name(), "grayscale");
        assert_eq!(filter.inputs().len(), 1);
        assert_eq!(filter.outputs().len(), 1);
        assert!(filter.as_any().is::<GrayscaleFilter>());
    }

    #[test]
    fn grayscale_empty_plane_is_noop() {
        // A frame with no planes must not panic (previously indexed data[0]).
        let mut frame = Frame::new_video(2, 2, PixelFormat::Rgba);
        frame.data.clear();
        GrayscaleFilter::new().apply(&mut frame);
        assert!(frame.data.is_empty());
    }

    #[test]
    fn grayscale_endpoints_are_exact() {
        // Pure black stays black, pure white stays white (rounding edge cases).
        let mut frame = rgba_frame(&[(0, 0, 0, 255), (255, 255, 255, 255)]);
        GrayscaleFilter::new().apply(&mut frame);
        let p = &frame.data[0];
        assert_eq!(&p[0..4], &[0, 0, 0, 255]);
        assert_eq!(&p[4..8], &[255, 255, 255, 255]);
    }

    #[test]
    fn grayscale_non_rgba_is_noop() {
        let mut frame = Frame::new_video(2, 1, PixelFormat::Rgb24);
        frame.data[0] = vec![200, 100, 50, 10, 20, 30];
        let orig = frame.data[0].clone();
        GrayscaleFilter::new().apply(&mut frame);
        assert_eq!(frame.data[0], orig);
    }

    #[test]
    fn grayscale_honors_padded_linesize_and_1x1() {
        let mut frame = Frame::new_video(1, 2, PixelFormat::Rgba);
        let stride = 8usize; // 1 * 4 active + 4 sentinel padding bytes
        let mut buf = vec![0xEEu8; stride * 2];
        buf[0..4].copy_from_slice(&[200, 100, 50, 255]);
        buf[stride..stride + 4].copy_from_slice(&[0, 255, 0, 99]);
        frame.data[0] = buf;
        frame.linesize[0] = stride;

        GrayscaleFilter::new().apply(&mut frame);
        assert_eq!(frame.linesize, vec![stride]);
        assert_eq!(&frame.data[0][0..4], &[124, 124, 124, 255]);
        assert_eq!(&frame.data[0][4..8], &[0xEE; 4]);
        // round(0.587 * 255) = round(149.685) = 150
        assert_eq!(&frame.data[0][stride..stride + 4], &[150, 150, 150, 99]);
        assert_eq!(&frame.data[0][stride + 4..stride + 8], &[0xEE; 4]);
    }
}
