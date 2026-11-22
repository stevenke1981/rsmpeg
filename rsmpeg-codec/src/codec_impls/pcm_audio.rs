//! PCM audio codec — uncompressed audio passthrough.
//!
//! Supports common PCM formats: U8, S16LE, S32LE, F32.

use std::collections::VecDeque;

use bytes::Bytes;

use crate::codec::{Codec, CodecCapabilities, DecodeStatus, Decoder, Encoder};
use crate::codec_id::CodecId;
use crate::codec_parameters::CodecParameters;
use crate::frame::Frame;
use crate::packet::{Packet, PacketFlags};
use rsmpeg_util::{MediaType, RsError, RsResult, SampleFormat};

/// PCM audio codec — uncompressed audio passthrough.
///
/// Differentiates between PCM sub-types via `codec_name` and `sample_format`,
/// while all share `CodecId::Pcm`.
pub struct PCMAudioCodec {
    pub codec_name: &'static str,
    pub sample_format: SampleFormat,
}

impl PCMAudioCodec {
    /// Create a new PCM codec for the given sample format.
    /// Returns `None` for non-PCM sample formats.
    pub fn new(sample_format: SampleFormat) -> Option<Self> {
        let codec_name = match sample_format {
            SampleFormat::U8 => "pcm_u8",
            SampleFormat::S16 => "pcm_s16le",
            SampleFormat::S32 => "pcm_s32le",
            SampleFormat::F32 => "pcm_f32le",
            SampleFormat::F64 => "pcm_f64le",
            // Planar formats are not handled as simple PCM for now
            _ => return None,
        };
        Some(PCMAudioCodec {
            codec_name,
            sample_format,
        })
    }
}

impl Codec for PCMAudioCodec {
    fn id(&self) -> CodecId {
        CodecId::Pcm
    }
    fn media_type(&self) -> MediaType {
        MediaType::Audio
    }
    fn name(&self) -> &'static str {
        self.codec_name
    }
    fn long_name(&self) -> &'static str {
        "PCM audio"
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
        Ok(Box::new(PCMAudioDecoder::new(self.sample_format)))
    }
    fn create_encoder(&self) -> RsResult<Box<dyn Encoder>> {
        Ok(Box::new(PCMAudioEncoder::new(self.sample_format)))
    }
}

/// Decoder for PCM audio — each packet becomes one frame of samples.
pub struct PCMAudioDecoder {
    sample_format: SampleFormat,
    params: Option<CodecParameters>,
    channels: u16,
    sample_rate: u32,
    pending: VecDeque<Frame>,
    eof: bool,
}

impl PCMAudioDecoder {
    pub fn new(sample_format: SampleFormat) -> Self {
        PCMAudioDecoder {
            sample_format,
            params: None,
            channels: 2,
            sample_rate: 44100,
            pending: VecDeque::new(),
            eof: false,
        }
    }

    #[allow(dead_code)]
    pub fn set_parameters(&mut self, params: CodecParameters) {
        if let Some(ch) = params.channels {
            self.channels = ch;
        }
        if let Some(sr) = params.sample_rate {
            self.sample_rate = sr;
        }
        self.params = Some(params);
    }

    fn packet_to_frame(&self, packet: &Packet) -> Frame {
        let bytes_per_sample = self.sample_format.bytes();
        let frame_size = bytes_per_sample * self.channels as usize;
        let nb_samples = packet.data.len().checked_div(frame_size).unwrap_or(0);

        Frame {
            data: vec![packet.data.to_vec()],
            linesize: vec![packet.data.len()],
            width: 0,
            height: 0,
            pixel_format: rsmpeg_util::PixelFormat::None,
            sample_format: self.sample_format,
            sample_rate: self.sample_rate,
            channels: self.channels,
            samples: nb_samples,
            pts: packet.pts,
            duration: packet.duration,
            time_base: packet.time_base,
            key_frame: true,
            pict_type: crate::picture_type::PictureType::I,
        }
    }
}

impl Decoder for PCMAudioDecoder {
    fn codec_id(&self) -> CodecId {
        CodecId::Pcm
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
            .unwrap_or(CodecParameters::new(CodecId::Pcm))
    }
}

/// Encoder for PCM audio — each frame becomes one packet.
pub struct PCMAudioEncoder {
    #[allow(dead_code)]
    sample_format: SampleFormat,
    params: Option<CodecParameters>,
}

impl PCMAudioEncoder {
    pub fn new(sample_format: SampleFormat) -> Self {
        PCMAudioEncoder {
            sample_format,
            params: None,
        }
    }

    #[allow(dead_code)]
    pub fn set_parameters(&mut self, params: CodecParameters) {
        self.params = Some(params);
    }
}

impl Encoder for PCMAudioEncoder {
    fn codec_id(&self) -> CodecId {
        CodecId::Pcm
    }

    fn encode(&mut self, frame: &Frame) -> RsResult<Vec<Packet>> {
        let data: Vec<u8> = frame.data.concat();
        let packet = Packet {
            data: Bytes::from(data),
            pts: frame.pts,
            dts: None,
            duration: frame.duration,
            stream_index: 0,
            flags: PacketFlags::KEY,
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
            .unwrap_or(CodecParameters::new(CodecId::Pcm))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use rsmpeg_util::Rational;

    #[test]
    fn test_pcm_codec_creation_valid() {
        for (fmt, expected_name) in &[
            (SampleFormat::U8, "pcm_u8"),
            (SampleFormat::S16, "pcm_s16le"),
            (SampleFormat::S32, "pcm_s32le"),
            (SampleFormat::F32, "pcm_f32le"),
            (SampleFormat::F64, "pcm_f64le"),
        ] {
            let codec = PCMAudioCodec::new(*fmt).unwrap();
            assert_eq!(codec.id(), CodecId::Pcm);
            assert_eq!(codec.media_type(), MediaType::Audio);
            assert_eq!(codec.name(), *expected_name);
            assert!(codec.capabilities().can_decode);
            assert!(codec.capabilities().can_encode);
        }
    }

    #[test]
    fn test_pcm_codec_creation_invalid() {
        assert!(PCMAudioCodec::new(SampleFormat::None).is_none());
        assert!(PCMAudioCodec::new(SampleFormat::S16P).is_none());
        assert!(PCMAudioCodec::new(SampleFormat::S32P).is_none());
    }

    #[test]
    fn test_pcm_decode() {
        let codec = PCMAudioCodec::new(SampleFormat::S16).unwrap();
        let mut decoder = codec.create_decoder().unwrap();
        // 100 bytes = 50 samples at 16-bit stereo (2 bytes/sample * 2 channels)
        let packet = Packet {
            data: Bytes::from(vec![0u8; 100]),
            pts: Some(0),
            dts: Some(0),
            duration: 1,
            stream_index: 0,
            flags: PacketFlags::KEY,
            pos: 0,
            time_base: Rational::new(1, 1000),
        };
        let frames = decoder.decode(&packet).unwrap();
        assert_eq!(frames.len(), 1);
        // 100 bytes / (2 bytes/sample * 2 channels) = 25 samples
        assert_eq!(frames[0].samples, 25);
        assert_eq!(frames[0].channels, 2);
    }

    #[test]
    fn test_pcm_decode_with_params() {
        let sample_format = SampleFormat::S16;
        let mut decoder = PCMAudioDecoder::new(sample_format);
        decoder.set_parameters(CodecParameters {
            codec_id: CodecId::Pcm,
            media_type: MediaType::Audio,
            width: None,
            height: None,
            pixel_format: None,
            sample_rate: Some(48000),
            channels: Some(1),
            sample_format: Some(sample_format),
            bit_rate: None,
            extradata: None,
            h264_bitstream_format: Default::default(),
        });

        let packet = Packet {
            data: Bytes::from(vec![0u8; 100]),
            pts: Some(0),
            dts: Some(0),
            duration: 1,
            stream_index: 0,
            flags: PacketFlags::KEY,
            pos: 0,
            time_base: Rational::new(1, 48000),
        };
        let frames = decoder.decode(&packet).unwrap();
        assert_eq!(frames.len(), 1);
        // 100 bytes / (2 bytes/sample * 1 channel) = 50 samples
        assert_eq!(frames[0].samples, 50);
        assert_eq!(frames[0].channels, 1);
        assert_eq!(frames[0].sample_rate, 48000);
    }

    #[test]
    fn test_pcm_send_receive() {
        let mut decoder = PCMAudioDecoder::new(SampleFormat::S16);
        let packet = Packet {
            data: Bytes::from(vec![0u8; 40]),
            pts: Some(7),
            dts: Some(7),
            duration: 10,
            stream_index: 0,
            flags: PacketFlags::KEY,
            pos: 0,
            time_base: Rational::new(1, 44100),
        };
        decoder.send_packet(Some(&packet)).unwrap();
        match decoder.receive_frame().unwrap() {
            DecodeStatus::Frame(f) => {
                // 40 / (2 * 2) = 10 samples
                assert_eq!(f.samples, 10);
                assert_eq!(f.pts, Some(7));
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
    fn test_pcm_encode() {
        let codec = PCMAudioCodec::new(SampleFormat::S16).unwrap();
        let mut encoder = codec.create_encoder().unwrap();
        let frame = Frame {
            data: vec![vec![0u8; 64]],
            linesize: vec![64],
            width: 0,
            height: 0,
            pixel_format: rsmpeg_util::PixelFormat::None,
            sample_format: SampleFormat::S16,
            sample_rate: 44100,
            channels: 2,
            samples: 16,
            pts: Some(0),
            duration: 1,
            time_base: Rational::new(1, 1000),
            key_frame: true,
            pict_type: crate::picture_type::PictureType::I,
        };
        let packets = encoder.encode(&frame).unwrap();
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0].data.len(), 64);
    }

    #[test]
    fn test_pcm_flush() {
        let mut decoder = PCMAudioDecoder::new(SampleFormat::S16);
        let frames = decoder.flush().unwrap();
        assert!(frames.is_empty());

        let mut encoder = PCMAudioEncoder::new(SampleFormat::S16);
        let packets = encoder.flush().unwrap();
        assert!(packets.is_empty());
    }

    #[test]
    fn test_pcm_different_formats_decode() {
        for fmt in &[
            SampleFormat::U8,
            SampleFormat::S16,
            SampleFormat::S32,
            SampleFormat::F32,
        ] {
            let bytes_per_sample = fmt.bytes();
            let channels = 2u16;
            let data_len = bytes_per_sample * channels as usize * 10; // 10 samples
            let packet = Packet {
                data: Bytes::from(vec![0u8; data_len]),
                pts: None,
                dts: None,
                duration: 0,
                stream_index: 0,
                flags: PacketFlags::empty(),
                pos: -1,
                time_base: Rational::new(1, 1000),
            };

            let mut decoder = PCMAudioDecoder::new(*fmt);
            let frames = decoder.decode(&packet).unwrap();
            assert_eq!(frames.len(), 1, "Failed for format {:?}", fmt);
            assert_eq!(frames[0].samples, 10, "Wrong sample count for {:?}", fmt);
            assert_eq!(frames[0].sample_format, *fmt);
        }
    }
}
