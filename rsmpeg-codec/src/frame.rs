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
    pub fn new_video(width: usize, height: usize, pix_fmt: PixelFormat) -> Self {
        let planes = pix_fmt.planes();
        let total_pixels = width * height;
        let data = (0..planes).map(|_| vec![0u8; total_pixels]).collect();
        let linesize = vec![width; planes];

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
