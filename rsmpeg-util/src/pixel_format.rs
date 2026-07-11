use serde::{Deserialize, Serialize};

/// Pixel format, equivalent to FFmpeg's AVPixelFormat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PixelFormat {
    /// YUV 4:2:0 planar (8-bit)
    Yuv420P,
    /// YUV 4:2:2 planar (8-bit)
    Yuv422P,
    /// YUV 4:4:4 planar (8-bit)
    Yuv444P,
    /// YUV 4:2:0 semi-planar (NV12)
    Nv12,
    /// YUV 4:2:0 semi-planar (NV21)
    Nv21,
    /// RGB 24-bit
    Rgb24,
    /// BGR 24-bit
    Bgr24,
    /// RGBA 32-bit
    Rgba,
    /// BGRA 32-bit
    Bgra,
    /// ARGB 32-bit
    Argb,
    /// Gray 8-bit
    Gray8,
    /// Gray 16-bit
    Gray16,
    /// YUV 4:2:0 10-bit planar
    Yuv420P10,
    /// YUV 4:2:0 12-bit planar
    Yuv420P12,
    /// None / unknown
    None,
}

impl PixelFormat {
    pub fn name(self) -> &'static str {
        match self {
            PixelFormat::Yuv420P => "yuv420p",
            PixelFormat::Yuv422P => "yuv422p",
            PixelFormat::Yuv444P => "yuv444p",
            PixelFormat::Nv12 => "nv12",
            PixelFormat::Nv21 => "nv21",
            PixelFormat::Rgb24 => "rgb24",
            PixelFormat::Bgr24 => "bgr24",
            PixelFormat::Rgba => "rgba",
            PixelFormat::Bgra => "bgra",
            PixelFormat::Argb => "argb",
            PixelFormat::Gray8 => "gray8",
            PixelFormat::Gray16 => "gray16",
            PixelFormat::Yuv420P10 => "yuv420p10",
            PixelFormat::Yuv420P12 => "yuv420p12",
            PixelFormat::None => "none",
        }
    }

    /// Number of bits per pixel (approximate).
    pub fn bits_per_pixel(self) -> usize {
        match self {
            PixelFormat::Yuv420P => 12,
            PixelFormat::Yuv422P => 16,
            PixelFormat::Yuv444P => 24,
            PixelFormat::Nv12 | PixelFormat::Nv21 => 12,
            PixelFormat::Rgb24 | PixelFormat::Bgr24 => 24,
            PixelFormat::Rgba | PixelFormat::Bgra | PixelFormat::Argb => 32,
            PixelFormat::Gray8 => 8,
            PixelFormat::Gray16 => 16,
            PixelFormat::Yuv420P10 => 15,
            PixelFormat::Yuv420P12 => 18,
            PixelFormat::None => 0,
        }
    }

    /// Number of planes.
    pub fn planes(self) -> usize {
        match self {
            PixelFormat::Yuv420P
            | PixelFormat::Yuv422P
            | PixelFormat::Yuv444P
            | PixelFormat::Yuv420P10
            | PixelFormat::Yuv420P12 => 3,
            PixelFormat::Nv12 | PixelFormat::Nv21 => 2,
            PixelFormat::None => 0,
            _ => 1,
        }
    }

    /// Per-plane `(byte_size, linesize)` for a frame of `width` × `height`.
    ///
    /// Uses floor division for chroma dimensions (FFmpeg-compatible).
    /// 10/12-bit planar formats are stored as 16-bit samples (2 bytes/component).
    pub fn plane_sizes(self, width: usize, height: usize) -> Vec<(usize, usize)> {
        match self {
            PixelFormat::Yuv420P => {
                let cw = width / 2;
                let ch = height / 2;
                vec![(width * height, width), (cw * ch, cw), (cw * ch, cw)]
            }
            PixelFormat::Yuv422P => {
                let cw = width / 2;
                vec![
                    (width * height, width),
                    (cw * height, cw),
                    (cw * height, cw),
                ]
            }
            PixelFormat::Yuv444P => {
                let plane = width * height;
                vec![(plane, width), (plane, width), (plane, width)]
            }
            PixelFormat::Nv12 | PixelFormat::Nv21 => {
                // Y plane + interleaved UV (height/2 rows × width bytes)
                vec![(width * height, width), (width * (height / 2), width)]
            }
            PixelFormat::Rgb24 | PixelFormat::Bgr24 => {
                vec![(width * height * 3, width * 3)]
            }
            PixelFormat::Rgba | PixelFormat::Bgra | PixelFormat::Argb => {
                vec![(width * height * 4, width * 4)]
            }
            PixelFormat::Gray8 => vec![(width * height, width)],
            PixelFormat::Gray16 => vec![(width * height * 2, width * 2)],
            // 10/12-bit stored in 16-bit little-endian samples
            PixelFormat::Yuv420P10 | PixelFormat::Yuv420P12 => {
                let cw = width / 2;
                let ch = height / 2;
                vec![
                    (width * height * 2, width * 2),
                    (cw * ch * 2, cw * 2),
                    (cw * ch * 2, cw * 2),
                ]
            }
            PixelFormat::None => Vec::new(),
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "yuv420p" => Some(PixelFormat::Yuv420P),
            "yuv422p" => Some(PixelFormat::Yuv422P),
            "yuv444p" => Some(PixelFormat::Yuv444P),
            "nv12" => Some(PixelFormat::Nv12),
            "nv21" => Some(PixelFormat::Nv21),
            "rgb24" => Some(PixelFormat::Rgb24),
            "bgr24" => Some(PixelFormat::Bgr24),
            "rgba" => Some(PixelFormat::Rgba),
            "bgra" => Some(PixelFormat::Bgra),
            "argb" => Some(PixelFormat::Argb),
            "gray8" => Some(PixelFormat::Gray8),
            "gray16" => Some(PixelFormat::Gray16),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plane_sizes_yuv420p() {
        let sizes = PixelFormat::Yuv420P.plane_sizes(64, 48);
        assert_eq!(sizes, vec![(64 * 48, 64), (32 * 24, 32), (32 * 24, 32)]);
    }

    #[test]
    fn test_plane_sizes_yuv422p() {
        let sizes = PixelFormat::Yuv422P.plane_sizes(64, 48);
        assert_eq!(sizes, vec![(64 * 48, 64), (32 * 48, 32), (32 * 48, 32)]);
    }

    #[test]
    fn test_plane_sizes_yuv444p() {
        let sizes = PixelFormat::Yuv444P.plane_sizes(16, 8);
        assert_eq!(sizes, vec![(128, 16), (128, 16), (128, 16)]);
    }

    #[test]
    fn test_plane_sizes_nv12() {
        let sizes = PixelFormat::Nv12.plane_sizes(64, 48);
        assert_eq!(sizes, vec![(64 * 48, 64), (64 * 24, 64)]);
    }

    #[test]
    fn test_plane_sizes_packed() {
        assert_eq!(
            PixelFormat::Rgb24.plane_sizes(10, 8),
            vec![(10 * 8 * 3, 30)]
        );
        assert_eq!(PixelFormat::Rgba.plane_sizes(10, 8), vec![(10 * 8 * 4, 40)]);
        assert_eq!(PixelFormat::Gray8.plane_sizes(10, 8), vec![(80, 10)]);
    }
}
