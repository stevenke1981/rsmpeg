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
        if frame.pixel_format != rsmpeg_util::PixelFormat::Rgba {
            return;
        }
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
        let Some(src_row_bytes) = sw.checked_mul(4) else {
            return;
        };
        let src_stride = frame
            .linesize
            .first()
            .copied()
            .filter(|&ls| ls >= src_row_bytes)
            .unwrap_or(src_row_bytes);
        let Some(needed) = sh
            .checked_sub(1)
            .and_then(|r| r.checked_mul(src_stride))
            .and_then(|o| o.checked_add(src_row_bytes))
        else {
            return;
        };
        // Refuse to read outside the packed RGBA plane: a missing or truncated
        // plane leaves the frame unchanged instead of panicking.
        let Some(src) = frame.data.first() else {
            return;
        };
        if src.len() < needed {
            return;
        }
        let out_row_bytes = w * 4;
        let mut out = vec![0u8; out_row_bytes * h];
        let x_off = x * 4;
        for row in 0..h {
            let s_row = (y + row) * src_stride + x_off;
            let d_row = row * out_row_bytes;
            out[d_row..d_row + out_row_bytes].copy_from_slice(&src[s_row..s_row + out_row_bytes]);
        }
        frame.data = vec![out];
        frame.linesize = vec![out_row_bytes];
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
        frame.linesize[0] = width * 4;
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

    #[test]
    fn crop_zero_area_leaves_frame_unchanged() {
        let pixels = [
            (1, 0, 0, 255),
            (2, 0, 0, 255),
            (3, 0, 0, 255),
            (4, 0, 0, 255),
        ];
        let mut frame = rgba_frame(2, 2, &pixels);
        // w = 0 → no-op.
        CropFilter::new(0, 0, 0, 2).apply(&mut frame);
        assert_eq!((frame.width, frame.height), (2, 2));
        // h = 0 → no-op.
        CropFilter::new(0, 0, 2, 0).apply(&mut frame);
        assert_eq!((frame.width, frame.height), (2, 2));
        // Origin past the frame → clamped region is empty → no-op.
        CropFilter::new(9, 9, 1, 1).apply(&mut frame);
        assert_eq!((frame.width, frame.height), (2, 2));
        assert_eq!(frame.data[0].len(), 2 * 2 * 4);
    }

    #[test]
    fn crop_missing_plane_is_noop() {
        let mut frame = Frame::new_video(2, 2, PixelFormat::Rgba);
        frame.data.clear();
        CropFilter::new(0, 0, 1, 1).apply(&mut frame);
        assert!(frame.data.is_empty());
    }

    #[test]
    fn crop_truncated_plane_is_noop() {
        // Declared 2x2 but only one pixel present: must not read out of bounds.
        let mut frame = Frame::new_video(2, 2, PixelFormat::Rgba);
        frame.data[0] = vec![7u8; 4];
        CropFilter::new(0, 0, 2, 2).apply(&mut frame);
        assert_eq!(frame.data[0], vec![7u8; 4]);
        assert_eq!((frame.width, frame.height), (2, 2));
    }

    #[test]
    fn crop_updates_linesize_and_keeps_metadata() {
        let mut frame = rgba_frame(3, 1, &[(1, 0, 0, 255), (2, 0, 0, 255), (3, 0, 0, 255)]);
        frame.pts = Some(42);
        CropFilter::new(1, 0, 2, 1).apply(&mut frame);
        assert_eq!(frame.width, 2);
        assert_eq!(frame.linesize[0], 2 * 4);
        assert_eq!(frame.data[0].len(), 2 * 4);
        // Metadata is untouched by the geometry change.
        assert_eq!(frame.pts, Some(42));
        assert_eq!(frame.pixel_format, PixelFormat::Rgba);
    }

    #[test]
    fn crop_non_rgba_is_noop() {
        let mut frame = Frame::new_video(2, 2, PixelFormat::Yuv420P);
        let orig = frame.data.clone();
        CropFilter::new(0, 0, 1, 1).apply(&mut frame);
        assert_eq!((frame.width, frame.height), (2, 2));
        assert_eq!(frame.data, orig);
    }

    #[test]
    fn crop_honors_padded_linesize_and_1x1() {
        let mut frame = Frame::new_video(2, 2, PixelFormat::Rgba);
        let stride = 12usize; // 2 * 4 = 8 active + 4 padding
        let mut buf = vec![0xEEu8; stride * 2];
        buf[0..8].copy_from_slice(&[1, 2, 3, 255, 4, 5, 6, 255]);
        buf[stride..stride + 8].copy_from_slice(&[7, 8, 9, 255, 10, 11, 12, 128]);
        frame.data[0] = buf;
        frame.linesize[0] = stride;

        CropFilter::new(1, 1, 1, 1).apply(&mut frame);
        assert_eq!((frame.width, frame.height), (1, 1));
        assert_eq!(frame.linesize, vec![4]);
        assert_eq!(frame.data[0], vec![10, 11, 12, 128]);
    }
}
