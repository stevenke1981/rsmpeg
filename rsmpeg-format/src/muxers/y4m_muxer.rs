use crate::demuxers::y4m_frame_bytes;
use crate::format::OutputFormat;
use crate::format_context::FormatContext;
use rsmpeg_codec::{CodecId, Packet};
use rsmpeg_util::{MediaType, PixelFormat, RsError, RsResult};

/// YUV4MPEG2 (`.y4m`) raw uncompressed video muxer.
#[derive(Debug, Default)]
pub struct Y4mMuxer {
    frame_bytes: usize,
    header_written: bool,
}

impl OutputFormat for Y4mMuxer {
    fn name(&self) -> &'static str {
        "y4m"
    }

    fn description(&self) -> &'static str {
        "YUV4MPEG2 raw video"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["y4m"]
    }

    fn write_header(&mut self, ctx: &mut FormatContext) -> RsResult<()> {
        if ctx.streams.len() != 1 {
            return Err(RsError::InvalidData(
                format!(
                    "Y4M muxer requires exactly 1 stream, got {}",
                    ctx.streams.len()
                )
                .into(),
            ));
        }

        let stream = &ctx.streams[0];
        if stream.media_type != MediaType::Video {
            return Err(RsError::InvalidData(
                "Y4M muxer requires a video stream".into(),
            ));
        }
        if !matches!(stream.codec_id, CodecId::RawVideo | CodecId::Unknown) {
            return Err(RsError::Unsupported(
                format!(
                    "Y4M muxer only supports raw video, got {:?}",
                    stream.codec_id
                )
                .into(),
            ));
        }

        let w = stream
            .codec_params
            .width
            .filter(|&w| w > 0)
            .ok_or_else(|| RsError::InvalidData("Y4M muxer: missing or zero width".into()))?;
        let h = stream
            .codec_params
            .height
            .filter(|&h| h > 0)
            .ok_or_else(|| RsError::InvalidData("Y4M muxer: missing or zero height".into()))?;
        let pix_fmt = stream
            .codec_params
            .pixel_format
            .ok_or_else(|| RsError::InvalidData("Y4M muxer: missing pixel_format".into()))?;

        let chroma = match pix_fmt {
            PixelFormat::Yuv420P => "420jpeg",
            PixelFormat::Yuv422P => "422",
            PixelFormat::Yuv444P => "444",
            PixelFormat::Gray8 => "mono",
            other => {
                return Err(RsError::Unsupported(
                    format!("Y4M muxer: unsupported pixel format {other:?}").into(),
                ))
            }
        };

        let frame_bytes = y4m_frame_bytes(w, h, pix_fmt).ok_or_else(|| {
            RsError::InvalidData("Y4M muxer: frame dimensions overflow usize".into())
        })?;

        let tb = stream.time_base;
        let (fps_num, fps_den) = if tb.num > 0 && tb.den > 0 {
            (tb.den, tb.num)
        } else {
            (30, 1)
        };

        let io = ctx
            .io
            .as_mut()
            .ok_or_else(|| RsError::InvalidData("No IO context".into()))?;
        let header = format!("YUV4MPEG2 W{w} H{h} F{fps_num}:{fps_den} Ip A1:1 C{chroma}\n");
        io.write_all(header.as_bytes())?;

        self.frame_bytes = frame_bytes;
        self.header_written = true;
        Ok(())
    }

    fn write_frame(&mut self, ctx: &mut FormatContext, packet: &Packet) -> RsResult<()> {
        if !self.header_written {
            return Err(RsError::InvalidData(
                "Y4M muxer: write_header must be called before write_frame".into(),
            ));
        }
        if packet.stream_index != 0 {
            return Err(RsError::InvalidData(
                format!(
                    "Y4M muxer: invalid stream_index {}, expected 0",
                    packet.stream_index
                )
                .into(),
            ));
        }
        if packet.data.len() != self.frame_bytes {
            return Err(RsError::InvalidData(
                format!(
                    "Y4M muxer: expected {} bytes per frame, got {}",
                    self.frame_bytes,
                    packet.data.len()
                )
                .into(),
            ));
        }

        let io = ctx
            .io
            .as_mut()
            .ok_or_else(|| RsError::InvalidData("No IO context".into()))?;
        io.write_all(b"FRAME\n")?;
        io.write_all(&packet.data)?;
        Ok(())
    }

    fn write_trailer(&mut self, ctx: &mut FormatContext) -> RsResult<()> {
        if !self.header_written {
            return Err(RsError::InvalidData(
                "Y4M muxer: write_header must be called before write_trailer".into(),
            ));
        }
        let io = ctx
            .io
            .as_mut()
            .ok_or_else(|| RsError::InvalidData("No IO context".into()))?;
        io.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demuxers::Y4mDemuxer;
    use crate::format::InputFormat;
    use crate::io_context::IOContext;
    use crate::stream::Stream;
    use rsmpeg_codec::CodecParameters;
    use rsmpeg_util::{Dict, Rational};

    fn video_ctx(
        width: usize,
        height: usize,
        pixel_format: PixelFormat,
        time_base: Rational,
    ) -> FormatContext {
        let mut stream = Stream::new(0, CodecId::RawVideo);
        stream.media_type = MediaType::Video;
        stream.time_base = time_base;
        stream.codec_params = CodecParameters {
            codec_id: CodecId::RawVideo,
            media_type: MediaType::Video,
            width: Some(width),
            height: Some(height),
            pixel_format: Some(pixel_format),
            sample_rate: None,
            channels: None,
            sample_format: None,
            bit_rate: None,
            extradata: None,
            h264_bitstream_format: Default::default(),
        };
        FormatContext {
            input: None,
            output: None,
            streams: vec![stream],
            io: Some(IOContext::from_output_buffer()),
            metadata: Dict::new(),
            duration: 0,
            bit_rate: 0,
            filename: None,
            format_name: Some("y4m".into()),
        }
    }

    #[test]
    fn roundtrip_all_y4m_formats_with_multi_frame_read_and_seek() {
        let formats = [
            (PixelFormat::Yuv420P, 6, 4, 36usize),
            (PixelFormat::Yuv422P, 6, 4, 48usize),
            (PixelFormat::Yuv444P, 6, 4, 72usize),
            (PixelFormat::Gray8, 6, 4, 24usize),
        ];

        for (pix_fmt, w, h, expected_len) in formats {
            let mut mux_ctx = video_ctx(w, h, pix_fmt, Rational::new(1, 24));
            let mut muxer = Y4mMuxer::default();
            muxer.write_header(&mut mux_ctx).unwrap();

            let frames: Vec<Vec<u8>> = (0..4u8)
                .map(|idx| {
                    (0..expected_len)
                        .map(|i| idx.wrapping_mul(31).wrapping_add(i as u8))
                        .collect()
                })
                .collect();

            for f in &frames {
                muxer
                    .write_frame(&mut mux_ctx, &Packet::new(bytes::Bytes::from(f.clone()), 0))
                    .unwrap();
            }
            muxer.write_trailer(&mut mux_ctx).unwrap();

            let y4m_bytes = mux_ctx.io.take().unwrap().into_buffer().unwrap();

            let mut demux_ctx = FormatContext {
                input: None,
                output: None,
                streams: Vec::new(),
                io: Some(IOContext::from_buffer(y4m_bytes)),
                metadata: Dict::new(),
                duration: 0,
                bit_rate: 0,
                filename: None,
                format_name: Some("y4m".into()),
            };
            let mut demuxer = Y4mDemuxer::default();
            demuxer.read_header(&mut demux_ctx).unwrap();

            let s = &demux_ctx.streams[0];
            assert_eq!(s.codec_params.width, Some(w));
            assert_eq!(s.codec_params.height, Some(h));
            assert_eq!(s.codec_params.pixel_format, Some(pix_fmt));
            assert_eq!(s.time_base, Rational::new(1, 24));
            assert_eq!(s.duration, 4);

            for (idx, expected) in frames.iter().enumerate() {
                let pkt = demuxer.read_frame(&mut demux_ctx).unwrap().unwrap();
                assert_eq!(pkt.pts, Some(idx as i64));
                assert_eq!(pkt.data.as_ref(), expected.as_slice());
            }
            assert!(demuxer.read_frame(&mut demux_ctx).unwrap().is_none());

            // Seek to frame 2 (~84 ms at 24 fps: 84 * 24 / 1000 = 2).
            demuxer.seek(&mut demux_ctx, 84).unwrap();
            let pkt2 = demuxer.read_frame(&mut demux_ctx).unwrap().unwrap();
            assert_eq!(pkt2.pts, Some(2));
            assert_eq!(pkt2.data.as_ref(), frames[2].as_slice());
        }
    }

    #[test]
    fn rejects_mismatched_frame_size_and_unsupported_pixel_format() {
        let mut bad_pix = video_ctx(4, 4, PixelFormat::Rgb24, Rational::new(1, 30));
        assert!(matches!(
            Y4mMuxer::default().write_header(&mut bad_pix),
            Err(RsError::Unsupported(_))
        ));

        let mut mux_ctx = video_ctx(4, 4, PixelFormat::Gray8, Rational::new(0, 1));
        let mut muxer = Y4mMuxer::default();
        muxer.write_header(&mut mux_ctx).unwrap();
        let err = muxer
            .write_frame(
                &mut mux_ctx,
                &Packet::new(bytes::Bytes::from(vec![0u8; 8]), 0),
            )
            .unwrap_err();
        assert!(matches!(err, RsError::InvalidData(_)));
        muxer.write_trailer(&mut mux_ctx).unwrap();

        let bytes = mux_ctx.io.take().unwrap().into_buffer().unwrap();
        assert!(bytes.starts_with(b"YUV4MPEG2 W4 H4 F30:1 "));
    }
}
