use crate::filter::Filter;
use crate::pad::Pad;
use rsmpeg_codec::Frame;

/// Rotate filter — rotates an RGBA `Frame` 90° clockwise in place.
///
/// This follows the crate's `Filter` trait exactly (name/description/inputs/outputs/
/// as_any), and additionally exposes `apply` for the actual pixel transformation.
pub struct RotateFilter;

impl RotateFilter {
    pub fn new() -> Self {
        Self
    }

    /// Replace `frame` with its 90° clockwise rotation.
    ///
    /// For an RGBA source of size `(W, H)` the destination is size `(H, W)`. A source
    /// pixel at `(x, y)` maps to destination `(dx, dy) = (H - 1 - y, x)`. Equivalently,
    /// destination row `dy` is source column `x = dy` and destination column
    /// `dx = H - 1 - y` corresponds to source row `y = H - 1 - dx`. We iterate the
    /// destination in row-major order and read the matching source pixel. Each pixel is
    /// a 4-byte stride (R, G, B, A); for RGBA, `frame.data[0]` is a single tightly-packed
    /// plane. If the source is empty, the frame is left unchanged.
    pub fn apply(&self, frame: &mut Frame) {
        let sw = frame.width;
        let sh = frame.height;
        if sw == 0 || sh == 0 {
            return;
        }
        let dw = sh;
        let dh = sw;
        let src = &frame.data[0];
        let stride = 4usize;
        let mut out = vec![0u8; dw * dh * stride];
        for dy in 0..dh {
            for dx in 0..dw {
                let sx = dy;
                let sy = sh - 1 - dx;
                let s_off = (sy * sw + sx) * stride;
                let d_off = (dy * dw + dx) * stride;
                out[d_off..(d_off + stride)].copy_from_slice(&src[s_off..(s_off + stride)]);
            }
        }
        frame.data = vec![out];
        frame.linesize = vec![dw * stride];
        frame.width = dw;
        frame.height = dh;
    }
}

impl Default for RotateFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl Filter for RotateFilter {
    fn name(&self) -> &'static str {
        "rotate"
    }
    fn description(&self) -> &'static str {
        "Rotate an RGBA frame 90° clockwise"
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
    fn rotate_90_cw_basic() {
        // 1x2 RGBA frame (W=1, H=2) with two distinct pixels, row-major (x,y):
        // row 0: (10,0,0,255)
        // row 1: (20,0,0,255)
        let mut frame = rgba_frame(1, 2, &[(10, 0, 0, 255), (20, 0, 0, 255)]);
        let filter = RotateFilter::new();
        filter.apply(&mut frame);
        // After 90° CW the frame becomes 2x1 (W=2, H=1).
        assert_eq!(frame.width, 2);
        assert_eq!(frame.height, 1);
        let p = &frame.data[0];
        assert_eq!(p.len(), 2 * 4);
        // The pixel that was at (0,1) lands at destination (0,0); the pixel that was at
        // (0,0) lands at destination (1,0).
        assert_eq!(p[0], 20);
        assert_eq!(p[1], 0);
        assert_eq!(p[2], 0);
        assert_eq!(p[3], 255);
        assert_eq!(p[4], 10);
        assert_eq!(p[5], 0);
        assert_eq!(p[6], 0);
        assert_eq!(p[7], 255);
    }

    #[test]
    fn rotate_90_cw_layout() {
        // 2x2 RGBA frame with 4 distinct pixels, row-major (x,y):
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
        let filter = RotateFilter::new();
        filter.apply(&mut frame);
        // After 90° CW the frame becomes 2x2 (W=2, H=2) as:
        // (3,0,0,255) (1,0,0,255)
        // (4,0,0,255) (2,0,0,255)
        assert_eq!(frame.width, 2);
        assert_eq!(frame.height, 2);
        let p = &frame.data[0];
        assert_eq!(p.len(), 2 * 2 * 4);
        assert_eq!(p[0], 3);
        assert_eq!(p[4], 1);
        assert_eq!(p[8], 4);
        assert_eq!(p[12], 2);
    }

    #[test]
    fn rotate_preserves_alpha() {
        // 2x2 RGBA frame with distinct alphas; alpha must survive the rotation.
        let pixels = [(1, 0, 0, 11), (2, 0, 0, 22), (3, 0, 0, 33), (4, 0, 0, 44)];
        let mut frame = rgba_frame(2, 2, &pixels);
        let filter = RotateFilter::new();
        filter.apply(&mut frame);
        let p = &frame.data[0];
        // Destination alpha layout mirrors the rotated pixel layout: 33,11,44,22.
        assert_eq!(p[3], 33);
        assert_eq!(p[7], 11);
        assert_eq!(p[11], 44);
        assert_eq!(p[15], 22);
    }

    #[test]
    fn rotate_registered() {
        let filter = RotateFilter::new();
        assert_eq!(filter.name(), "rotate");
        assert_eq!(filter.inputs().len(), 1);
        assert_eq!(filter.outputs().len(), 1);
        assert!(filter.as_any().is::<RotateFilter>());
    }
}
