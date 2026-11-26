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
        // A frame without a packed RGBA plane (or with a plane shorter than the
        // declared geometry) cannot be blurred; leave it untouched instead of
        // panicking on the index.
        let Some(src) = frame.data.first() else {
            return;
        };
        if src.len() < needed {
            return;
        }
        let mut out = vec![0u8; row_bytes * h];
        for (y, out_row) in out.chunks_exact_mut(row_bytes).enumerate() {
            if y == 0 || y + 1 == h {
                for (x, dst_px) in out_row.chunks_exact_mut(4).enumerate() {
                    blur_clamped_pixel(src, src_stride, w, h, x, y, dst_px);
                }
                continue;
            }
            let r0 = (y - 1) * src_stride;
            let r1 = y * src_stride;
            let r2 = (y + 1) * src_stride;

            // Left border column (x = 0)
            blur_clamped_pixel(src, src_stride, w, h, 0, y, &mut out_row[0..4]);

            // Fast interior path: all 9 neighbors are in bounds (count == 9).
            if w >= 3 {
                for (dx, dst_px) in out_row[4..row_bytes - 4].chunks_exact_mut(4).enumerate() {
                    let x = dx + 1;
                    let xl = (x - 1) * 4;
                    let xm = x * 4;
                    let xr = (x + 1) * 4;
                    let sum_r = src[r0 + xl] as u32
                        + src[r0 + xm] as u32
                        + src[r0 + xr] as u32
                        + src[r1 + xl] as u32
                        + src[r1 + xm] as u32
                        + src[r1 + xr] as u32
                        + src[r2 + xl] as u32
                        + src[r2 + xm] as u32
                        + src[r2 + xr] as u32;
                    let sum_g = src[r0 + xl + 1] as u32
                        + src[r0 + xm + 1] as u32
                        + src[r0 + xr + 1] as u32
                        + src[r1 + xl + 1] as u32
                        + src[r1 + xm + 1] as u32
                        + src[r1 + xr + 1] as u32
                        + src[r2 + xl + 1] as u32
                        + src[r2 + xm + 1] as u32
                        + src[r2 + xr + 1] as u32;
                    let sum_b = src[r0 + xl + 2] as u32
                        + src[r0 + xm + 2] as u32
                        + src[r0 + xr + 2] as u32
                        + src[r1 + xl + 2] as u32
                        + src[r1 + xm + 2] as u32
                        + src[r1 + xr + 2] as u32
                        + src[r2 + xl + 2] as u32
                        + src[r2 + xm + 2] as u32
                        + src[r2 + xr + 2] as u32;
                    dst_px.copy_from_slice(&[
                        (sum_r / 9) as u8,
                        (sum_g / 9) as u8,
                        (sum_b / 9) as u8,
                        src[r1 + xm + 3],
                    ]);
                }
            }

            // Right border column (x = w - 1) when w >= 2
            if w >= 2 {
                blur_clamped_pixel(
                    src,
                    src_stride,
                    w,
                    h,
                    w - 1,
                    y,
                    &mut out_row[row_bytes - 4..],
                );
            }
        }
        frame.data = vec![out];
        frame.linesize = vec![row_bytes];
    }
}

#[inline]
fn blur_clamped_pixel(
    src: &[u8],
    src_stride: usize,
    w: usize,
    h: usize,
    x: usize,
    y: usize,
    dst_px: &mut [u8],
) {
    let x_start = x.saturating_sub(1);
    let x_end = (x + 1).min(w - 1);
    let y_start = y.saturating_sub(1);
    let y_end = (y + 1).min(h - 1);
    let mut sum_r = 0u32;
    let mut sum_g = 0u32;
    let mut sum_b = 0u32;
    let mut count = 0u32;
    for ny in y_start..=y_end {
        let row_off = ny * src_stride;
        for nx in x_start..=x_end {
            let off = row_off + nx * 4;
            sum_r += src[off] as u32;
            sum_g += src[off + 1] as u32;
            sum_b += src[off + 2] as u32;
            count += 1;
        }
    }
    let center = y * src_stride + x * 4;
    dst_px.copy_from_slice(&[
        (sum_r / count) as u8,
        (sum_g / count) as u8,
        (sum_b / count) as u8,
        src[center + 3],
    ]);
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
        frame.linesize[0] = width * 4;
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

    #[test]
    fn blur_empty_frame_is_noop() {
        // Zero-size and plane-less frames must be left untouched, not panic.
        let mut zero = Frame::new_video(0, 0, PixelFormat::Rgba);
        BoxBlurFilter::new().apply(&mut zero);
        assert_eq!(zero.width, 0);

        let mut no_plane = Frame::new_video(2, 2, PixelFormat::Rgba);
        no_plane.data.clear();
        BoxBlurFilter::new().apply(&mut no_plane);
        assert!(no_plane.data.is_empty());
    }

    #[test]
    fn blur_truncated_plane_is_noop() {
        // Declared 4x4 but only one pixel of data: must not index out of bounds.
        let mut frame = Frame::new_video(4, 4, PixelFormat::Rgba);
        frame.data[0] = vec![9u8; 4];
        BoxBlurFilter::new().apply(&mut frame);
        assert_eq!(frame.data[0], vec![9u8; 4]);
    }

    #[test]
    fn blur_of_single_pixel_frame_is_identity() {
        // The window is clamped at the borders, so a 1x1 frame averages to itself.
        let mut frame = rgba_frame(1, 1, &[(12, 34, 56, 78)]);
        BoxBlurFilter::new().apply(&mut frame);
        assert_eq!(frame.data[0], vec![12, 34, 56, 78]);
    }

    #[test]
    fn blur_preserves_alpha_and_averages_neighbourhood() {
        // 3x1 frame: 0, 90, 0 → interior average is (0+90+0)/3 = 30.
        let mut frame = rgba_frame(3, 1, &[(0, 0, 0, 255), (90, 90, 90, 7), (0, 0, 0, 255)]);
        BoxBlurFilter::new().apply(&mut frame);
        let p = &frame.data[0];
        // Edge pixel 0 averages the two available in-bounds samples: (0+90)/2 = 45.
        assert_eq!(p[0], 45);
        // Interior pixel 1 averages all three: 30.
        assert_eq!(p[4], 30);
        assert_eq!(p[5], 30);
        assert_eq!(p[6], 30);
        // Alpha is copied verbatim from the source centre pixel.
        assert_eq!(p[3], 255);
        assert_eq!(p[7], 7);
        assert_eq!(p[11], 255);
    }

    #[test]
    fn blur_non_rgba_is_noop() {
        let mut frame = Frame::new_video(2, 2, PixelFormat::Rgb24);
        let orig = vec![10u8, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120];
        frame.data[0] = orig.clone();
        BoxBlurFilter::new().apply(&mut frame);
        assert_eq!(frame.data[0], orig);
        assert_eq!(frame.pixel_format, PixelFormat::Rgb24);
    }

    #[test]
    fn blur_honors_padded_linesize() {
        // 3x3 frame with 4 padding bytes (0xEE) per row: linesize = 3*4 + 4 = 16.
        let mut frame = Frame::new_video(3, 3, PixelFormat::Rgba);
        let stride = 16usize;
        let mut buf = vec![0xEEu8; stride * 3];
        for y in 0..3 {
            for x in 0..3 {
                let off = y * stride + x * 4;
                let v = if x == 1 && y == 1 { 90 } else { 0 };
                buf[off] = v;
                buf[off + 1] = v;
                buf[off + 2] = v;
                buf[off + 3] = 255;
            }
        }
        frame.data[0] = buf;
        frame.linesize[0] = stride;

        BoxBlurFilter::new().apply(&mut frame);
        assert_eq!(frame.linesize, vec![12]);
        assert_eq!(frame.data[0].len(), 3 * 12);
        // Center pixel averages 8 zeros + 90 -> 10; padding 0xEE must never bleed in.
        let center_off = 4 * 4;
        assert_eq!(
            &frame.data[0][center_off..center_off + 4],
            &[10, 10, 10, 255]
        );
        // Top-left corner averages 4 pixels: (0+0+0+90)/4 = 22.
        assert_eq!(&frame.data[0][0..4], &[22, 22, 22, 255]);
    }
}
