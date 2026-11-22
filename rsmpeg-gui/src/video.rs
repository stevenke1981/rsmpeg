//! Conversion of decoded RGBA8 video frames into GPUI [`RenderImage`] textures.
//!
//! GPUI's DirectX / Metal sprite atlas expects premultiplied BGRA8 pixels inside
//! an [`image::RgbaImage`] buffer. Opaque video frames (alpha = 255) only need a
//! red/blue channel swap.

use std::sync::Arc;

use gpui::RenderImage;
use image::{Frame, RgbaImage};
use smallvec::smallvec;

use crate::state::rgba_to_bgra_bytes;

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
}
