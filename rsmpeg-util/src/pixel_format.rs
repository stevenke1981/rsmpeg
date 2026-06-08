//! Pixel format definitions.

/// A color space / pixel format identifier.
///
/// Analogous to `AVPixelFormat` in FFmpeg.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PixelFormat {
    /// No pixel format specified.
    None,
    /// YUV 4:2:0 planar (8-bit).
    Yuv420p,
    /// RGB packed 24-bit.
    Rgb24,
    /// RGBA packed 32-bit.
    Rgba,
}

impl PixelFormat {
    /// Return the number of bits per pixel for this format.
    pub fn bits_per_pixel(&self) -> u32 {
        match self {
            PixelFormat::None => 0,
            PixelFormat::Yuv420p => 12,
            PixelFormat::Rgb24 => 24,
            PixelFormat::Rgba => 32,
        }
    }
}
