//! Raw video codec — copies frames as-is without compression.

use std::collections::VecDeque;

use bytes::Bytes;

use crate::codec::{Codec, CodecCapabilities, DecodeStatus, Decoder, Encoder};
use crate::codec_id::CodecId;
use crate::codec_parameters::CodecParameters;
use crate::frame::Frame;
use crate::packet::{Packet, PacketFlags};
use rsmpeg_util::{MediaType, PixelFormat, RsError, RsResult};

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
        let width = self.params.as_ref().and_then(|p| p.width).unwrap_or(0);
        let height = self.params.as_ref().and_then(|p| p.height).unwrap_or(0);
        let pixel_format = self
            .params
            .as_ref()
            .and_then(|p| p.pixel_format)
            .unwrap_or(PixelFormat::None);
        // Without a usable layout the payload is passed through as one opaque
        // plane.
        let (data, linesize) = split_planes(&packet.data, width, height, pixel_format)
            .unwrap_or_else(|| (vec![packet.data.to_vec()], vec![packet.data.len()]));

        Frame {
            data,
            linesize,
            width,
            height,
            pixel_format,
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

/// Plane buffers and their strides.
type Planes = (Vec<Vec<u8>>, Vec<usize>);

/// Lay a raw video payload out as the planes of a `width × height` frame.
///
/// Returns `None` when the layout cannot be derived: zero dimensions, a format
/// without a known 8/16-bit layout, a size overflow, or a payload shorter than
/// one full frame. Bytes beyond a full planar or semi-planar frame are ignored.
fn split_planes(data: &[u8], width: usize, height: usize, format: PixelFormat) -> Option<Planes> {
    if width == 0 || height == 0 {
        return None;
    }
    match format {
        PixelFormat::Gray8 => split_packed(data, width, height, 1),
        PixelFormat::Gray16 => split_packed(data, width, height, 2),
        PixelFormat::Rgb24 | PixelFormat::Bgr24 => split_packed(data, width, height, 3),
        PixelFormat::Rgba | PixelFormat::Bgra | PixelFormat::Argb => {
            split_packed(data, width, height, 4)
        }
        PixelFormat::Yuv420P => {
            let chroma = (width.div_ceil(2), height.div_ceil(2));
            split_planar(data, width, height, chroma)
        }
        PixelFormat::Yuv422P => split_planar(data, width, height, (width.div_ceil(2), height)),
        PixelFormat::Yuv444P => split_planar(data, width, height, (width, height)),
        PixelFormat::Nv12 | PixelFormat::Nv21 => split_semi_planar(data, width, height),
        PixelFormat::Yuv420P10 | PixelFormat::Yuv420P12 | PixelFormat::None => None,
    }
}

/// Single interleaved plane. A payload that divides evenly into `height` rows
/// carries row padding (e.g. 4-byte aligned DIB rows), which becomes the stride.
fn split_packed(
    data: &[u8],
    width: usize,
    height: usize,
    bytes_per_pixel: usize,
) -> Option<Planes> {
    let min_row = width.checked_mul(bytes_per_pixel)?;
    if data.len() < min_row.checked_mul(height)? {
        return None;
    }
    let stride = if data.len() % height == 0 {
        data.len() / height
    } else {
        min_row
    };
    Some((vec![data.to_vec()], vec![stride]))
}

/// Tightly packed Y, U and V planes, stored one after the other.
fn split_planar(
    data: &[u8],
    width: usize,
    height: usize,
    (chroma_w, chroma_h): (usize, usize),
) -> Option<Planes> {
    let y_len = width.checked_mul(height)?;
    let c_len = chroma_w.checked_mul(chroma_h)?;
    let total = c_len.checked_mul(2)?.checked_add(y_len)?;
    let (y, chroma) = data.get(..total)?.split_at(y_len);
    let (u, v) = chroma.split_at(c_len);
    Some((
        vec![y.to_vec(), u.to_vec(), v.to_vec()],
        vec![width, chroma_w, chroma_w],
    ))
}

/// Tightly packed Y plane followed by an interleaved 4:2:0 chroma plane.
fn split_semi_planar(data: &[u8], width: usize, height: usize) -> Option<Planes> {
    let y_len = width.checked_mul(height)?;
    let uv_row = width.div_ceil(2).checked_mul(2)?;
    let uv_len = uv_row.checked_mul(height.div_ceil(2))?;
    let total = y_len.checked_add(uv_len)?;
    let (y, uv) = data.get(..total)?.split_at(y_len);
    Some((vec![y.to_vec(), uv.to_vec()], vec![width, uv_row]))
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

    /// Decode one `len`-byte packet whose bytes count up from zero.
    fn decode_layout(format: PixelFormat, width: usize, height: usize, len: usize) -> Frame {
        let mut params = make_params();
        params.width = Some(width);
        params.height = Some(height);
        params.pixel_format = Some(format);
        let mut decoder = RawVideoDecoder::new();
        decoder.set_parameters(params);

        let packet = Packet::new(Bytes::from(counting_bytes(len)), 0);
        let mut frames = decoder.decode(&packet).unwrap();
        assert_eq!(frames.len(), 1);
        let frame = frames.remove(0);
        assert_eq!((frame.width, frame.height), (width, height));
        assert_eq!(frame.pixel_format, format);
        frame
    }

    fn counting_bytes(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    /// Assert one frame's planes are consecutive slices of the payload.
    fn assert_planes(frame: &Frame, lengths: &[usize], linesize: &[usize]) {
        let payload = counting_bytes(lengths.iter().sum());
        assert_eq!(frame.linesize, linesize);
        assert_eq!(frame.data.len(), lengths.len());
        let mut offset = 0;
        for (plane, &len) in frame.data.iter().zip(lengths) {
            assert_eq!(plane.as_slice(), &payload[offset..offset + len]);
            offset += len;
        }
    }

    #[test]
    fn packed_formats_report_one_row_as_linesize() {
        for (format, bpp) in [
            (PixelFormat::Rgb24, 3),
            (PixelFormat::Bgr24, 3),
            (PixelFormat::Rgba, 4),
            (PixelFormat::Bgra, 4),
            (PixelFormat::Argb, 4),
            (PixelFormat::Gray8, 1),
            (PixelFormat::Gray16, 2),
        ] {
            let (w, h) = (5, 3);
            let frame = decode_layout(format, w, h, w * h * bpp);
            assert_planes(&frame, &[w * h * bpp], &[w * bpp]);
        }
    }

    #[test]
    fn packed_formats_derive_padded_strides_from_the_payload() {
        // 5-pixel BGR24 rows padded to 16 bytes, as in uncompressed AVI.
        let frame = decode_layout(PixelFormat::Bgr24, 5, 3, 16 * 3);
        assert_planes(&frame, &[48], &[16]);

        // Trailing bytes that do not divide into rows keep the minimal stride.
        let frame = decode_layout(PixelFormat::Rgb24, 5, 3, 15 * 3 + 1);
        assert_planes(&frame, &[46], &[15]);
    }

    #[test]
    fn planar_formats_split_into_three_planes() {
        // Even dimensions.
        let frame = decode_layout(PixelFormat::Yuv420P, 6, 4, 24 + 6 + 6);
        assert_planes(&frame, &[24, 6, 6], &[6, 3, 3]);
        let frame = decode_layout(PixelFormat::Yuv422P, 6, 4, 24 + 12 + 12);
        assert_planes(&frame, &[24, 12, 12], &[6, 3, 3]);
        let frame = decode_layout(PixelFormat::Yuv444P, 6, 4, 24 * 3);
        assert_planes(&frame, &[24, 24, 24], &[6, 6, 6]);

        // Odd dimensions round chroma up.
        let frame = decode_layout(PixelFormat::Yuv420P, 5, 3, 15 + 6 + 6);
        assert_planes(&frame, &[15, 6, 6], &[5, 3, 3]);
        let frame = decode_layout(PixelFormat::Yuv422P, 5, 3, 15 + 9 + 9);
        assert_planes(&frame, &[15, 9, 9], &[5, 3, 3]);
        let frame = decode_layout(PixelFormat::Yuv444P, 5, 3, 15 * 3);
        assert_planes(&frame, &[15, 15, 15], &[5, 5, 5]);
    }

    #[test]
    fn semi_planar_formats_split_into_two_planes() {
        for format in [PixelFormat::Nv12, PixelFormat::Nv21] {
            let frame = decode_layout(format, 6, 4, 24 + 12);
            assert_planes(&frame, &[24, 12], &[6, 6]);
            // Odd width: the interleaved row still holds whole UV pairs.
            let frame = decode_layout(format, 5, 3, 15 + 12);
            assert_planes(&frame, &[15, 12], &[5, 6]);
        }
    }

    #[test]
    fn planar_formats_ignore_trailing_bytes() {
        let frame = decode_layout(PixelFormat::Yuv420P, 4, 2, 8 + 2 + 2 + 7);
        assert_eq!(frame.linesize, vec![4, 2, 2]);
        let lengths: Vec<usize> = frame.data.iter().map(Vec::len).collect();
        assert_eq!(lengths, vec![8, 2, 2]);
    }

    #[test]
    fn short_payloads_stay_a_single_opaque_plane() {
        for (format, full) in [
            (PixelFormat::Rgb24, 48),
            (PixelFormat::Rgba, 64),
            (PixelFormat::Yuv420P, 24),
            (PixelFormat::Yuv422P, 32),
            (PixelFormat::Yuv444P, 48),
            (PixelFormat::Nv12, 24),
        ] {
            let frame = decode_layout(format, 4, 4, full - 1);
            assert_planes(&frame, &[full - 1], &[full - 1]);
        }
    }

    #[test]
    fn unknown_layouts_and_overflowing_sizes_stay_opaque() {
        for format in [
            PixelFormat::None,
            PixelFormat::Yuv420P10,
            PixelFormat::Yuv420P12,
        ] {
            let frame = decode_layout(format, 4, 4, 64);
            assert_planes(&frame, &[64], &[64]);
        }
        for format in [PixelFormat::Rgba, PixelFormat::Yuv444P, PixelFormat::Nv12] {
            let frame = decode_layout(format, usize::MAX, usize::MAX, 16);
            assert_planes(&frame, &[16], &[16]);
        }
    }
}
