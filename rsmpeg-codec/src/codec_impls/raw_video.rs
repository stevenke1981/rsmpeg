//! Raw video codec — copies frames as-is without compression.

use bytes::Bytes;

use crate::codec::{Codec, CodecCapabilities, Decoder, Encoder};
use crate::codec_id::CodecId;
use crate::codec_parameters::CodecParameters;
use crate::frame::Frame;
use crate::packet::{Packet, PacketFlags};
use rsmpeg_util::{MediaType, RsResult};

/// Raw video codec — copies frames as-is without compression.
pub struct RawVideoCodec;

impl Codec for RawVideoCodec {
    fn id(&self) -> CodecId {
        CodecId::RawVideo
    }
    fn media_type(&self) -> MediaType {
        MediaType::Video
    }
    fn name(&self) -> &'static str {
        "rawvideo"
    }
    fn long_name(&self) -> &'static str {
        "Raw video"
    }
    fn capabilities(&self) -> CodecCapabilities {
        CodecCapabilities {
            can_decode: true,
            can_encode: true,
            lossless: true,
            intra_only: true,
        }
    }
    fn create_decoder(&self) -> RsResult<Box<dyn Decoder>> {
        Ok(Box::new(RawVideoDecoder::new()))
    }
    fn create_encoder(&self) -> RsResult<Box<dyn Encoder>> {
        Ok(Box::new(RawVideoEncoder::new()))
    }
}

/// Decoder for raw video — each packet becomes one frame.
pub struct RawVideoDecoder {
    params: Option<CodecParameters>,
}

impl RawVideoDecoder {
    pub fn new() -> Self {
        RawVideoDecoder { params: None }
    }

    #[allow(dead_code)]
    pub fn set_parameters(&mut self, params: CodecParameters) {
        self.params = Some(params);
    }
}

impl Default for RawVideoDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder for RawVideoDecoder {
    fn codec_id(&self) -> CodecId {
        CodecId::RawVideo
    }

    fn decode(&mut self, packet: &Packet) -> RsResult<Vec<Frame>> {
        let data = packet.data.to_vec();
        let frame = Frame {
            data: vec![data],
            linesize: vec![packet.data.len()],
            width: self.params.as_ref().and_then(|p| p.width).unwrap_or(0),
            height: self.params.as_ref().and_then(|p| p.height).unwrap_or(0),
            pixel_format: self
                .params
                .as_ref()
                .and_then(|p| p.pixel_format)
                .unwrap_or(rsmpeg_util::PixelFormat::None),
            sample_format: rsmpeg_util::SampleFormat::None,
            sample_rate: 0,
            channels: 0,
            samples: 0,
            pts: packet.pts,
            duration: packet.duration,
            time_base: packet.time_base,
            key_frame: packet.is_key(),
            pict_type: crate::picture_type::PictureType::None,
        };
        Ok(vec![frame])
    }

    fn flush(&mut self) -> RsResult<Vec<Frame>> {
        Ok(vec![])
    }

    fn get_parameters(&self) -> CodecParameters {
        self.params
            .clone()
            .unwrap_or(CodecParameters::new(CodecId::RawVideo))
    }
}

/// Encoder for raw video — each frame becomes one packet.
pub struct RawVideoEncoder {
    params: Option<CodecParameters>,
}

impl RawVideoEncoder {
    pub fn new() -> Self {
        RawVideoEncoder { params: None }
    }

    #[allow(dead_code)]
    pub fn set_parameters(&mut self, params: CodecParameters) {
        self.params = Some(params);
    }
}

impl Default for RawVideoEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Encoder for RawVideoEncoder {
    fn codec_id(&self) -> CodecId {
        CodecId::RawVideo
    }

    fn encode(&mut self, frame: &Frame) -> RsResult<Vec<Packet>> {
        let data: Vec<u8> = frame.data.concat();
        let packet = Packet {
            data: Bytes::from(data),
            pts: frame.pts,
            dts: None,
            duration: frame.duration,
            stream_index: 0,
            flags: if frame.key_frame {
                PacketFlags::KEY
            } else {
                PacketFlags::empty()
            },
            pos: -1,
            time_base: frame.time_base,
        };
        Ok(vec![packet])
    }

    fn flush(&mut self) -> RsResult<Vec<Packet>> {
        Ok(vec![])
    }

    fn get_parameters(&self) -> CodecParameters {
        self.params
            .clone()
            .unwrap_or(CodecParameters::new(CodecId::RawVideo))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use rsmpeg_util::Rational;

    fn test_time_base() -> Rational {
        Rational::new(1, 1000)
    }

    #[test]
    fn test_raw_video_codec_metadata() {
        let codec = RawVideoCodec;
        assert_eq!(codec.id(), CodecId::RawVideo);
        assert_eq!(codec.media_type(), MediaType::Video);
        assert_eq!(codec.name(), "rawvideo");
        assert!(codec.capabilities().can_decode);
        assert!(codec.capabilities().can_encode);
        assert!(codec.capabilities().lossless);
    }

    #[test]
    fn test_raw_video_decode() {
        let codec = RawVideoCodec;
        let mut decoder = codec.create_decoder().unwrap();
        let packet = Packet {
            data: Bytes::from(vec![0u8; 100]),
            pts: Some(0),
            dts: Some(0),
            duration: 1,
            stream_index: 0,
            flags: PacketFlags::KEY,
            pos: 0,
            time_base: test_time_base(),
        };
        let frames = decoder.decode(&packet).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data.len(), 1);
        assert_eq!(frames[0].data[0].len(), 100);
    }

    fn make_params() -> CodecParameters {
        CodecParameters {
            codec_id: CodecId::RawVideo,
            media_type: MediaType::Video,
            width: Some(10),
            height: Some(10),
            pixel_format: Some(rsmpeg_util::PixelFormat::Gray8),
            sample_rate: None,
            channels: None,
            sample_format: None,
            bit_rate: None,
            extradata: None,
        }
    }

    #[test]
    fn test_raw_video_decode_with_params() {
        let mut decoder = RawVideoDecoder::new();
        decoder.set_parameters(make_params());

        let packet = Packet {
            data: Bytes::from(vec![0u8; 100]),
            pts: Some(0),
            dts: Some(0),
            duration: 1,
            stream_index: 0,
            flags: PacketFlags::KEY,
            pos: 0,
            time_base: test_time_base(),
        };
        let frames = decoder.decode(&packet).unwrap();
        assert_eq!(frames[0].width, 10);
        assert_eq!(frames[0].height, 10);
    }

    #[test]
    fn test_raw_video_encode() {
        let codec = RawVideoCodec;
        let mut encoder = codec.create_encoder().unwrap();
        let frame = Frame {
            data: vec![vec![0u8; 64]],
            linesize: vec![8],
            width: 8,
            height: 8,
            pixel_format: rsmpeg_util::PixelFormat::Gray8,
            sample_format: rsmpeg_util::SampleFormat::None,
            sample_rate: 0,
            channels: 0,
            samples: 0,
            pts: Some(0),
            duration: 1,
            time_base: test_time_base(),
            key_frame: true,
            pict_type: crate::picture_type::PictureType::I,
        };
        let packets = encoder.encode(&frame).unwrap();
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0].data.len(), 64);
    }

    #[test]
    fn test_raw_video_flush() {
        let mut decoder = RawVideoDecoder::new();
        let frames = decoder.flush().unwrap();
        assert!(frames.is_empty());

        let mut encoder = RawVideoEncoder::new();
        let packets = encoder.flush().unwrap();
        assert!(packets.is_empty());
    }
}
