//! OpenH264-backed H.264 decoder implementing [`rsmpeg_codec::Decoder`].
//!
//! Converts AVCC (length-prefixed) samples to Annex B via
//! [`crate::h264_bitstream`], then decodes to tightly packed YUV420P
//! [`Frame`]s. PTS is preserved best-effort with a FIFO of input packet
//! timestamps (no full B-frame reorder map yet).

use std::collections::VecDeque;

use openh264::formats::YUVSource;
use rsmpeg_codec::{CodecId, CodecParameters, DecodeStatus, Decoder, Frame, Packet, PictureType};
use rsmpeg_util::{PixelFormat, Rational, RsError, RsResult, SampleFormat};

use crate::h264_bitstream::{
    avcc_extradata_to_annex_b, avcc_nal_length_size, packet_for_decoder, H264BitstreamFormat,
};

/// OpenH264 decoder wrapper for the rsmpeg send/receive `Decoder` trait.
pub struct OpenH264Decoder {
    decoder: openh264::decoder::Decoder,
    params: CodecParameters,
    bitstream_format: H264BitstreamFormat,
    sps_pps_prefix: Option<Vec<u8>>,
    sps_pps_sent: bool,
    pending: VecDeque<Frame>,
    /// Best-effort PTS FIFO: one entry per successfully submitted packet.
    pts_queue: VecDeque<Option<i64>>,
    last_time_base: Rational,
    eof: bool,
}

impl OpenH264Decoder {
    /// Create a decoder with default parameters (H.264, Annex B, unknown size).
    pub fn new() -> RsResult<Self> {
        Self::from_params(&CodecParameters::new(CodecId::H264))
    }

    /// Construct from [`CodecParameters`].
    ///
    /// Uses:
    /// - `extradata` as optional avcC (SPS/PPS + NAL length size)
    /// - `width` / `height` when known (updated again on first decoded frame)
    /// - `h264_bitstream_format` when extradata is absent
    pub fn from_params(params: &CodecParameters) -> RsResult<Self> {
        if params.codec_id != CodecId::H264 && params.codec_id != CodecId::Unknown {
            return Err(RsError::Unsupported(
                format!(
                    "OpenH264Decoder only supports H.264, got {}",
                    params.codec_id.name()
                )
                .into(),
            ));
        }

        let mut params = params.clone();
        params.codec_id = CodecId::H264;
        params.media_type = CodecId::H264.media_type();
        if params.pixel_format.is_none() {
            params.pixel_format = Some(PixelFormat::Yuv420P);
        }

        let (sps_pps_prefix, bitstream_format) = resolve_bitstream(&params);

        Ok(Self {
            decoder: create_decoder()?,
            params,
            bitstream_format,
            sps_pps_prefix,
            sps_pps_sent: false,
            pending: VecDeque::new(),
            pts_queue: VecDeque::new(),
            last_time_base: Rational::new(1, 1000),
            eof: false,
        })
    }

    /// Construct from optional avcC extradata and known dimensions.
    pub fn with_extradata(
        extradata: Option<&[u8]>,
        width: Option<usize>,
        height: Option<usize>,
    ) -> RsResult<Self> {
        let mut params = CodecParameters::new(CodecId::H264);
        params.width = width;
        params.height = height;
        params.pixel_format = Some(PixelFormat::Yuv420P);
        if let Some(extra) = extradata {
            params.extradata = Some(extra.to_vec());
            if let Ok(n) = avcc_nal_length_size(extra) {
                params.h264_bitstream_format = rsmpeg_codec::H264BitstreamFormat::Avcc {
                    nal_length_size: n as u8,
                };
            }
        }
        Self::from_params(&params)
    }

    fn submit_annex_b(&mut self, annex_b: &[u8], pts: Option<i64>) -> RsResult<()> {
        self.pts_queue.push_back(pts);
        match self.decoder.decode(annex_b) {
            Ok(Some(yuv)) => {
                let out_pts = self.pts_queue.pop_front().unwrap_or(None);
                let frame = yuv_to_frame(&yuv, out_pts, self.last_time_base);
                self.update_params_from_frame(&frame);
                self.pending.push_back(frame);
                Ok(())
            }
            Ok(None) => Ok(()),
            Err(e) => {
                // Drop the PTS we just queued; failed decode produced no frame.
                let _ = self.pts_queue.pop_back();
                Err(RsError::Codec(
                    format!("OpenH264 decode failed: {e}").into(),
                ))
            }
        }
    }

    fn drain_flush(&mut self) {
        // Convert all flushed YUV buffers to owned Frames before touching `self`
        // again (DecodedYUV borrows the decoder).
        let time_base = self.last_time_base;
        let mut owned = Vec::new();
        match self.decoder.flush_remaining() {
            Ok(frames) => {
                for yuv in frames {
                    let pts = self.pts_queue.pop_front().unwrap_or(None);
                    owned.push(yuv_to_frame(&yuv, pts, time_base));
                }
            }
            Err(_) => {
                // Best-effort flush; ignore decoder flush errors at EOS.
            }
        }
        for frame in owned {
            self.update_params_from_frame(&frame);
            self.pending.push_back(frame);
        }
    }

    fn update_params_from_frame(&mut self, frame: &Frame) {
        if frame.width > 0 {
            self.params.width = Some(frame.width);
        }
        if frame.height > 0 {
            self.params.height = Some(frame.height);
        }
        self.params.pixel_format = Some(PixelFormat::Yuv420P);
    }

    fn recreate_decoder(&mut self) -> RsResult<()> {
        self.decoder = create_decoder()?;
        self.sps_pps_sent = false;
        self.pending.clear();
        self.pts_queue.clear();
        self.eof = false;
        Ok(())
    }
}

impl Decoder for OpenH264Decoder {
    fn codec_id(&self) -> CodecId {
        CodecId::H264
    }

    fn send_packet(&mut self, packet: Option<&Packet>) -> RsResult<()> {
        match packet {
            Some(pkt) => {
                if self.eof {
                    return Err(RsError::Codec(
                        "cannot send packet after end-of-stream; call reset() first".into(),
                    ));
                }
                self.last_time_base = pkt.time_base;
                let annex_b = packet_for_decoder(
                    &pkt.data,
                    self.bitstream_format,
                    self.sps_pps_prefix.as_deref(),
                    self.sps_pps_sent,
                )
                .map_err(|e| RsError::InvalidData(format!("H.264 bitstream: {e}").into()))?;

                if annex_b.is_empty() {
                    return Err(RsError::InvalidData(
                        "H.264 packet conversion produced empty Annex B buffer".into(),
                    ));
                }

                self.sps_pps_sent = true;
                let pts = pkt.pts.or(pkt.dts);
                self.submit_annex_b(&annex_b, pts)?;
                Ok(())
            }
            None => {
                self.eof = true;
                self.drain_flush();
                Ok(())
            }
        }
    }

    fn receive_frame(&mut self) -> RsResult<DecodeStatus> {
        if let Some(frame) = take_display_order(&mut self.pending) {
            return Ok(DecodeStatus::Frame(frame));
        }
        if self.eof {
            Ok(DecodeStatus::EndOfStream)
        } else {
            Ok(DecodeStatus::NeedMoreInput)
        }
    }

    fn reset(&mut self) -> RsResult<()> {
        self.recreate_decoder()
    }

    fn get_parameters(&self) -> CodecParameters {
        self.params.clone()
    }
}

/// Take the next frame in display (PTS) order from `pending`.
///
/// If every queued frame has a PTS, the smallest-PTS frame is returned
/// (correct B-frame display order). Otherwise falls back to FIFO.
fn take_display_order(pending: &mut VecDeque<Frame>) -> Option<Frame> {
    if pending.is_empty() {
        return None;
    }
    if pending.iter().all(|f| f.pts.is_some()) {
        let mut best = 0usize;
        for i in 1..pending.len() {
            if pending[i].pts < pending[best].pts {
                best = i;
            }
        }
        return pending.remove(best);
    }
    pending.pop_front()
}

fn create_decoder() -> RsResult<openh264::decoder::Decoder> {
    openh264::decoder::Decoder::with_api_config(
        openh264::OpenH264API::from_source(),
        openh264::decoder::DecoderConfig::new()
            .flush_after_decode(openh264::decoder::Flush::NoFlush),
    )
    .map_err(|e| RsError::Codec(format!("OpenH264 init failed: {e}").into()))
}

/// Resolve SPS/PPS prefix and container bitstream format from codec params.
fn resolve_bitstream(params: &CodecParameters) -> (Option<Vec<u8>>, H264BitstreamFormat) {
    if let Some(ref avcc) = params.extradata {
        match (avcc_nal_length_size(avcc), avcc_extradata_to_annex_b(avcc)) {
            (Ok(n), Ok(annex)) => {
                return (
                    Some(annex),
                    H264BitstreamFormat::Avcc { nal_length_size: n },
                );
            }
            (Ok(n), Err(_)) => {
                return (None, H264BitstreamFormat::Avcc { nal_length_size: n });
            }
            _ => {}
        }
    }

    match params.h264_bitstream_format {
        rsmpeg_codec::H264BitstreamFormat::Avcc { nal_length_size } => (
            None,
            H264BitstreamFormat::Avcc {
                nal_length_size: nal_length_size as usize,
            },
        ),
        rsmpeg_codec::H264BitstreamFormat::AnnexB => (None, H264BitstreamFormat::AnnexB),
        rsmpeg_codec::H264BitstreamFormat::Unknown => {
            // Prefer AVCC length 4 for MP4-like containers without clear signal.
            (None, H264BitstreamFormat::Avcc { nal_length_size: 4 })
        }
    }
}

/// Copy OpenH264 YUV (possibly padded strides) into a tightly packed YUV420P frame.
fn yuv_to_frame(yuv: &impl YUVSource, pts: Option<i64>, time_base: Rational) -> Frame {
    let (w, h) = yuv.dimensions();
    let (y_stride, u_stride, v_stride) = yuv.strides();
    let mut frame = Frame::new_video(w, h, PixelFormat::Yuv420P);
    frame.pts = pts;
    frame.time_base = time_base;
    frame.sample_format = SampleFormat::None;
    frame.pict_type = PictureType::None;
    frame.key_frame = false;

    if w == 0 || h == 0 {
        return frame;
    }

    let y_src = yuv.y();
    let u_src = yuv.u();
    let v_src = yuv.v();

    // Luma: copy `w` bytes per row from possibly padded stride.
    for row in 0..h {
        let src = row * y_stride;
        let dst = row * w;
        if src + w <= y_src.len() && dst + w <= frame.data[0].len() {
            frame.data[0][dst..dst + w].copy_from_slice(&y_src[src..src + w]);
        }
    }

    let cw = w / 2;
    let ch = h / 2;
    for row in 0..ch {
        let u_src_off = row * u_stride;
        let v_src_off = row * v_stride;
        let dst = row * cw;
        if u_src_off + cw <= u_src.len() && dst + cw <= frame.data[1].len() {
            frame.data[1][dst..dst + cw].copy_from_slice(&u_src[u_src_off..u_src_off + cw]);
        }
        if v_src_off + cw <= v_src.len() && dst + cw <= frame.data[2].len() {
            frame.data[2][dst..dst + cw].copy_from_slice(&v_src[v_src_off..v_src_off + cw]);
        }
    }

    frame
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn construct_reset_need_more_input() {
        let mut dec = OpenH264Decoder::new().expect("create decoder");
        assert_eq!(dec.codec_id(), CodecId::H264);
        assert!(matches!(
            dec.receive_frame().unwrap(),
            DecodeStatus::NeedMoreInput
        ));
        dec.reset().unwrap();
        assert!(matches!(
            dec.receive_frame().unwrap(),
            DecodeStatus::NeedMoreInput
        ));
        let p = dec.get_parameters();
        assert_eq!(p.codec_id, CodecId::H264);
    }

    #[test]
    fn from_params_dimensions() {
        let mut params = CodecParameters::new(CodecId::H264);
        params.width = Some(640);
        params.height = Some(360);
        params.h264_bitstream_format = rsmpeg_codec::H264BitstreamFormat::AnnexB;
        let dec = OpenH264Decoder::from_params(&params).unwrap();
        let got = dec.get_parameters();
        assert_eq!(got.width, Some(640));
        assert_eq!(got.height, Some(360));
        assert_eq!(got.pixel_format, Some(PixelFormat::Yuv420P));
    }

    #[test]
    fn with_extradata_avcc() {
        // Minimal-looking avcC used by h264_bitstream tests.
        let avcc: Vec<u8> = vec![
            0x01, 0x42, 0x00, 0x1e, 0xff, 0xe1, 0x00, 0x0a, 0x67, 0x42, 0x00, 0x1e, 0x8d, 0x00,
            0x00, 0x03, 0x00, 0x01, 0x01, 0x00, 0x04, 0x68, 0xce, 0x06, 0xe2,
        ];
        let dec = OpenH264Decoder::with_extradata(Some(&avcc), Some(320), Some(240)).unwrap();
        let p = dec.get_parameters();
        assert_eq!(p.width, Some(320));
        assert_eq!(p.height, Some(240));
        assert!(p.extradata.is_some());
    }

    #[test]
    fn flush_without_input_is_eos() {
        let mut dec = OpenH264Decoder::new().unwrap();
        dec.send_packet(None).unwrap();
        assert!(matches!(
            dec.receive_frame().unwrap(),
            DecodeStatus::EndOfStream
        ));
        dec.reset().unwrap();
        assert!(matches!(
            dec.receive_frame().unwrap(),
            DecodeStatus::NeedMoreInput
        ));
    }

    #[test]
    fn empty_annex_b_submit_need_more_or_ok() {
        let mut params = CodecParameters::new(CodecId::H264);
        params.h264_bitstream_format = rsmpeg_codec::H264BitstreamFormat::AnnexB;
        let mut dec = OpenH264Decoder::from_params(&params).unwrap();
        // AUD NAL (type 9) — typically produces no picture.
        let annex_b = [0u8, 0, 0, 1, 0x09, 0x10];
        match dec.submit_annex_b(&annex_b, Some(42)) {
            Ok(()) => {
                let status = dec.receive_frame().unwrap();
                assert!(matches!(
                    status,
                    DecodeStatus::NeedMoreInput | DecodeStatus::Frame(_)
                ));
            }
            Err(_) => {
                // Incomplete NAL is acceptable for this smoke test.
            }
        }
    }

    #[test]
    fn rejects_non_h264_codec_id() {
        let params = CodecParameters::new(CodecId::Hevc);
        assert!(OpenH264Decoder::from_params(&params).is_err());
    }

    #[test]
    fn yuv_plane_sizes_match_frame_new_video() {
        // Synthetic tight YUV420 source to validate plane packing.
        struct TightYuv {
            y: Vec<u8>,
            u: Vec<u8>,
            v: Vec<u8>,
            w: usize,
            h: usize,
        }
        impl YUVSource for TightYuv {
            fn dimensions(&self) -> (usize, usize) {
                (self.w, self.h)
            }
            fn strides(&self) -> (usize, usize, usize) {
                (self.w, self.w / 2, self.w / 2)
            }
            fn y(&self) -> &[u8] {
                &self.y
            }
            fn u(&self) -> &[u8] {
                &self.u
            }
            fn v(&self) -> &[u8] {
                &self.v
            }
        }

        let w = 16;
        let h = 8;
        let src = TightYuv {
            y: vec![16; w * h],
            u: vec![128; (w / 2) * (h / 2)],
            v: vec![128; (w / 2) * (h / 2)],
            w,
            h,
        };
        let frame = yuv_to_frame(&src, Some(7), Rational::new(1, 30));
        assert_eq!(frame.width, w);
        assert_eq!(frame.height, h);
        assert_eq!(frame.pixel_format, PixelFormat::Yuv420P);
        assert_eq!(frame.pts, Some(7));
        assert_eq!(frame.data.len(), 3);
        assert_eq!(frame.data[0].len(), w * h);
        assert_eq!(frame.data[1].len(), (w / 2) * (h / 2));
        assert_eq!(frame.data[2].len(), (w / 2) * (h / 2));
        assert_eq!(frame.linesize, vec![w, w / 2, w / 2]);
    }

    #[test]
    fn take_display_order_returns_smallest_pts_first() {
        use rsmpeg_codec::Frame;

        let mut pending: VecDeque<Frame> = VecDeque::new();
        let mut f3 = Frame::new_video(2, 2, PixelFormat::Yuv420P);
        f3.pts = Some(3);
        let mut f1 = Frame::new_video(2, 2, PixelFormat::Yuv420P);
        f1.pts = Some(1);
        let mut f2 = Frame::new_video(2, 2, PixelFormat::Yuv420P);
        f2.pts = Some(2);
        // Inserted in PTS order [3, 1, 2].
        pending.push_back(f3);
        pending.push_back(f1);
        pending.push_back(f2);

        assert_eq!(take_display_order(&mut pending).unwrap().pts, Some(1));
        assert_eq!(take_display_order(&mut pending).unwrap().pts, Some(2));
        assert_eq!(take_display_order(&mut pending).unwrap().pts, Some(3));
        assert!(take_display_order(&mut pending).is_none());
    }

    #[test]
    fn take_display_order_falls_back_to_fifo_when_pts_missing() {
        use rsmpeg_codec::Frame;

        let mut pending: VecDeque<Frame> = VecDeque::new();
        let mut a = Frame::new_video(2, 2, PixelFormat::Yuv420P);
        a.pts = Some(10);
        let mut b = Frame::new_video(2, 2, PixelFormat::Yuv420P);
        b.pts = None;
        let mut c = Frame::new_video(2, 2, PixelFormat::Yuv420P);
        c.pts = Some(1);
        // Mixed timestamps → FIFO insertion order [10, None, 1] must be preserved.
        pending.push_back(a);
        pending.push_back(b);
        pending.push_back(c);

        assert_eq!(take_display_order(&mut pending).unwrap().pts, Some(10));
        assert_eq!(take_display_order(&mut pending).unwrap().pts, None);
        assert_eq!(take_display_order(&mut pending).unwrap().pts, Some(1));
        assert!(take_display_order(&mut pending).is_none());
    }
}
