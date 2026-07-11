use crate::filter::Filter;
use crate::pad::Pad;
use rsmpeg_codec::Frame;

/// Box blur filter — applies a simple 3×3 box blur to an RGBA `Frame`.
///
/// This follows the crate's `Filter` trait exactly (name/description/inputs/outputs/
/// as_any), and additionally exposes `apply` for the actual pixel transformation.
///
/// For every output pixel the R/G/B channels are the average of the 3×3 neighborhood
/// (coordinates are clamped to `[0, W)` and `[0, H)` at the edges, so the window
/// count is 9 in the interior and fewer at the borders). The alpha channel is copied
/// verbatim from the center pixel (it is not blurred).
pub struct BoxBlurFilter;

impl BoxBlurFilter {
    pub fn new() -> Self {
        Self
    }

    /// Replace `frame` with its 3×3 box-blurred version.
    ///
    /// Each pixel is a 4-byte stride (R, G, B, A); for RGBA, `frame.data[0]` is a
    /// single tightly-packed plane. The output frame keeps the same dimensions and
    /// stride. If the source is empty, the frame is left unchanged.
    pub fn apply(&self, frame: &mut Frame) {
        let w = frame.width;
        let h = frame.height;
        if w == 0 || h == 0 {
            return;
        }
        let src = &frame.data[0];
        let stride = 4usize;
        let mut out = vec![0u8; w * h * stride];
        for y in 0..h {
            for x in 0..w {
                let mut sum_r = 0u32;
                let mut sum_g = 0u32;
                let mut sum_b = 0u32;
                let mut count = 0u32;
                for dy in -1i32..=1 {
                    for dx in -1i32..=1 {
                        let nx = x as i32 + dx;
                        let ny = y as i32 + dy;
                        if nx < 0 || nx >= w as i32 || ny < 0 || ny >= h as i32 {
                            continue;
                        }
                        let off = ((ny as usize) * w + (nx as usize)) * stride;
                        sum_r += src[off] as u32;
                        sum_g += src[off + 1] as u32;
                        sum_b += src[off + 2] as u32;
                        count += 1;
                    }
                }
                let off = (y * w + x) * stride;
                out[off] = (sum_r / count) as u8;
                out[off + 1] = (sum_g / count) as u8;
                out[off + 2] = (sum_b / count) as u8;
                out[off + 3] = src[off + 3];
            }
        }
        frame.data = vec![out];
        frame.linesize = vec![w * stride];
    }
}

impl Default for BoxBlurFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl Filter for BoxBlurFilter {
    fn name(&self) -> &'static str {
        "blur"
    }
    fn description(&self) -> &'static str {
        "Apply a 3×3 box blur to RGBA video frames, preserving alpha"
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

    fn rgba_frame(width: usize, height: usize, pixels: &[(u8, u8, u8, u8)]) -> Frame {
        let n = pixels.len();
        let mut data = vec![0u8; n * 4];
        for (idx, &(r, g, b, a)) in pixels.iter().enumerate() {
            data[idx * 4] = r;
            data[idx * 4 + 1] = g;
            data[idx * 4 + 2] = b;
            data[idx * 4 + 3] = a;
        }
        let mut frame = Frame::new_video(width, height, PixelFormat::Rgba);
        frame.data[0] = data;
        frame.linesize[0] = n * 4;
        frame
    }

    #[test]
    fn blur_uniform_stays_same() {
        // A 3x3 frame filled with a single color must be unchanged by the blur.
        let pixels = [(100u8, 50, 25, 255); 9];
        let mut frame = rgba_frame(3, 3, &pixels);
        let filter = BoxBlurFilter::new();
        filter.apply(&mut frame);
        assert_eq!(frame.width, 3);
        assert_eq!(frame.height, 3);
        let p = &frame.data[0];
        assert_eq!(p.len(), 9 * 4);
        for px in 0..9 {
            let base = px * 4;
            assert_eq!(p[base], 100);
            assert_eq!(p[base + 1], 50);
            assert_eq!(p[base + 2], 25);
            assert_eq!(p[base + 3], 255);
        }
    }

    #[test]
    fn blur_spreads_bright_pixel() {
        // A 5x5 black frame with a single white pixel in the center: after a 3x3 box
        // blur that pixel and its 8 neighbors must be grey (>0 and <255).
        let w = 5usize;
        let h = 5usize;
        let mut pixels = [(0u8, 0, 0, 255); 25];
        let center = (h / 2) * w + (w / 2);
        pixels[center] = (255, 255, 255, 255);
        let mut frame = rgba_frame(w, h, &pixels);
        let filter = BoxBlurFilter::new();
        filter.apply(&mut frame);
        let p = &frame.data[0];
        let cx = w / 2;
        let cy = h / 2;
        for dy in -1i32..=1 {
            for dx in -1i32..=1 {
                let x = (cx as i32 + dx) as usize;
                let y = (cy as i32 + dy) as usize;
                let off = (y * w + x) * 4;
                let r = p[off];
                let g = p[off + 1];
                let b = p[off + 2];
                assert!(r > 0 && r < 255, "R={r} should be grey");
                assert!(g > 0 && g < 255, "G={g} should be grey");
                assert!(b > 0 && b < 255, "B={b} should be grey");
                // Alpha is copied from the (white) center pixel, so it stays 255.
                assert_eq!(p[off + 3], 255);
            }
        }
    }

    #[test]
    fn blur_registered_as_filter() {
        let filter = BoxBlurFilter::new();
        assert_eq!(filter.name(), "blur");
        assert_eq!(filter.inputs().len(), 1);
        assert_eq!(filter.outputs().len(), 1);
        assert!(filter.as_any().is::<BoxBlurFilter>());
    }
}
