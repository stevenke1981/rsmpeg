//! Raw video codec — copies frames as-is without compression.

use std::collections::VecDeque;

use bytes::Bytes;

use crate::codec::{Codec, CodecCapabilities, DecodeStatus, Decoder, Encoder};
use crate::codec_id::CodecId;
use crate::codec_parameters::CodecParameters;
use crate::frame::Frame;
use crate::packet::{Packet, PacketFlags};
use rsmpeg_util::{MediaType, RsError, RsResult};

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
    pending: VecDeque<Frame>,
    eof: bool,
}

impl RawVideoDecoder {
    pub fn new() -> Self {
        RawVideoDecoder {
            params: None,
            pending: VecDeque::new(),
            eof: false,
        }
    }

    #[allow(dead_code)]
    pub fn set_parameters(&mut self, params: CodecParameters) {
        self.params = Some(params);
    }

    fn packet_to_frame(&self, packet: &Packet) -> Frame {
        Frame {
            data: vec![packet.data.to_vec()],
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
        }
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

    fn send_packet(&mut self, packet: Option<&Packet>) -> RsResult<()> {
        match packet {
            Some(pkt) => {
                if self.eof {
                    return Err(RsError::Codec(
                        "cannot send packet after end-of-stream; call reset() first".into(),
                    ));
                }
                self.pending.push_back(self.packet_to_frame(pkt));
                Ok(())
            }
            None => {
                self.eof = true;
                Ok(())
            }
        }
    }

    fn receive_frame(&mut self) -> RsResult<DecodeStatus> {
        if let Some(frame) = self.pending.pop_front() {
            Ok(DecodeStatus::Frame(frame))
        } else if self.eof {
            Ok(DecodeStatus::EndOfStream)
        } else {
            Ok(DecodeStatus::NeedMoreInput)
        }
    }

    fn reset(&mut self) -> RsResult<()> {
        self.pending.clear();
        self.eof = false;
        Ok(())
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
            h264_bitstream_format: Default::default(),
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
    fn test_raw_video_send_receive() {
        let mut decoder = RawVideoDecoder::new();
        decoder.set_parameters(make_params());

        let packet = Packet {
            data: Bytes::from(vec![1u8; 50]),
            pts: Some(42),
            dts: Some(42),
            duration: 1,
            stream_index: 0,
            flags: PacketFlags::KEY,
            pos: 0,
            time_base: test_time_base(),
        };

        decoder.send_packet(Some(&packet)).unwrap();
        match decoder.receive_frame().unwrap() {
            DecodeStatus::Frame(f) => {
                assert_eq!(f.pts, Some(42));
                assert_eq!(f.data[0].len(), 50);
            }
            other => panic!("expected Frame, got {:?}", other),
        }
        assert!(matches!(
            decoder.receive_frame().unwrap(),
            DecodeStatus::NeedMoreInput
        ));

        decoder.send_packet(None).unwrap();
        assert!(matches!(
            decoder.receive_frame().unwrap(),
            DecodeStatus::EndOfStream
        ));
    }

    #[test]
    fn test_raw_video_reset_after_eos() {
        let mut decoder = RawVideoDecoder::new();
        decoder.send_packet(None).unwrap();
        assert!(matches!(
            decoder.receive_frame().unwrap(),
            DecodeStatus::EndOfStream
        ));
        // Sending after EOS must fail until reset
        let packet = Packet {
            data: Bytes::from(vec![0u8; 4]),
            pts: None,
            dts: None,
            duration: 0,
            stream_index: 0,
            flags: PacketFlags::empty(),
            pos: -1,
            time_base: test_time_base(),
        };
        assert!(decoder.send_packet(Some(&packet)).is_err());
        decoder.reset().unwrap();
        decoder.send_packet(Some(&packet)).unwrap();
        assert!(matches!(
            decoder.receive_frame().unwrap(),
            DecodeStatus::Frame(_)
        ));
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
