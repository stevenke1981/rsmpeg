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
        let buf = &mut frame.data[0];
        let mut i = 0usize;
        while i + 3 < buf.len() {
            let r = buf[i] as f32;
            let g = buf[i + 1] as f32;
            let b = buf[i + 2] as f32;
            let y = (0.299 * r + 0.587 * g + 0.114 * b).round() as u8;
            buf[i] = y;
            buf[i + 1] = y;
            buf[i + 2] = y;
            // buf[i + 3] (alpha) is intentionally preserved.
            i += 4;
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
}
