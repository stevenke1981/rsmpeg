//! Conversion of decoded RGBA8 video frames into GPUI [`RenderImage`] textures.
//!
//! GPUI's DirectX / Metal sprite atlas expects premultiplied BGRA8 pixels inside
//! an [`image::RgbaImage`] buffer. Opaque video frames (alpha = 255) only need a
//! red/blue channel swap.

use std::path::Path;
use std::sync::Arc;

use gpui::RenderImage;
use image::{Frame, ImageFormat, RgbaImage};
use smallvec::smallvec;

use crate::state::rgba_to_bgra_bytes;

/// Non-destructive video presentation and color adjustments applied before rendering or saving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoTransform {
    pub rotation_deg: u16,
    pub mirror_horizontal: bool,
    pub grayscale: bool,
    pub brightness: i16,
    pub contrast_pct: u16,
}

impl Default for VideoTransform {
    fn default() -> Self {
        Self {
            rotation_deg: 0,
            mirror_horizontal: false,
            grayscale: false,
            brightness: 0,
            contrast_pct: 100,
        }
    }
}

impl VideoTransform {
    /// Return `true` when no rotation, mirroring, grayscale, brightness, or contrast adjustment is active.
    pub fn is_identity(&self) -> bool {
        self.rotation_deg.is_multiple_of(360)
            && !self.mirror_horizontal
            && !self.grayscale
            && self.brightness == 0
            && self.contrast_pct == 100
    }

    /// Rotate clockwise by 90 degrees (`0 -> 90 -> 180 -> 270 -> 0`).
    pub fn rotate_cw(&mut self) {
        self.rotation_deg = (self.rotation_deg + 90) % 360;
    }

    /// Adjust brightness offset within `-50..=50`.
    pub fn adjust_brightness(&mut self, delta: i16) {
        self.brightness = self.brightness.saturating_add(delta).clamp(-50, 50);
    }

    /// Adjust contrast percentage within `50..=150`.
    pub fn adjust_contrast(&mut self, delta: i16) {
        let next = i32::from(self.contrast_pct) + i32::from(delta);
        self.contrast_pct = next.clamp(50, 150) as u16;
    }

    /// Reset all video adjustments back to neutral defaults.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Apply [`VideoTransform`] to a packed RGBA8 pixel buffer, returning `(out_w, out_h, out_rgba)`.
pub fn apply_video_transform(
    width: usize,
    height: usize,
    rgba: &[u8],
    transform: VideoTransform,
) -> Option<(usize, usize, Vec<u8>)> {
    if width == 0 || height == 0 {
        return None;
    }
    let expected = width.checked_mul(height)?.checked_mul(4)?;
    if rgba.len() < expected {
        return None;
    }
    if transform.is_identity() {
        return Some((width, height, rgba[..expected].to_vec()));
    }

    let bright_offset = i32::from(transform.brightness) * 255 / 100;
    let contrast = i32::from(transform.contrast_pct);
    let mut lut = [0u8; 256];
    for (idx, slot) in lut.iter_mut().enumerate() {
        let v = idx as i32 + bright_offset;
        let contrasted = ((v - 128) * contrast) / 100 + 128;
        *slot = contrasted.clamp(0, 255) as u8;
    }

    let rot = transform.rotation_deg % 360;
    let (out_w, out_h) = match rot {
        90 | 270 => (height, width),
        _ => (width, height),
    };

    let mut out = vec![0u8; expected];
    for sy in 0..height {
        for sx in 0..width {
            let src_off = (sy * width + sx) * 4;
            let mut r = lut[rgba[src_off] as usize];
            let mut g = lut[rgba[src_off + 1] as usize];
            let mut b = lut[rgba[src_off + 2] as usize];
            let a = rgba[src_off + 3];

            if transform.grayscale {
                let luma =
                    ((77 * u32::from(r) + 150 * u32::from(g) + 29 * u32::from(b)) >> 8) as u8;
                r = luma;
                g = luma;
                b = luma;
            }

            let mx = if transform.mirror_horizontal {
                width - 1 - sx
            } else {
                sx
            };
            let (dx, dy) = match rot {
                90 => (height - 1 - sy, mx),
                180 => (width - 1 - mx, height - 1 - sy),
                270 => (sy, width - 1 - mx),
                _ => (mx, sy),
            };
            let dst_off = (dy * out_w + dx) * 4;
            out[dst_off] = r;
            out[dst_off + 1] = g;
            out[dst_off + 2] = b;
            out[dst_off + 3] = a;
        }
    }

    Some((out_w, out_h, out))
}

/// Build a GPUI [`RenderImage`] from a packed RGBA8 frame buffer.
///
/// Returns `None` if dimensions are zero, exceed `u32`, or do not match `rgba.len()`.
pub fn build_render_image(width: usize, height: usize, rgba: &[u8]) -> Option<Arc<RenderImage>> {
    if width == 0 || height == 0 {
        return None;
    }
    let expected = width.checked_mul(height)?.checked_mul(4)?;
    if rgba.len() < expected {
        return None;
    }
    let w = u32::try_from(width).ok()?;
    let h = u32::try_from(height).ok()?;
    let bgra = rgba_to_bgra_bytes(&rgba[..expected]);
    let buffer = RgbaImage::from_raw(w, h, bgra)?;
    Some(Arc::new(RenderImage::new(smallvec![Frame::new(buffer)])))
}

/// Build a GPUI [`RenderImage`] after applying [`VideoTransform`].
pub fn build_render_image_transformed(
    width: usize,
    height: usize,
    rgba: &[u8],
    transform: VideoTransform,
) -> Option<Arc<RenderImage>> {
    if transform.is_identity() {
        return build_render_image(width, height, rgba);
    }
    let (out_w, out_h, transformed) = apply_video_transform(width, height, rgba, transform)?;
    build_render_image(out_w, out_h, &transformed)
}

/// Apply [`VideoTransform`] and encode the frame as a PNG image at `path`.
pub fn save_screenshot_png(
    path: &Path,
    width: usize,
    height: usize,
    rgba: &[u8],
    transform: VideoTransform,
) -> Result<(), String> {
    let (out_w, out_h, pixels) = apply_video_transform(width, height, rgba, transform)
        .ok_or_else(|| "invalid video frame buffer".to_string())?;
    let w = u32::try_from(out_w).map_err(|_| "frame width exceeds u32".to_string())?;
    let h = u32::try_from(out_h).map_err(|_| "frame height exceeds u32".to_string())?;
    let buffer = RgbaImage::from_raw(w, h, pixels)
        .ok_or_else(|| "failed to build RGBA image buffer".to_string())?;
    buffer
        .save_with_format(path, ImageFormat::Png)
        .map_err(|e| format!("failed to write PNG screenshot: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_render_image_validates_dimensions_and_swaps_channels() {
        assert!(build_render_image(0, 10, &[]).is_none());
        assert!(build_render_image(2, 2, &[0u8; 8]).is_none());

        let rgba = vec![255u8, 10, 20, 255, 1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255];
        let image = build_render_image(2, 2, &rgba).expect("valid 2x2 RGBA frame");
        assert_eq!(image.frame_count(), 1);
        let size = image.size(0);
        assert_eq!(size.width.0, 2);
        assert_eq!(size.height.0, 2);
    }

    #[test]
    fn video_transform_clamps_and_cycles_cleanly() {
        let mut t = VideoTransform::default();
        assert!(t.is_identity());

        t.rotate_cw();
        assert_eq!(t.rotation_deg, 90);
        assert!(!t.is_identity());
        t.rotate_cw();
        assert_eq!(t.rotation_deg, 180);
        t.rotate_cw();
        assert_eq!(t.rotation_deg, 270);
        t.rotate_cw();
        assert_eq!(t.rotation_deg, 0);
        assert!(t.is_identity());

        t.adjust_brightness(100);
        assert_eq!(t.brightness, 50);
        t.adjust_brightness(-200);
        assert_eq!(t.brightness, -50);

        t.adjust_contrast(100);
        assert_eq!(t.contrast_pct, 150);
        t.adjust_contrast(-200);
        assert_eq!(t.contrast_pct, 50);

        t.mirror_horizontal = true;
        t.grayscale = true;
        t.reset();
        assert!(t.is_identity());
    }

    #[test]
    fn apply_video_transform_rotates_and_mirrors_corners_accurately() {
        // 2x3 image with distinguishable red channels 10..60.
        let rgba = vec![
            10, 0, 0, 255, 20, 0, 0, 255, // row 0
            30, 0, 0, 255, 40, 0, 0, 255, // row 1
            50, 0, 0, 255, 60, 0, 0, 255, // row 2
        ];

        // 90 deg CW -> 3x2: top-left=50, top-right=10, bottom-left=60, bottom-right=20
        let (w90, h90, p90) = apply_video_transform(
            2,
            3,
            &rgba,
            VideoTransform {
                rotation_deg: 90,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!((w90, h90), (3, 2));
        assert_eq!(p90[0], 50);
        assert_eq!(p90[8], 10);
        assert_eq!(p90[12], 60);
        assert_eq!(p90[20], 20);

        // 180 deg -> 2x3: top-left=60, bottom-right=10
        let (w180, h180, p180) = apply_video_transform(
            2,
            3,
            &rgba,
            VideoTransform {
                rotation_deg: 180,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!((w180, h180), (2, 3));
        assert_eq!(p180[0], 60);
        assert_eq!(p180[20], 10);

        // 270 deg CW -> 3x2: top-left=20, top-right=60, bottom-left=10, bottom-right=50
        let (w270, h270, p270) = apply_video_transform(
            2,
            3,
            &rgba,
            VideoTransform {
                rotation_deg: 270,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!((w270, h270), (3, 2));
        assert_eq!(p270[0], 20);
        assert_eq!(p270[8], 60);
        assert_eq!(p270[12], 10);
        assert_eq!(p270[20], 50);

        // Mirror horizontal -> swaps left and right columns
        let (wm, hm, pm) = apply_video_transform(
            2,
            3,
            &rgba,
            VideoTransform {
                mirror_horizontal: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!((wm, hm), (2, 3));
        assert_eq!(pm[0], 20);
        assert_eq!(pm[4], 10);

        let img_rot = build_render_image_transformed(
            2,
            3,
            &rgba,
            VideoTransform {
                rotation_deg: 90,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(img_rot.size(0).width.0, 3);
        assert_eq!(img_rot.size(0).height.0, 2);
    }

    #[test]
    fn apply_video_transform_applies_grayscale_brightness_contrast_and_saves_png() {
        let rgba = vec![200, 100, 50, 255, 100, 128, 240, 255];

        // Grayscale BT.601 luma check on pixel 0
        let (_, _, gray) = apply_video_transform(
            2,
            1,
            &rgba,
            VideoTransform {
                grayscale: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(&gray[0..4], &[124, 124, 124, 255]);

        // Brightness +20 (+51) check on pixel 1
        let (_, _, bright) = apply_video_transform(
            2,
            1,
            &rgba,
            VideoTransform {
                brightness: 20,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(&bright[4..8], &[151, 179, 255, 255]);

        // Contrast 150% check on pixel 0 (R=200 -> 236, G=100 -> 86)
        let (_, _, contrast) = apply_video_transform(
            2,
            1,
            &rgba,
            VideoTransform {
                contrast_pct: 150,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(contrast[0], 236);
        assert_eq!(contrast[1], 86);

        // Save PNG screenshot and check PNG signature
        let path = std::env::temp_dir().join("rsmpeg_gui_screenshot_test.png");
        let _ = std::fs::remove_file(&path);
        save_screenshot_png(
            &path,
            2,
            1,
            &rgba,
            VideoTransform {
                rotation_deg: 90,
                grayscale: true,
                ..Default::default()
            },
        )
        .expect("screenshot save succeeds");
        let bytes = std::fs::read(&path).expect("saved PNG readable");
        let _ = std::fs::remove_file(&path);
        assert!(bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]));
    }
}
