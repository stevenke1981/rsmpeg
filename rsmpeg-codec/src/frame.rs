use crate::PictureType;
use rsmpeg_util::{PixelFormat, Rational, SampleFormat};

/// Uncompressed media frame, equivalent to FFmpeg's AVFrame.
#[derive(Debug, Clone)]
pub struct Frame {
    pub data: Vec<Vec<u8>>,
    pub linesize: Vec<usize>,
    pub width: usize,
    pub height: usize,
    pub pixel_format: PixelFormat,
    pub sample_format: SampleFormat,
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: usize,
    pub pts: Option<i64>,
    pub duration: i64,
    pub time_base: Rational,
    pub key_frame: bool,
    pub pict_type: PictureType,
}

impl Frame {
    /// Allocate a video frame with correct per-plane sizes for `pix_fmt`.
    ///
    /// Plane layout (8-bit unless noted):
    /// - YUV420P: Y=`w*h`, U/V=`(w/2)*(h/2)`, linesize Y=`w`, U/V=`w/2`
    /// - YUV422P: Y=`w*h`, U/V=`(w/2)*h`, linesize Y=`w`, U/V=`w/2`
    /// - YUV444P: Y/U/V=`w*h`, linesize=`w`
    /// - NV12/NV21: Y=`w*h`, UV=`w*(h/2)`, linesize Y=`w`, UV=`w`
    /// - RGB24/BGR24: 1 plane `w*h*3`, linesize=`w*3`
    /// - RGBA/BGRA/ARGB: 1 plane `w*h*4`, linesize=`w*4`
    /// - Gray8: 1 plane `w*h`, linesize=`w`
    pub fn new_video(width: usize, height: usize, pix_fmt: PixelFormat) -> Self {
        let plane_info = pix_fmt.plane_sizes(width, height);
        let data = plane_info
            .iter()
            .map(|(size, _)| vec![0u8; *size])
            .collect();
        let linesize = plane_info.iter().map(|(_, ls)| *ls).collect();

        Frame {
            data,
            linesize,
            width,
            height,
            pixel_format: pix_fmt,
            sample_format: SampleFormat::None,
            sample_rate: 0,
            channels: 0,
            samples: 0,
            pts: None,
            duration: 0,
            time_base: Rational::new(1, 1000),
            key_frame: false,
            pict_type: PictureType::None,
        }
    }

    pub fn new_audio(
        sample_format: SampleFormat,
        sample_rate: u32,
        channels: u16,
        samples: usize,
    ) -> Self {
        let bytes_per_sample = sample_format.bytes();
        let total_bytes = samples * channels as usize * bytes_per_sample;
        Frame {
            data: vec![vec![0u8; total_bytes]],
            linesize: vec![total_bytes],
            width: 0,
            height: 0,
            pixel_format: PixelFormat::None,
            sample_format,
            sample_rate,
            channels,
            samples,
            pts: None,
            duration: 0,
            time_base: Rational::new(1, sample_rate as i32),
            key_frame: true,
            pict_type: PictureType::I,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_video_yuv420p_planes() {
        let w = 64;
        let h = 48;
        let frame = Frame::new_video(w, h, PixelFormat::Yuv420P);
        assert_eq!(frame.data.len(), 3);
        assert_eq!(frame.linesize, vec![w, w / 2, w / 2]);
        assert_eq!(frame.data[0].len(), w * h);
        assert_eq!(frame.data[1].len(), (w / 2) * (h / 2));
        assert_eq!(frame.data[2].len(), (w / 2) * (h / 2));
    }

    #[test]
    fn test_new_video_yuv422p_planes() {
        let w = 64;
        let h = 48;
        let frame = Frame::new_video(w, h, PixelFormat::Yuv422P);
        assert_eq!(frame.data.len(), 3);
        assert_eq!(frame.linesize, vec![w, w / 2, w / 2]);
        assert_eq!(frame.data[0].len(), w * h);
        assert_eq!(frame.data[1].len(), (w / 2) * h);
        assert_eq!(frame.data[2].len(), (w / 2) * h);
    }

    #[test]
    fn test_new_video_yuv444p_planes() {
        let w = 32;
        let h = 16;
        let frame = Frame::new_video(w, h, PixelFormat::Yuv444P);
        assert_eq!(frame.data.len(), 3);
        assert_eq!(frame.linesize, vec![w, w, w]);
        assert_eq!(frame.data[0].len(), w * h);
        assert_eq!(frame.data[1].len(), w * h);
        assert_eq!(frame.data[2].len(), w * h);
    }

    #[test]
    fn test_new_video_nv12_planes() {
        let w = 64;
        let h = 48;
        let frame = Frame::new_video(w, h, PixelFormat::Nv12);
        assert_eq!(frame.data.len(), 2);
        assert_eq!(frame.linesize, vec![w, w]);
        assert_eq!(frame.data[0].len(), w * h);
        assert_eq!(frame.data[1].len(), w * (h / 2));
    }

    #[test]
    fn test_new_video_rgb24_rgba_gray8() {
        let w = 10;
        let h = 8;

        let rgb = Frame::new_video(w, h, PixelFormat::Rgb24);
        assert_eq!(rgb.data.len(), 1);
        assert_eq!(rgb.linesize, vec![w * 3]);
        assert_eq!(rgb.data[0].len(), w * h * 3);

        let rgba = Frame::new_video(w, h, PixelFormat::Rgba);
        assert_eq!(rgba.data.len(), 1);
        assert_eq!(rgba.linesize, vec![w * 4]);
        assert_eq!(rgba.data[0].len(), w * h * 4);

        let gray = Frame::new_video(w, h, PixelFormat::Gray8);
        assert_eq!(gray.data.len(), 1);
        assert_eq!(gray.linesize, vec![w]);
        assert_eq!(gray.data[0].len(), w * h);
    }

    #[test]
    fn new_audio_sets_fields() {
        // Existing factory signature: new_audio(sample_format, sample_rate, channels, samples)
        let frame = Frame::new_audio(SampleFormat::S16, 44_100, 2, 1024);
        assert_eq!(frame.channels, 2);
        assert_eq!(frame.sample_rate, 44_100);
        assert_eq!(frame.sample_format, SampleFormat::S16);
        assert_eq!(frame.samples, 1024);
        assert_eq!(frame.pixel_format, PixelFormat::None);
        assert_eq!(frame.width, 0);
        assert_eq!(frame.height, 0);
        assert_eq!(frame.time_base, Rational::new(1, 44_100));
        assert_eq!(frame.pts, None);
        assert!(frame.key_frame);
        assert_eq!(frame.pict_type, PictureType::I);
    }
}
