use crate::filter::Filter;
use crate::pad::Pad;
use rsmpeg_codec::Frame;

/// Mirror filter — horizontally flips an RGBA `Frame` (reverses each row).
///
/// This follows the crate's `Filter` trait exactly (name/description/inputs/outputs/
/// as_any), and additionally exposes `apply` for the actual pixel transformation.
pub struct MirrorFilter;

impl MirrorFilter {
    pub fn new() -> Self {
        Self
    }

    /// Horizontally flip the packed RGBA plane of `frame`.
    ///
    /// For each row, pixel at column `x` is swapped with pixel at column `w - 1 - x`.
    /// Each pixel is a 4-byte stride (R, G, B, A); the alpha channel is part of the
    /// swap but is preserved in value because the whole pixel is exchanged. For RGBA,
    /// `frame.data[0]` is a single tightly-packed plane with a 4-byte stride.
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
        // Guard against a frame with no plane / a truncated plane so a malformed
        // input cannot panic the playback thread.
        let Some(buf) = frame.data.first_mut() else {
            return;
        };
        if buf.len() < needed {
            return;
        }
        for y in 0..h {
            let row_start = y * src_stride;
            let row = &mut buf[row_start..row_start + row_bytes];
            for x in 0..w / 2 {
                let l_start = x * 4;
                let r_start = (w - 1 - x) * 4;
                let (left_part, right_part) = row.split_at_mut(r_start);
                left_part[l_start..l_start + 4].swap_with_slice(&mut right_part[..4]);
            }
        }
    }
}

impl Default for MirrorFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl Filter for MirrorFilter {
    fn name(&self) -> &'static str {
        "mirror"
    }
    fn description(&self) -> &'static str {
        "Horizontally flip RGBA video frames, preserving alpha"
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
    fn mirror_flips_horizontally() {
        // 2x1 RGBA frame: pixels [ (1,0,0,255), (2,0,0,255) ]
        let mut frame = rgba_frame(2, 1, &[(1, 0, 0, 255), (2, 0, 0, 255)]);
        let filter = MirrorFilter::new();
        filter.apply(&mut frame);
        let p = &frame.data[0];
        // First pixel should now be (2,0,0,255)
        assert_eq!(p[0], 2);
        assert_eq!(p[1], 0);
        assert_eq!(p[2], 0);
        assert_eq!(p[3], 255);
        // Second pixel should now be (1,0,0,255)
        assert_eq!(p[4], 1);
        assert_eq!(p[5], 0);
        assert_eq!(p[6], 0);
        assert_eq!(p[7], 255);
    }

    #[test]
    fn mirror_preserves_alpha() {
        // 4x1 RGBA frame with distinct alphas.
        let pixels = [(1, 0, 0, 10), (2, 0, 0, 20), (3, 0, 0, 30), (4, 0, 0, 40)];
        let mut frame = rgba_frame(4, 1, &pixels);
        let filter = MirrorFilter::new();
        filter.apply(&mut frame);
        let p = &frame.data[0];
        // After horizontal flip the alphas must be 40,30,20,10 in order.
        assert_eq!(p[3], 40);
        assert_eq!(p[7], 30);
        assert_eq!(p[11], 20);
        assert_eq!(p[15], 10);
        // All RGB are still 0..4 but reversed; verify alpha positions untouched.
        for px in 0..4 {
            assert_eq!(p[px * 4 + 3], [40u8, 30, 20, 10][px]);
        }
    }

    #[test]
    fn mirror_registered_as_filter() {
        let filter = MirrorFilter::new();
        assert_eq!(filter.name(), "mirror");
        assert_eq!(filter.inputs().len(), 1);
        assert_eq!(filter.outputs().len(), 1);
        assert!(filter.as_any().is::<MirrorFilter>());
    }

    #[test]
    fn mirror_is_its_own_inverse() {
        // Applying the flip twice must restore the original buffer.
        let pixels = [
            (1, 2, 3, 4),
            (5, 6, 7, 8),
            (9, 10, 11, 12),
            (13, 14, 15, 16),
        ];
        let mut frame = rgba_frame(2, 2, &pixels);
        let original = frame.data[0].clone();
        let filter = MirrorFilter::new();
        filter.apply(&mut frame);
        assert_ne!(frame.data[0], original, "one flip must change the pixels");
        filter.apply(&mut frame);
        assert_eq!(frame.data[0], original, "two flips must be the identity");
    }

    #[test]
    fn mirror_odd_width_keeps_centre_pixel() {
        // With an odd width the middle column maps onto itself and must not move.
        let mut frame = rgba_frame(3, 1, &[(10, 0, 0, 255), (20, 0, 0, 255), (30, 0, 0, 255)]);
        MirrorFilter::new().apply(&mut frame);
        let p = &frame.data[0];
        assert_eq!(p[0], 30);
        assert_eq!(p[4], 20);
        assert_eq!(p[8], 10);
    }

    #[test]
    fn mirror_missing_plane_is_noop() {
        let mut frame = Frame::new_video(2, 2, PixelFormat::Rgba);
        frame.data.clear();
        MirrorFilter::new().apply(&mut frame);
        assert!(frame.data.is_empty());
    }

    #[test]
    fn mirror_truncated_plane_is_noop() {
        let mut frame = Frame::new_video(2, 2, PixelFormat::Rgba);
        frame.data[0] = vec![5u8; 4];
        MirrorFilter::new().apply(&mut frame);
        assert_eq!(frame.data[0], vec![5u8; 4]);
    }

    #[test]
    fn mirror_non_rgba_is_noop() {
        let mut frame = Frame::new_video(2, 1, PixelFormat::Bgra);
        frame.data[0] = vec![1, 2, 3, 255, 4, 5, 6, 255];
        let orig = frame.data[0].clone();
        MirrorFilter::new().apply(&mut frame);
        assert_eq!(frame.data[0], orig);
    }

    #[test]
    fn mirror_honors_padded_linesize_and_1x1() {
        let mut single = rgba_frame(1, 1, &[(7, 8, 9, 10)]);
        MirrorFilter::new().apply(&mut single);
        assert_eq!(single.data[0], vec![7, 8, 9, 10]);

        let mut frame = Frame::new_video(2, 2, PixelFormat::Rgba);
        let stride = 12usize; // 2 * 4 active + 4 padding
        let mut buf = vec![0xEEu8; stride * 2];
        buf[0..8].copy_from_slice(&[1, 0, 0, 255, 2, 0, 0, 255]);
        buf[stride..stride + 8].copy_from_slice(&[3, 0, 0, 128, 4, 0, 0, 64]);
        frame.data[0] = buf;
        frame.linesize[0] = stride;

        MirrorFilter::new().apply(&mut frame);
        assert_eq!(frame.linesize, vec![stride]);
        assert_eq!(&frame.data[0][0..8], &[2, 0, 0, 255, 1, 0, 0, 255]);
        assert_eq!(&frame.data[0][8..12], &[0xEE; 4]);
        assert_eq!(
            &frame.data[0][stride..stride + 8],
            &[4, 0, 0, 64, 3, 0, 0, 128]
        );
        assert_eq!(&frame.data[0][stride + 8..stride + 12], &[0xEE; 4]);
    }
}
