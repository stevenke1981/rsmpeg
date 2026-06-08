use crate::codec::Decoder;
use crate::codec::Encoder;
use crate::codec_id::CodecId;
use crate::codec_registry::global_codec_registry;
use crate::frame::Frame;
use crate::packet::Packet;
use rsmpeg_util::{PixelFormat, Rational, RsError, RsResult, SampleFormat};

/// CodecContext — the bridge between a codec and the media processing pipeline.
///
/// Equivalent to FFmpeg's AVCodecContext.
pub struct CodecContext {
    codec_id: CodecId,
    pub width: Option<usize>,
    pub height: Option<usize>,
    pub pixel_format: Option<PixelFormat>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
    pub sample_format: Option<SampleFormat>,
    pub bit_rate: Option<u64>,
    pub time_base: Rational,
    decoder: Option<Box<dyn Decoder>>,
    encoder: Option<Box<dyn Encoder>>,
    is_open: bool,
}

impl CodecContext {
    pub fn builder() -> CodecContextBuilder {
        CodecContextBuilder::new()
    }

    /// Open the codec context — finds the codec in the registry and creates
    /// a decoder or encoder instance.
    pub fn open(&mut self) -> RsResult<()> {
        let registry = global_codec_registry()
            .read()
            .map_err(|_| RsError::Bug("codec registry lock poisoned".into()))?;

        let codec = registry.find_by_id(self.codec_id).ok_or_else(|| {
            RsError::NotFound(format!("Codec {:?} not found", self.codec_id).into())
        })?;

        // Try to create a decoder first
        if codec.capabilities().can_decode {
            self.decoder = Some(codec.create_decoder()?);
            self.is_open = true;
            tracing::info!("Opened decoder for {:?}", self.codec_id);
            return Ok(());
        }

        // Then try encoder
        if codec.capabilities().can_encode {
            self.encoder = Some(codec.create_encoder()?);
            self.is_open = true;
            tracing::info!("Opened encoder for {:?}", self.codec_id);
            return Ok(());
        }

        Err(RsError::NotFound(
            format!("No decoder or encoder for {:?}", self.codec_id).into(),
        ))
    }

    /// Decode a packet into frames.
    pub fn decode(&mut self, packet: &Packet) -> RsResult<Vec<Frame>> {
        if let Some(ref mut dec) = self.decoder {
            dec.decode(packet)
        } else {
            Err(RsError::Bug("CodecContext not opened for decoding".into()))
        }
    }

    /// Encode a frame into packets.
    pub fn encode(&mut self, frame: &Frame) -> RsResult<Vec<Packet>> {
        if let Some(ref mut enc) = self.encoder {
            enc.encode(frame)
        } else {
            Err(RsError::Bug("CodecContext not opened for encoding".into()))
        }
    }

    /// Flush pending frames/packets.
    pub fn flush_decoder(&mut self) -> RsResult<Vec<Frame>> {
        if let Some(ref mut dec) = self.decoder {
            dec.flush()
        } else {
            Ok(vec![])
        }
    }

    pub fn flush_encoder(&mut self) -> RsResult<Vec<Packet>> {
        if let Some(ref mut enc) = self.encoder {
            enc.flush()
        } else {
            Ok(vec![])
        }
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
    pub fn is_open(&self) -> bool {
        self.is_open
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
            decoder: None,
            encoder: None,
            is_open: false,
        }
    }
}

impl Default for CodecContextBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn test_builder_defaults() {
        let ctx = CodecContext::builder().build();
        assert_eq!(ctx.codec_id(), CodecId::Unknown);
        assert!(!ctx.is_open());
    }

    #[test]
    fn test_builder_with_params() {
        let ctx = CodecContext::builder()
            .codec_id(CodecId::H264)
            .width(1920)
            .height(1080)
            .pixel_format(PixelFormat::Yuv420P)
            .bit_rate(2_000_000)
            .time_base(Rational::new(1, 1000))
            .build();
        assert_eq!(ctx.codec_id(), CodecId::H264);
        assert_eq!(ctx.width(), Some(1920));
        assert_eq!(ctx.height(), Some(1080));
        assert_eq!(ctx.bit_rate(), Some(2_000_000));
    }

    #[test]
    fn test_open_nonexistent_codec() {
        let mut ctx = CodecContext::builder().codec_id(CodecId::H264).build();
        // Should fail because no H264 codec is registered
        assert!(ctx.open().is_err());
        assert!(!ctx.is_open());
    }

    #[test]
    fn test_decode_without_open() {
        let mut ctx = CodecContext::builder().codec_id(CodecId::Mp3).build();
        let packet = Packet::new(Bytes::new(), 0);
        let result = ctx.decode(&packet);
        assert!(result.is_err());
    }
}
