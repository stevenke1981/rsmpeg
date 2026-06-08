use crate::codec_id::CodecId;
use crate::frame::Frame;
use crate::packet::Packet;
use rsmpeg_util::{PixelFormat, Rational, RsResult, SampleFormat};

pub struct CodecContext {
    codec_id: CodecId,
    width: Option<usize>,
    height: Option<usize>,
    pixel_format: Option<PixelFormat>,
    sample_rate: Option<u32>,
    channels: Option<u16>,
    sample_format: Option<SampleFormat>,
    bit_rate: Option<u64>,
    time_base: Rational,
}

impl CodecContext {
    pub fn builder() -> CodecContextBuilder {
        CodecContextBuilder::new()
    }
    pub fn open(&mut self) -> RsResult<()> {
        Ok(())
    }
    pub fn decode(&mut self, _packet: &Packet) -> RsResult<Vec<Frame>> {
        Ok(Vec::new())
    }
    pub fn encode(&mut self, _frame: &Frame) -> RsResult<Vec<Packet>> {
        Ok(Vec::new())
    }
    pub fn codec_id(&self) -> CodecId {
        self.codec_id
    }
    pub fn width(&self) -> Option<usize> {
        self.width
    }
    pub fn height(&self) -> Option<usize> {
        self.height
    }
    pub fn pixel_format(&self) -> Option<PixelFormat> {
        self.pixel_format
    }
    pub fn sample_format(&self) -> Option<SampleFormat> {
        self.sample_format
    }
    pub fn sample_rate(&self) -> Option<u32> {
        self.sample_rate
    }
    pub fn channels(&self) -> Option<u16> {
        self.channels
    }
    pub fn bit_rate(&self) -> Option<u64> {
        self.bit_rate
    }
    pub fn time_base(&self) -> Rational {
        self.time_base
    }
}

pub struct CodecContextBuilder {
    codec_id: CodecId,
    width: Option<usize>,
    height: Option<usize>,
    pixel_format: Option<PixelFormat>,
    sample_rate: Option<u32>,
    channels: Option<u16>,
    sample_format: Option<SampleFormat>,
    bit_rate: Option<u64>,
    time_base: Rational,
}

impl CodecContextBuilder {
    pub fn new() -> Self {
        CodecContextBuilder {
            codec_id: CodecId::Unknown,
            width: None,
            height: None,
            pixel_format: None,
            sample_rate: None,
            channels: None,
            sample_format: None,
            bit_rate: None,
            time_base: Rational::new(1, 1000),
        }
    }
    pub fn codec_id(mut self, id: CodecId) -> Self {
        self.codec_id = id;
        self
    }
    pub fn width(mut self, w: usize) -> Self {
        self.width = Some(w);
        self
    }
    pub fn height(mut self, h: usize) -> Self {
        self.height = Some(h);
        self
    }
    pub fn pixel_format(mut self, f: PixelFormat) -> Self {
        self.pixel_format = Some(f);
        self
    }
    pub fn sample_rate(mut self, sr: u32) -> Self {
        self.sample_rate = Some(sr);
        self
    }
    pub fn channels(mut self, ch: u16) -> Self {
        self.channels = Some(ch);
        self
    }
    pub fn sample_format(mut self, f: SampleFormat) -> Self {
        self.sample_format = Some(f);
        self
    }
    pub fn bit_rate(mut self, br: u64) -> Self {
        self.bit_rate = Some(br);
        self
    }
    pub fn time_base(mut self, tb: Rational) -> Self {
        self.time_base = tb;
        self
    }
    pub fn build(self) -> CodecContext {
        CodecContext {
            codec_id: self.codec_id,
            width: self.width,
            height: self.height,
            pixel_format: self.pixel_format,
            sample_rate: self.sample_rate,
            channels: self.channels,
            sample_format: self.sample_format,
            bit_rate: self.bit_rate,
            time_base: self.time_base,
        }
    }
}

impl Default for CodecContextBuilder {
    fn default() -> Self {
        Self::new()
    }
}
