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
        let w = frame.width as usize;
        let h = frame.height as usize;
        if w == 0 || h == 0 {
            return;
        }
        let buf = &mut frame.data[0];
        let stride = 4usize;
        for y in 0..h {
            let row = y * w * stride;
            for x in 0..w / 2 {
                let l = row + x * stride;
                let r = row + (w - 1 - x) * stride;
                for c in 0..stride {
                    buf.swap(l + c, r + c);
                }
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
        frame.linesize[0] = n * 4;
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
}
