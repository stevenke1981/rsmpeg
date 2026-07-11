use crate::filter::Filter;
use crate::pad::Pad;
use rsmpeg_codec::Frame;

/// Crop filter — extracts a sub-rectangle from an RGBA `Frame` into a new, smaller RGBA `Frame`.
///
/// This follows the crate's `Filter` trait exactly (name/description/inputs/outputs/
/// as_any), and additionally exposes `apply` for the actual pixel transformation.
pub struct CropFilter {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
}

impl CropFilter {
    pub fn new(x: usize, y: usize, w: usize, h: usize) -> Self {
        Self { x, y, w, h }
    }

    /// Replace `frame` with the cropped sub-rectangle (clamped to source bounds).
    ///
    /// The crop rectangle `(x, y, w, h)` is clamped so it never exceeds the source
    /// frame: `x` is capped at `width`, `y` at `height`, and `w`/`h` are reduced so
    /// the region stays inside the frame. Each pixel is a 4-byte stride (R, G, B, A);
    /// for RGBA, `frame.data[0]` is a single tightly-packed plane. If the clamped
    /// region has zero area (or the source is empty), the frame is left unchanged.
    pub fn apply(&self, frame: &mut Frame) {
        let sw = frame.width;
        let sh = frame.height;
        if sw == 0 || sh == 0 {
            return;
        }
        let x = self.x.min(sw);
        let y = self.y.min(sh);
        let w = self.w.min(sw - x);
        let h = self.h.min(sh - y);
        if w == 0 || h == 0 {
            return;
        }
        let src = &frame.data[0];
        let stride = 4usize;
        let mut out = vec![0u8; w * h * stride];
        for row in 0..h {
            let s_row = (y + row) * sw * stride + x * stride;
            let d_row = row * w * stride;
            out[d_row..(w * stride + d_row)].copy_from_slice(&src[s_row..(w * stride + s_row)]);
        }
        frame.data = vec![out];
        frame.linesize = vec![w * stride];
        frame.width = w;
        frame.height = h;
    }
}

impl Default for CropFilter {
    fn default() -> Self {
        Self::new(0, 0, 0, 0)
    }
}

impl Filter for CropFilter {
    fn name(&self) -> &'static str {
        "crop"
    }
    fn description(&self) -> &'static str {
        "Crop an RGBA frame to a sub-rectangle"
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
    fn crop_extracts_subrect() {
        // 2x2 RGBA frame with distinct pixels, row-major (x,y):
        // (1,0,0,255) (2,0,0,255)
        // (3,0,0,255) (4,0,0,255)
        let mut frame = rgba_frame(
            2,
            2,
            &[
                (1, 0, 0, 255),
                (2, 0, 0, 255),
                (3, 0, 0, 255),
                (4, 0, 0, 255),
            ],
        );
        let filter = CropFilter::new(1, 0, 1, 1);
        filter.apply(&mut frame);
        // Result is 1x1 containing the pixel that was at (1, 0): (2,0,0,255).
        assert_eq!(frame.width, 1);
        assert_eq!(frame.height, 1);
        let p = &frame.data[0];
        assert_eq!(p.len(), 4);
        assert_eq!(p[0], 2);
        assert_eq!(p[1], 0);
        assert_eq!(p[2], 0);
        assert_eq!(p[3], 255);
    }

    #[test]
    fn crop_clamps_out_of_bounds() {
        // Requesting a region far larger than the 2x2 frame must clamp, not panic.
        let pixels = [
            (1, 0, 0, 255),
            (2, 0, 0, 255),
            (3, 0, 0, 255),
            (4, 0, 0, 255),
        ];
        let mut frame = rgba_frame(2, 2, &pixels);
        let filter = CropFilter::new(0, 0, 100, 100);
        filter.apply(&mut frame);
        // Clamped to the full 2x2 frame.
        assert_eq!(frame.width, 2);
        assert_eq!(frame.height, 2);
        let p = &frame.data[0];
        assert_eq!(p.len(), 2 * 2 * 4);
        assert_eq!(p[0], 1);
        assert_eq!(p[4], 2);
        assert_eq!(p[8], 3);
        assert_eq!(p[12], 4);
    }

    #[test]
    fn crop_registered_as_filter() {
        let filter = CropFilter::new(0, 0, 1, 1);
        assert_eq!(filter.name(), "crop");
        assert_eq!(filter.inputs().len(), 1);
        assert_eq!(filter.outputs().len(), 1);
        assert!(filter.as_any().is::<CropFilter>());
    }
}
