use crate::format::InputFormat;
use crate::format_context::FormatContext;
use crate::io_context::IOContext;
use crate::probe::ProbeScore;
use crate::stream::Stream;
use rsmpeg_codec::{CodecId, CodecParameters, Packet, PacketFlags};
use rsmpeg_util::{MediaType, PixelFormat, Rational, RsError, RsResult};
use std::io::SeekFrom;

/// Maximum stream header line length in a YUV4MPEG2 stream.
const MAX_Y4M_HEADER_BYTES: usize = 4096;

/// Maximum per-frame header line length in a YUV4MPEG2 stream.
const MAX_Y4M_FRAME_HEADER_BYTES: usize = 1024;

/// Upper bound on frames scanned sequentially during `read_header` before
/// falling back to stride-based estimation.
const MAX_Y4M_SCAN_FRAMES: u64 = 4096;

/// Compute the byte size of one YUV4MPEG2 video frame, returning `None` on
/// arithmetic overflow or unsupported pixel formats.
pub(crate) fn y4m_frame_bytes(width: usize, height: usize, pix_fmt: PixelFormat) -> Option<usize> {
    if width == 0 || height == 0 {
        return None;
    }
    let y = width.checked_mul(height)?;
    match pix_fmt {
        PixelFormat::Yuv420P => {
            let cw = width.div_ceil(2);
            let ch = height.div_ceil(2);
            let c = cw.checked_mul(ch)?.checked_mul(2)?;
            y.checked_add(c)
        }
        PixelFormat::Yuv422P => {
            let cw = width.div_ceil(2);
            let c = cw.checked_mul(height)?.checked_mul(2)?;
            y.checked_add(c)
        }
        PixelFormat::Yuv444P => y.checked_mul(3),
        PixelFormat::Gray8 => Some(y),
        _ => None,
    }
}

/// Raw video demuxer — no container, just raw pixel data.
/// Used primarily for testing the demuxer scaffolding.
pub struct RawVideoDemuxer;

impl InputFormat for RawVideoDemuxer {
    fn name(&self) -> &'static str {
        "rawvideo"
    }

    fn description(&self) -> &'static str {
        "Raw video (no container)"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["raw", "yuv"]
    }

    fn probe(&self, _buf: &[u8]) -> ProbeScore {
        ProbeScore::NoMatch
    }

    fn read_header(&mut self, ctx: &mut FormatContext) -> RsResult<()> {
        let mut stream = Stream::new(0, CodecId::Unknown);
        stream.media_type = MediaType::Video;
        ctx.streams.push(stream);
        tracing::info!("RawVideo: added placeholder video stream");
        Ok(())
    }

    fn read_frame(&mut self, _ctx: &mut FormatContext) -> RsResult<Option<Packet>> {
        Ok(None)
    }

    fn seek(&mut self, _ctx: &mut FormatContext, _ts: i64) -> RsResult<()> {
        Ok(())
    }
}

/// YUV4MPEG2 (`.y4m`) raw uncompressed video demuxer.
#[derive(Debug, Default)]
pub struct Y4mDemuxer {
    width: usize,
    height: usize,
    pixel_format: Option<PixelFormat>,
    fps_num: u64,
    fps_den: u64,
    time_base_num: i32,
    time_base_den: i32,
    frame_bytes: usize,
    data_start: u64,
    frame_idx: u64,
}

impl Y4mDemuxer {
    fn time_base(&self) -> Rational {
        if self.time_base_den > 0 {
            Rational::new(self.time_base_num, self.time_base_den)
        } else {
            Rational::new(1, 25)
        }
    }
}

impl InputFormat for Y4mDemuxer {
    fn name(&self) -> &'static str {
        "y4m"
    }

    fn description(&self) -> &'static str {
        "YUV4MPEG2 raw video"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["y4m"]
    }

    fn probe(&self, buf: &[u8]) -> ProbeScore {
        if buf.starts_with(b"YUV4MPEG2 ") {
            ProbeScore::Certain
        } else {
            ProbeScore::NoMatch
        }
    }

    fn read_header(&mut self, ctx: &mut FormatContext) -> RsResult<()> {
        let io = ctx
            .io
            .as_mut()
            .ok_or_else(|| RsError::InvalidData("No IO context".into()))?;

        io.seek(SeekFrom::Start(0))?;
        let line = read_line_bounded(io, MAX_Y4M_HEADER_BYTES)?
            .ok_or_else(|| RsError::InvalidData("Y4M: empty input".into()))?;

        if !line.starts_with(b"YUV4MPEG2 ") {
            return Err(RsError::InvalidData(
                "Y4M: missing YUV4MPEG2 stream header".into(),
            ));
        }

        let header_str = std::str::from_utf8(&line)
            .map_err(|_| RsError::InvalidData("Y4M: non-UTF8 stream header".into()))?;

        let mut width: Option<usize> = None;
        let mut height: Option<usize> = None;
        let mut fps: Option<(u64, u64)> = None;
        let mut pixel_format = PixelFormat::Yuv420P;

        for token in header_str["YUV4MPEG2 ".len()..].split_ascii_whitespace() {
            let Some((tag, rest)) = split_first_char(token) else {
                continue;
            };
            match tag {
                'W' => {
                    let w: usize = rest.parse().map_err(|_| {
                        RsError::InvalidData(format!("Y4M: invalid width '{token}'").into())
                    })?;
                    if w == 0 {
                        return Err(RsError::InvalidData("Y4M: zero width".into()));
                    }
                    width = Some(w);
                }
                'H' => {
                    let h: usize = rest.parse().map_err(|_| {
                        RsError::InvalidData(format!("Y4M: invalid height '{token}'").into())
                    })?;
                    if h == 0 {
                        return Err(RsError::InvalidData("Y4M: zero height".into()));
                    }
                    height = Some(h);
                }
                'F' => {
                    let (num_s, den_s) = rest.split_once(':').ok_or_else(|| {
                        RsError::InvalidData(format!("Y4M: malformed frame rate '{token}'").into())
                    })?;
                    let num: u64 = num_s.parse().map_err(|_| {
                        RsError::InvalidData(format!("Y4M: invalid fps numerator '{token}'").into())
                    })?;
                    let den: u64 = den_s.parse().map_err(|_| {
                        RsError::InvalidData(
                            format!("Y4M: invalid fps denominator '{token}'").into(),
                        )
                    })?;
                    if num == 0 || den == 0 {
                        return Err(RsError::InvalidData(
                            format!("Y4M: zero fps component '{token}'").into(),
                        ));
                    }
                    fps = Some((num, den));
                }
                'C' => {
                    pixel_format = match rest {
                        "420" | "420jpeg" | "420mpeg2" | "420paldv" => PixelFormat::Yuv420P,
                        "422" => PixelFormat::Yuv422P,
                        "444" => PixelFormat::Yuv444P,
                        "mono" => PixelFormat::Gray8,
                        other => {
                            return Err(RsError::Unsupported(
                                format!("Y4M: unsupported colorspace 'C{other}'").into(),
                            ))
                        }
                    };
                }
                'I' | 'A' | 'X' => {
                    // Interlacing, pixel aspect ratio, and application metadata tags.
                }
                _ => {
                    tracing::debug!("Y4M: ignoring unknown header tag '{}'", token);
                }
            }
        }

        let width = width.ok_or_else(|| RsError::InvalidData("Y4M: missing W tag".into()))?;
        let height = height.ok_or_else(|| RsError::InvalidData("Y4M: missing H tag".into()))?;
        let (raw_fps_num, raw_fps_den) = fps.unwrap_or((25, 1));
        let g = gcd_u64(raw_fps_num, raw_fps_den);
        let fps_num = raw_fps_num / g;
        let fps_den = raw_fps_den / g;
        let time_base = reduce_and_clamp_rational(fps_den, fps_num);

        let frame_bytes = y4m_frame_bytes(width, height, pixel_format)
            .ok_or_else(|| RsError::InvalidData("Y4M: frame dimensions overflow usize".into()))?;
        let frame_i64 = i64::try_from(frame_bytes)
            .map_err(|_| RsError::InvalidData("Y4M: frame size exceeds i64".into()))?;

        let data_start = io.tell()?;
        let file_len = io.seek(SeekFrom::End(0))?;
        io.seek(SeekFrom::Start(data_start))?;

        let total_frames = if file_len <= data_start {
            0
        } else {
            let mut scanned_frames = 0u64;
            let mut hit_eof = false;
            while scanned_frames < MAX_Y4M_SCAN_FRAMES {
                let line = match read_line_bounded(io, MAX_Y4M_FRAME_HEADER_BYTES) {
                    Ok(Some(l)) => l,
                    Ok(None) => {
                        hit_eof = true;
                        break;
                    }
                    Err(_) => break,
                };
                if !is_valid_frame_header(&line) {
                    break;
                }
                let payload_pos = io.tell()?;
                if payload_pos.saturating_add(frame_bytes as u64) > file_len {
                    break;
                }
                io.seek(SeekFrom::Current(frame_i64))?;
                scanned_frames += 1;
            }
            if hit_eof || scanned_frames < MAX_Y4M_SCAN_FRAMES {
                scanned_frames
            } else {
                let scanned_end = io.tell()?;
                let remaining = file_len.saturating_sub(scanned_end);
                let consumed = scanned_end.saturating_sub(data_start);
                let avg_stride = (consumed / scanned_frames).max(6 + frame_bytes as u64);
                scanned_frames.saturating_add(remaining / avg_stride)
            }
        };
        io.seek(SeekFrom::Start(data_start))?;

        let duration_ms = ((total_frames as u128 * 1000 * fps_den as u128) / fps_num as u128)
            .min(i64::MAX as u128) as i64;
        let bit_rate = ((frame_bytes as u128 * 8 * fps_num as u128) / fps_den as u128)
            .min(u64::MAX as u128) as u64;

        self.width = width;
        self.height = height;
        self.pixel_format = Some(pixel_format);
        self.fps_num = fps_num;
        self.fps_den = fps_den;
        self.time_base_num = time_base.num;
        self.time_base_den = time_base.den;
        self.frame_bytes = frame_bytes;
        self.data_start = data_start;
        self.frame_idx = 0;

        let fps_rational = Rational::new(time_base.den, time_base.num);
        let mut stream = Stream::new(0, CodecId::RawVideo);
        stream.media_type = MediaType::Video;
        stream.codec_params = CodecParameters {
            codec_id: CodecId::RawVideo,
            media_type: MediaType::Video,
            width: Some(width),
            height: Some(height),
            pixel_format: Some(pixel_format),
            sample_rate: None,
            channels: None,
            sample_format: None,
            bit_rate: Some(bit_rate),
            extradata: None,
            h264_bitstream_format: Default::default(),
        };
        stream.time_base = time_base;
        stream.avg_frame_rate = fps_rational;
        stream.r_frame_rate = fps_rational;
        stream.duration = total_frames.min(i64::MAX as u64) as i64;

        ctx.duration = duration_ms;
        ctx.bit_rate = bit_rate;
        ctx.streams.push(stream);

        tracing::info!(
            "Y4M: {}x{}, {:?}, {}/{} fps, {} frames ({}ms)",
            width,
            height,
            pixel_format,
            fps_num,
            fps_den,
            total_frames,
            duration_ms
        );
        Ok(())
    }

    fn read_frame(&mut self, ctx: &mut FormatContext) -> RsResult<Option<Packet>> {
        if self.frame_bytes == 0 {
            return Err(RsError::InvalidData(
                "Y4M: read_header must be called before read_frame".into(),
            ));
        }
        let io = ctx
            .io
            .as_mut()
            .ok_or_else(|| RsError::InvalidData("No IO context".into()))?;

        let Some(line) = read_line_bounded(io, MAX_Y4M_FRAME_HEADER_BYTES)? else {
            return Ok(None);
        };
        if !is_valid_frame_header(&line) {
            return Err(RsError::InvalidData("Y4M: expected FRAME header".into()));
        }

        let pos = i64::try_from(io.tell()?).unwrap_or(i64::MAX);
        let data = io.read_bytes(self.frame_bytes)?;
        let pts = i64::try_from(self.frame_idx).unwrap_or(i64::MAX);
        self.frame_idx = self.frame_idx.saturating_add(1);

        let mut pkt = Packet::new(bytes::Bytes::from(data), 0);
        pkt.pts = Some(pts);
        pkt.dts = Some(pts);
        pkt.duration = 1;
        pkt.time_base = self.time_base();
        pkt.flags = PacketFlags::KEY;
        pkt.pos = pos;
        Ok(Some(pkt))
    }

    fn seek(&mut self, ctx: &mut FormatContext, timestamp_ms: i64) -> RsResult<()> {
        if self.frame_bytes == 0 {
            return Err(RsError::InvalidData(
                "Y4M: read_header must be called before seek".into(),
            ));
        }
        let io = ctx
            .io
            .as_mut()
            .ok_or_else(|| RsError::InvalidData("No IO context".into()))?;

        let target_frame = if self.fps_num == 0 || self.fps_den == 0 {
            0
        } else {
            ((timestamp_ms.max(0) as u128 * self.fps_num as u128) / (1000 * self.fps_den as u128))
                .min(u64::MAX as u128) as u64
        };

        let payload_skip = i64::try_from(self.frame_bytes)
            .map_err(|_| RsError::InvalidData("Y4M: frame size exceeds i64".into()))?;

        io.seek(SeekFrom::Start(self.data_start))?;
        let mut landed = 0u64;
        while landed < target_frame {
            let Some(line) = read_line_bounded(io, MAX_Y4M_FRAME_HEADER_BYTES)? else {
                break;
            };
            if !is_valid_frame_header(&line) {
                return Err(RsError::InvalidData(
                    "Y4M: expected FRAME header while seeking".into(),
                ));
            }
            io.seek(SeekFrom::Current(payload_skip))?;
            landed += 1;
        }
        self.frame_idx = landed;
        Ok(())
    }
}

/// Read an ASCII header line up to `\n` (not included), returning `None` on
/// clean EOF at the first byte and `Err(InvalidData)` on mid-line EOF or
/// lines exceeding `max_len`.
fn read_line_bounded(io: &mut IOContext, max_len: usize) -> RsResult<Option<Vec<u8>>> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match io.read(&mut byte) {
            Ok(0) => {
                if line.is_empty() {
                    return Ok(None);
                }
                return Err(RsError::InvalidData(
                    "Y4M: truncated line before newline".into(),
                ));
            }
            Ok(_) => {
                if byte[0] == b'\n' {
                    if line.last() == Some(&b'\r') {
                        line.pop();
                    }
                    return Ok(Some(line));
                }
                if line.len() >= max_len {
                    return Err(RsError::InvalidData(
                        "Y4M: header line exceeds maximum length".into(),
                    ));
                }
                line.push(byte[0]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }
    }
}

fn is_valid_frame_header(line: &[u8]) -> bool {
    line == b"FRAME" || line.starts_with(b"FRAME ")
}

fn split_first_char(s: &str) -> Option<(char, &str)> {
    let mut chars = s.chars();
    let first = chars.next()?;
    Some((first, chars.as_str()))
}

fn gcd_u64(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a.max(1)
}

fn reduce_and_clamp_rational(num: u64, den: u64) -> Rational {
    let g = gcd_u64(num, den);
    let (mut n, mut d) = (num / g, den / g);
    while n > i32::MAX as u64 || d > i32::MAX as u64 {
        n = (n >> 1).max(1);
        d = (d >> 1).max(1);
    }
    Rational::new(n as i32, d as i32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsmpeg_util::Dict;

    fn ctx_with(data: Vec<u8>) -> FormatContext {
        FormatContext {
            input: None,
            output: None,
            streams: Vec::new(),
            io: Some(IOContext::from_buffer(data)),
            metadata: Dict::new(),
            duration: 0,
            bit_rate: 0,
            filename: None,
            format_name: Some("y4m".into()),
        }
    }

    fn build_y4m(header: &str, frames: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(header.as_bytes());
        if !header.ends_with('\n') {
            out.push(b'\n');
        }
        for (tag, payload) in frames {
            out.extend_from_slice(b"FRAME");
            if !tag.is_empty() {
                out.push(b' ');
                out.extend_from_slice(tag.as_bytes());
            }
            out.push(b'\n');
            out.extend_from_slice(payload);
        }
        out
    }

    #[test]
    fn probe_matches_y4m_header_only() {
        let d = Y4mDemuxer::default();
        assert_eq!(d.probe(b"YUV4MPEG2 W4 H4 F30:1\n"), ProbeScore::Certain);
        assert_eq!(d.probe(b"YUV4MPEG2"), ProbeScore::NoMatch);
        assert_eq!(d.probe(b"RIFF....WAVE"), ProbeScore::NoMatch);
    }

    #[test]
    fn read_header_parses_colorspaces_tags_and_duration() {
        let cases = [
            ("C420", PixelFormat::Yuv420P, 4 * 4 + 2 * (2 * 2)),
            ("C420jpeg", PixelFormat::Yuv420P, 24),
            ("C420mpeg2", PixelFormat::Yuv420P, 24),
            ("C420paldv", PixelFormat::Yuv420P, 24),
            ("C422", PixelFormat::Yuv422P, 4 * 4 + 2 * (2 * 4)),
            ("C444", PixelFormat::Yuv444P, 4 * 4 * 3),
            ("Cmono", PixelFormat::Gray8, 4 * 4),
        ];

        for (c_tag, expected_fmt, expected_bytes) in cases {
            let hdr = format!("YUV4MPEG2 W4 H4 F60:2 Ip A1:1 Xvendor=rsmpeg {c_tag}");
            let frames = vec![
                ("Xnote=first", vec![0x11u8; expected_bytes]),
                ("", vec![0x22u8; expected_bytes]),
                ("", vec![0x33u8; expected_bytes]),
            ];
            let data = build_y4m(&hdr, &frames);
            let mut ctx = ctx_with(data);
            let mut d = Y4mDemuxer::default();
            d.read_header(&mut ctx).unwrap();

            assert_eq!(ctx.streams.len(), 1);
            let s = &ctx.streams[0];
            assert_eq!(s.codec_id, CodecId::RawVideo);
            assert_eq!(s.media_type, MediaType::Video);
            assert_eq!(s.codec_params.width, Some(4));
            assert_eq!(s.codec_params.height, Some(4));
            assert_eq!(s.codec_params.pixel_format, Some(expected_fmt));
            // 60:2 reduces to 30:1 -> time_base = 1/30.
            assert_eq!(s.time_base, Rational::new(1, 30));
            assert_eq!(s.avg_frame_rate, Rational::new(30, 1));
            assert_eq!(s.duration, 3);
            assert_eq!(ctx.duration, 100); // 3 frames at 30 fps = 100 ms

            let mut read_pkts = Vec::new();
            while let Some(p) = d.read_frame(&mut ctx).unwrap() {
                read_pkts.push(p);
            }
            assert_eq!(read_pkts.len(), 3);
            for (idx, p) in read_pkts.iter().enumerate() {
                assert_eq!(p.pts, Some(idx as i64));
                assert_eq!(p.dts, Some(idx as i64));
                assert_eq!(p.duration, 1);
                assert_eq!(p.time_base, Rational::new(1, 30));
                assert!(p.is_key());
                assert_eq!(p.data.as_ref(), frames[idx].1.as_slice());
            }
        }
    }

    #[test]
    fn odd_dimensions_use_ceil_chroma_and_seek_handles_tagged_frames() {
        // 3x3 Yuv420P: Y = 9, U = 2*2 = 4, V = 2*2 = 4 => 17 bytes per frame.
        let hdr = "YUV4MPEG2 W3 H3 F10:1";
        let f0 = vec![0x00u8; 17];
        let f1 = vec![0x11u8; 17];
        let f2 = vec![0x22u8; 17];
        let f3 = vec![0x33u8; 17];
        let data = build_y4m(
            hdr,
            &[
                ("I? Xalpha=1", f0.clone()),
                ("Xbeta=2222", f1.clone()),
                ("", f2.clone()),
                ("Xgamma=3", f3.clone()),
            ],
        );

        let mut ctx = ctx_with(data);
        let mut d = Y4mDemuxer::default();
        d.read_header(&mut ctx).unwrap();
        assert_eq!(ctx.streams[0].duration, 4);
        assert_eq!(ctx.duration, 400);

        // At 10 fps, 200 ms -> frame 2.
        d.seek(&mut ctx, 200).unwrap();
        let p2 = d.read_frame(&mut ctx).unwrap().expect("frame 2");
        assert_eq!(p2.pts, Some(2));
        assert_eq!(p2.data.as_ref(), f2.as_slice());

        let p3 = d.read_frame(&mut ctx).unwrap().expect("frame 3");
        assert_eq!(p3.pts, Some(3));
        assert_eq!(p3.data.as_ref(), f3.as_slice());
        assert!(d.read_frame(&mut ctx).unwrap().is_none());

        // Seek back to negative timestamp clamps to 0.
        d.seek(&mut ctx, -50).unwrap();
        let p0 = d.read_frame(&mut ctx).unwrap().expect("frame 0");
        assert_eq!(p0.pts, Some(0));
        assert_eq!(p0.data.as_ref(), f0.as_slice());

        // Seek past EOF yields None on read_frame.
        d.seek(&mut ctx, 5000).unwrap();
        assert!(d.read_frame(&mut ctx).unwrap().is_none());
    }

    #[test]
    fn rejects_malformed_and_unsupported_y4m_headers() {
        let bad_cases = [
            b"".as_slice(),
            b"NOT_Y4M W4 H4\n",
            b"YUV4MPEG2 W4 H4", // missing newline
            b"YUV4MPEG2 W0 H4\n",
            b"YUV4MPEG2 W4\n",
            b"YUV4MPEG2 W4 H4 F0:1\n",
            b"YUV4MPEG2 W4 H4 F25:0\n",
            b"YUV4MPEG2 W4 H4 F25\n",
        ];
        for bytes in bad_cases {
            let mut ctx = ctx_with(bytes.to_vec());
            let mut d = Y4mDemuxer::default();
            assert!(
                matches!(d.read_header(&mut ctx), Err(RsError::InvalidData(_))),
                "expected InvalidData for {:?}",
                String::from_utf8_lossy(bytes)
            );
            assert!(ctx.streams.is_empty());
        }

        // Unsupported 10-bit colorspace.
        let mut ctx = ctx_with(b"YUV4MPEG2 W4 H4 C420p10\n".to_vec());
        let mut d = Y4mDemuxer::default();
        assert!(matches!(
            d.read_header(&mut ctx),
            Err(RsError::Unsupported(_))
        ));
    }

    #[test]
    fn omitted_fps_and_colorspace_use_defaults() {
        let data = build_y4m("YUV4MPEG2 W2 H2", &[("", vec![1, 2, 3, 4, 5, 6])]);
        let mut ctx = ctx_with(data);
        let mut d = Y4mDemuxer::default();
        d.read_header(&mut ctx).unwrap();
        let s = &ctx.streams[0];
        assert_eq!(s.codec_params.pixel_format, Some(PixelFormat::Yuv420P));
        assert_eq!(s.time_base, Rational::new(1, 25));
        assert_eq!(s.duration, 1);
    }
}
