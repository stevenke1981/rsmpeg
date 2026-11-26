use super::avi_demuxer::parse_wave_format_ex;
use super::pcm::{wave_pcm_layout, PcmLayout};
use crate::format::InputFormat;
use crate::format_context::FormatContext;
use crate::probe::ProbeScore;
use crate::stream::Stream;
use rsmpeg_codec::{CodecId, CodecParameters, Packet};
use rsmpeg_util::{MediaType, RsError, RsResult};
use std::io::SeekFrom;

/// WAV (Waveform Audio) demuxer.
///
/// Parses the RIFF/WAVE header, scans the chunk list for `fmt ` (skipping
/// `JUNK`, `bext`, `LIST`, `fact`, ... with RIFF even-byte padding) and then
/// `data`, and streams canonical little-endian PCM packets from `read_frame`.
///
/// Supported encodings: integer PCM 8/16/24/32-bit (24-bit is widened to
/// `S32`, value `<< 8`), IEEE float 32/64-bit, G.711 A-law / µ-law (expanded
/// to `S16`), plain or wrapped in `WAVE_FORMAT_EXTENSIBLE`.
pub struct WAVDemuxer {
    data_start: u64,
    data_total: u64,
    data_remaining: u64,
    sample_rate: u32,
    channels: u16,
    /// Bytes per sample frame **on disk** (all channels).
    block_align: u16,
    /// Bytes per packet (~20 ms of audio), on disk.
    packet_bytes: usize,
    /// Conversion from on-disk samples to canonical packet bytes.
    layout: PcmLayout,
    /// Index of the next sample frame to be emitted (packet pts/dts).
    current_frame: u64,
}

impl InputFormat for WAVDemuxer {
    fn name(&self) -> &'static str {
        "wav"
    }

    fn description(&self) -> &'static str {
        "WAV (Waveform Audio)"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["wav"]
    }

    fn probe(&self, buf: &[u8]) -> ProbeScore {
        if buf.len() >= 12 && &buf[0..4] == b"RIFF" && &buf[8..12] == b"WAVE" {
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

        let file_len = io.seek(SeekFrom::End(0))?;
        io.seek(SeekFrom::Start(0))?;
        let mut riff = [0u8; 12];
        io.read_exact(&mut riff)?;
        if &riff[0..4] != b"RIFF" || &riff[8..12] != b"WAVE" {
            return Err(RsError::InvalidData("WAV: missing RIFF/WAVE header".into()));
        }

        // --- Scan chunks (from offset 12) for `fmt ` ---
        // `data` normally follows `fmt `, but a `data` chunk seen first is
        // remembered rather than mis-parsed.
        let mut early_data: Option<(u64, u64)> = None;
        let fmt_body = loop {
            let Some((id, size)) = next_chunk(io)? else {
                return Err(RsError::InvalidData("WAV: missing fmt chunk".into()));
            };
            let body_start = io.tell()?;
            if &id == b"fmt " {
                if size < 16 {
                    return Err(RsError::InvalidData(
                        format!("WAV: fmt chunk too small ({size} bytes)").into(),
                    ));
                }
                // Read the fields we understand (WAVEFORMATEXTENSIBLE is 40
                // bytes); a hostile size must not trigger a huge allocation.
                let body = io.read_bytes((size as usize).min(MAX_FMT_BYTES))?;
                io.seek(SeekFrom::Start(body_start + padded(size)))?;
                break body;
            }
            if &id == b"data" && early_data.is_none() {
                early_data = Some((body_start, u64::from(size)));
            }
            io.seek(SeekFrom::Start(body_start + padded(size)))?;
        };

        let wf = parse_wave_format_ex(&fmt_body)
            .ok_or_else(|| RsError::InvalidData("WAV: malformed fmt chunk".into()))?;
        let channels = wf.channels;
        let sample_rate = wf.sample_rate;
        let bits_per_sample = wf.bits_per_sample;
        if channels == 0 {
            return Err(RsError::InvalidData("WAV: zero channels".into()));
        }

        let (sample_format, layout) =
            wave_pcm_layout(wf.format_tag, bits_per_sample).ok_or_else(|| {
                RsError::Unsupported(
                    format!(
                        "WAV: unsupported encoding (format tag 0x{:04X}, {} bits)",
                        wf.format_tag, bits_per_sample
                    )
                    .into(),
                )
            })?;
        // All supported encodings decode through the PCM passthrough codec.
        let codec_id = CodecId::Pcm;

        // --- Find `data` chunk ---
        let (data_start, declared) = match early_data {
            Some(d) => d,
            None => {
                let size = u64::from(find_data_chunk(io)?);
                (io.tell()?, size)
            }
        };
        // A streamed / truncated file may claim more than it holds.
        let data_size = declared.min(file_len.saturating_sub(data_start));

        // Bytes per sample frame as stored on disk.
        let src_sample_bytes = match layout {
            PcmLayout::Passthrough => sample_format.bytes(),
            other => other.src_bytes(),
        } as u64;
        let frame_size = src_sample_bytes * u64::from(channels);
        let block_align = u16::try_from(frame_size).map_err(|_| {
            RsError::InvalidData(
                format!("WAV: {channels} channels × {src_sample_bytes} bytes is too wide").into(),
            )
        })?;

        // Exact frame (sample-per-channel) count from the `data` chunk. This is
        // the authoritative length: the stream's time base is 1/sample_rate, so
        // `duration` must be this number of ticks. Deriving ticks from a
        // millisecond figure first truncated anything shorter than 1 ms to zero
        // (41 samples at 48 kHz → 0 ms → 0 ticks → "0.000 s").
        let total_frames = data_size.checked_div(frame_size).unwrap_or(0);

        let duration_ms = if sample_rate > 0 && frame_size > 0 {
            // `data_size * 1000` can overflow u64 for multi-exabyte claims; use a
            // widening multiply and clamp into the i64 the API exposes.
            let ms = (data_size as u128 * 1000) / (sample_rate as u128 * frame_size as u128);
            ms.min(i64::MAX as u128) as i64
        } else {
            0
        };

        let bit_rate = sample_rate as u64 * channels as u64 * bits_per_sample as u64;

        // The stream time base is 1/sample_rate, which must be representable as
        // an `i32` pair. Falling back to 1/1 (the previous behaviour) did not
        // just lose precision — it re-interpreted every timestamp in seconds,
        // turning 4 samples of a 2^31 Hz file into "4 seconds". Reject the file
        // instead of emitting metadata we cannot honour.
        let time_base_den = time_base_den(sample_rate).ok_or_else(|| {
            RsError::InvalidData(
                format!(
                    "WAV: sample rate {sample_rate} Hz cannot be represented as a time base \
                     (must be 1..={})",
                    i32::MAX
                )
                .into(),
            )
        })?;

        // ~20 ms of audio per packet
        let samples_per_packet = (sample_rate as usize / 50).max(1);
        // `samples_per_packet * block_align` is derived from file fields; use
        // checked math so a hostile `sample_rate`/`block_align` pair cannot
        // overflow `usize` (which wraps in release builds and then aborts on a
        // huge allocation). Saturating is not enough on its own: a bogus pair
        // would ask for a `usize::MAX`-byte buffer, so cap the packet size at
        // a sane bound and let `read_frame` clamp to the remaining bytes.
        let packet_bytes = samples_per_packet
            .checked_mul(block_align as usize)
            .ok_or_else(|| {
                RsError::InvalidData(
                    format!(
                        "WAV: packet size overflow ({} samples/packet × {} bytes/block)",
                        samples_per_packet, block_align
                    )
                    .into(),
                )
            })?
            .max(block_align as usize)
            .min(MAX_PACKET_BYTES);

        self.data_start = data_start;
        self.data_total = data_size;
        self.data_remaining = data_size;
        self.sample_rate = sample_rate;
        self.channels = channels;
        self.block_align = block_align.max(1);
        self.packet_bytes = packet_bytes;
        self.layout = layout;
        self.current_frame = 0;
        io.seek(SeekFrom::Start(data_start))?;

        // --- Build stream ---
        let mut stream = Stream::new(0, codec_id);
        stream.media_type = MediaType::Audio;
        stream.codec_params = CodecParameters {
            codec_id,
            media_type: MediaType::Audio,
            width: None,
            height: None,
            pixel_format: None,
            sample_format: Some(sample_format),
            sample_rate: Some(sample_rate),
            channels: Some(channels),
            bit_rate: Some(bit_rate),
            extradata: None,
            h264_bitstream_format: Default::default(),
        };
        stream.time_base = rsmpeg_util::Rational::new(1, time_base_den);
        // `Stream::duration` is in `time_base` ticks, not milliseconds: storing
        // ms here made a 80 ms file report 0.01 s (4 ticks × 1/44100). The tick
        // count comes straight from the sample count (1 tick = 1 frame) so
        // sub-millisecond files keep their real length instead of rounding to 0.
        stream.duration = total_frames.min(i64::MAX as u64) as i64;
        ctx.duration = duration_ms;
        ctx.bit_rate = bit_rate;
        ctx.streams.push(stream);

        tracing::info!(
            "WAV: {}ch, {}Hz, {}bit (tag 0x{:04X} → {:?}), {}ms",
            channels,
            sample_rate,
            bits_per_sample,
            wf.format_tag,
            sample_format,
            duration_ms
        );
        Ok(())
    }

    fn read_frame(&mut self, ctx: &mut FormatContext) -> RsResult<Option<Packet>> {
        let align = u64::from(self.block_align.max(1));
        // A trailing partial sample frame cannot be decoded; treat it as EOF.
        if self.data_remaining < align {
            self.data_remaining = 0;
            return Ok(None);
        }
        let io = ctx
            .io
            .as_mut()
            .ok_or_else(|| RsError::InvalidData("No IO context".into()))?;

        // Whole blocks only, but always at least one.
        let to_read = (self.packet_bytes as u64).min(self.data_remaining);
        let to_read = (to_read / align * align).max(align) as usize;

        let pos = self.data_start + (self.data_total - self.data_remaining);
        let mut buf = vec![0u8; to_read];
        match io.read_exact(&mut buf) {
            Ok(()) => {}
            Err(_) => {
                self.data_remaining = 0;
                return Ok(None);
            }
        }
        self.data_remaining = self.data_remaining.saturating_sub(to_read as u64);

        let frames = to_read as u64 / align;
        let pts = i64::try_from(self.current_frame).unwrap_or(i64::MAX);
        self.current_frame = self.current_frame.saturating_add(frames);

        let mut packet = Packet::new(bytes::Bytes::from(self.layout.apply(buf)), 0);
        packet.pts = Some(pts);
        packet.dts = Some(pts);
        packet.pos = i64::try_from(pos).unwrap_or(i64::MAX);
        packet.duration = frames as i64;
        // `read_header` refused to open files whose rate cannot be represented,
        // so this cannot silently degrade to 1/1.
        packet.time_base = match time_base_den(self.sample_rate) {
            Some(den) => rsmpeg_util::Rational::new(1, den),
            None => {
                return Err(RsError::InvalidData(
                    "WAV: sample rate cannot be represented as a time base".into(),
                ))
            }
        };
        packet.flags = rsmpeg_codec::PacketFlags::KEY;
        Ok(Some(packet))
    }

    fn seek(&mut self, ctx: &mut FormatContext, timestamp_ms: i64) -> RsResult<()> {
        let io = ctx
            .io
            .as_mut()
            .ok_or_else(|| RsError::InvalidData("No IO context".into()))?;
        let target_ms = timestamp_ms.max(0) as u128;
        let align = u128::from(self.block_align.max(1));
        let total_frames = u128::from(self.data_total) / align;
        // Frame index on disk; every frame is `block_align` bytes, so the byte
        // offset is always sample-aligned in the original encoding.
        let frame_offset = ((target_ms * u128::from(self.sample_rate)) / 1000).min(total_frames);
        let byte_offset = (frame_offset * align) as u64;

        io.seek(SeekFrom::Start(self.data_start.saturating_add(byte_offset)))?;
        self.data_remaining = self.data_total.saturating_sub(byte_offset);
        self.current_frame = frame_offset as u64;
        Ok(())
    }
}

impl Default for WAVDemuxer {
    fn default() -> Self {
        Self {
            data_start: 0,
            data_total: 0,
            data_remaining: 0,
            sample_rate: 0,
            channels: 0,
            block_align: 1,
            packet_bytes: 4096,
            layout: PcmLayout::Passthrough,
            current_frame: 0,
        }
    }
}

/// Upper bound for one PCM packet (~64 KiB), so a hostile header cannot make
/// the demuxer allocate an enormous buffer.
const MAX_PACKET_BYTES: usize = 64 * 1024;

/// Largest `fmt ` body we read (WAVEFORMATEXTENSIBLE is 40 bytes; anything
/// beyond a few KiB is skipped, not allocated).
const MAX_FMT_BYTES: usize = 4096;

/// RIFF chunk bodies are padded to an even number of bytes.
fn padded(size: u32) -> u64 {
    u64::from(size) + u64::from(size & 1)
}

/// Represent `sample_rate` as the `i32` denominator of a [`Rational`] time base.
///
/// The raw `u32` comes straight from the container header: `as i32` would turn
/// e.g. `4_000_000_000` into a negative denominator, and `0` into a division by
/// zero on every later timestamp conversion. Neither is representable, so this
/// returns `None` and the caller reports an error rather than guessing `1/1`.
fn time_base_den(sample_rate: u32) -> Option<i32> {
    match i32::try_from(sample_rate) {
        Ok(v) if v > 0 => Some(v),
        _ => None,
    }
}

/// Read the next RIFF chunk header (`None` at end of input).
fn next_chunk(io: &mut crate::io_context::IOContext) -> RsResult<Option<([u8; 4], u32)>> {
    let mut hdr = [0u8; 8];
    let mut filled = 0;
    while filled < hdr.len() {
        match io.read(&mut hdr[filled..]) {
            Ok(0) => return Ok(None),
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }
    }
    let id = [hdr[0], hdr[1], hdr[2], hdr[3]];
    let size = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]);
    Ok(Some((id, size)))
}

/// Scan through RIFF chunks to find the `data` chunk and return its size.
/// On success the I/O position is the first byte of the `data` body.
fn find_data_chunk(io: &mut crate::io_context::IOContext) -> RsResult<u32> {
    loop {
        let Some((id, size)) = next_chunk(io)? else {
            return Ok(0); // EOF without finding data chunk
        };
        if &id == b"data" {
            return Ok(size);
        }
        // Skip chunk data (pad to even byte boundary per RIFF spec)
        let skip = padded(size);
        if skip > 0 && io.seek(SeekFrom::Current(skip as i64)).is_err() {
            return Ok(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format_context::FormatContext;
    use crate::io_context::IOContext;
    use rsmpeg_util::SampleFormat;

    /// Build a canonical 16-bit stereo WAV with `nb_samples` silent frames.
    fn build_wav(nb_samples: u32, sample_rate: u32, channels: u16, bits: u16) -> Vec<u8> {
        let block_align = channels * bits / 8;
        let byte_rate = sample_rate * u32::from(block_align);
        let data_len = nb_samples * u32::from(block_align);

        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&(36 + data_len).to_le_bytes());
        v.extend_from_slice(b"WAVE");
        v.extend_from_slice(b"fmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes()); // PCM
        v.extend_from_slice(&channels.to_le_bytes());
        v.extend_from_slice(&sample_rate.to_le_bytes());
        v.extend_from_slice(&byte_rate.to_le_bytes());
        v.extend_from_slice(&block_align.to_le_bytes());
        v.extend_from_slice(&bits.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&data_len.to_le_bytes());
        v.resize(v.len() + data_len as usize, 0u8);
        v
    }

    fn ctx_with(data: Vec<u8>) -> FormatContext {
        FormatContext {
            input: None,
            output: None,
            streams: Vec::new(),
            io: Some(IOContext::from_buffer(data)),
            metadata: rsmpeg_util::Dict::new(),
            duration: 0,
            bit_rate: 0,
            filename: None,
            format_name: Some("wav".into()),
        }
    }

    #[test]
    fn probe_accepts_riff_wave_only() {
        let wav = build_wav(0, 8000, 1, 8);
        assert_eq!(WAVDemuxer::default().probe(&wav), ProbeScore::Certain);
        // RIFF but not WAVE.
        let mut avi = wav.clone();
        avi[8..12].copy_from_slice(b"AVI ");
        assert_eq!(WAVDemuxer::default().probe(&avi), ProbeScore::NoMatch);
        assert_eq!(
            WAVDemuxer::default().probe(b"RIFF"),
            ProbeScore::NoMatch,
            "short buffers must not panic"
        );
    }

    #[test]
    fn read_header_parses_pcm16_stereo() {
        let data = build_wav(48_000, 48_000, 2, 16);
        let mut ctx = ctx_with(data);
        let mut demuxer = WAVDemuxer::default();
        demuxer.read_header(&mut ctx).unwrap();

        assert_eq!(ctx.streams.len(), 1);
        let s = &ctx.streams[0];
        assert_eq!(s.media_type, MediaType::Audio);
        assert_eq!(s.codec_id, CodecId::Pcm);
        assert_eq!(s.codec_params.sample_rate, Some(48_000));
        assert_eq!(s.codec_params.channels, Some(2));
        assert_eq!(s.codec_params.sample_format, Some(SampleFormat::S16));
        // 48000 stereo 16-bit samples = exactly 1 second.
        assert_eq!(ctx.duration, 1_000);
        // bit_rate = rate * channels * bits
        assert_eq!(ctx.bit_rate, 48_000 * 2 * 16);
    }

    #[test]
    fn short_wav_reports_the_right_duration_in_seconds() {
        // 80 ms of 8 kHz mono u8 → 640 samples. The stream used to store the
        // millisecond figure in a 1/sample_rate time base, so `probe_basic`
        // printed 0.01 s (640 ticks × 1/8000 would have been 0.08 s, but the
        // stored value was the 80 ms number itself).
        let nb = 640u32;
        let data = build_wav(nb, 8_000, 1, 8);
        let mut ctx = ctx_with(data);
        let mut demuxer = WAVDemuxer::default();
        demuxer.read_header(&mut ctx).unwrap();

        let stream = &ctx.streams[0];
        assert_eq!(ctx.duration, 80, "container duration is milliseconds");
        assert_eq!(
            stream.time_base,
            rsmpeg_util::Rational::new(1, 8_000),
            "time base is 1/sample_rate"
        );
        assert_eq!(
            stream.duration, 640,
            "stream duration must be in ticks (samples), not milliseconds"
        );
        let seconds = stream.duration_seconds();
        assert!(
            (seconds - 0.08).abs() < 1e-9,
            "80 ms must report 0.08 s, got {seconds}"
        );
    }

    #[test]
    fn one_second_wav_reports_one_second() {
        let data = build_wav(44_100, 44_100, 2, 16);
        let mut ctx = ctx_with(data);
        let mut demuxer = WAVDemuxer::default();
        demuxer.read_header(&mut ctx).unwrap();
        let stream = &ctx.streams[0];
        assert_eq!(stream.duration, 44_100, "44 100 ticks at 1/44100");
        assert!((stream.duration_seconds() - 1.0).abs() < 1e-9);
        assert_eq!(ctx.duration, 1_000);
    }

    #[test]
    fn review_sub_millisecond_wav_keeps_its_sample_count_as_ticks() {
        // Fifth review P2, the reviewer's `fractional_ms.wav`: 41 samples at
        // 48 kHz mono 16-bit is ~0.854 ms. Deriving the tick count from a
        // millisecond figure rounded it to `0 ms → 0 ticks → 0.000 s`, so a
        // legal file claimed to have no duration at all. The stream time base is
        // 1/48000, so the tick count must be the real sample count.
        let nb = 41u32;
        let data = build_wav(nb, 48_000, 1, 16);
        let mut ctx = ctx_with(data);
        let mut demuxer = WAVDemuxer::default();
        demuxer.read_header(&mut ctx).unwrap();

        let stream = &ctx.streams[0];
        assert_eq!(
            stream.duration, 41,
            "stream ticks must be the real sample count, not a quantised ms value"
        );
        let seconds = stream.duration_seconds();
        assert!(
            (seconds - 41.0 / 48_000.0).abs() < 1e-12,
            "0.854 ms must be reported exactly, got {seconds}"
        );
        // The container-level field stays in whole milliseconds by definition.
        assert_eq!(ctx.duration, 0, "container duration is milliseconds");
    }

    #[test]
    fn review_wav_ticks_are_not_derived_from_rounded_milliseconds() {
        // Sweep lengths around the 1 ms boundary: a tick count derived from a
        // rounded millisecond value collapses to 0 for every length below one
        // millisecond, and drifts above it. Each of these must be exact.
        for rate in [8_000u32, 44_100, 48_000, 96_000] {
            for frames in [1u32, 2, 41, 43, 44, 47, 48, 95, 96, 479] {
                let data = build_wav(frames, rate, 1, 16);
                let mut ctx = ctx_with(data);
                let mut demuxer = WAVDemuxer::default();
                demuxer
                    .read_header(&mut ctx)
                    .unwrap_or_else(|e| panic!("{frames} frames @ {rate} Hz: {e}"));
                let stream = &ctx.streams[0];
                assert_eq!(
                    stream.duration,
                    i64::from(frames),
                    "{frames} frames @ {rate} Hz must be {frames} ticks"
                );
                let expected = f64::from(frames) / f64::from(rate);
                assert!(
                    (stream.duration_seconds() - expected).abs() < 1e-12,
                    "{frames} frames @ {rate} Hz: {} != {expected}",
                    stream.duration_seconds()
                );
            }
        }
    }

    #[test]
    fn read_frame_streams_whole_data_chunk_then_eof() {
        let nb = 4_800u32; // 100 ms at 48 kHz
        let data = build_wav(nb, 48_000, 2, 16);
        let mut ctx = ctx_with(data);
        let mut demuxer = WAVDemuxer::default();
        demuxer.read_header(&mut ctx).unwrap();

        let mut total_samples = 0i64;
        let mut packets = 0;
        while let Some(p) = demuxer.read_frame(&mut ctx).unwrap() {
            assert!(!p.data.is_empty());
            // Every packet is whole blocks, so it is always a whole sample count.
            assert_eq!(p.data.len() % 4, 0, "packet not block-aligned");
            assert!(p.is_key());
            total_samples += p.duration;
            packets += 1;
            assert!(packets < 1_000, "read_frame must terminate");
        }
        assert_eq!(total_samples, i64::from(nb));
        // Subsequent reads stay at EOF instead of looping.
        assert!(demuxer.read_frame(&mut ctx).unwrap().is_none());
    }

    #[test]
    fn review_24_bit_stereo_ticks_match_frames_and_packet() {
        // Sixth review neighbour of the sub-millisecond fix, mirroring the
        // reviewer's `stereo_24bit_sub_ms.wav`: 44.1 kHz, 24-bit stereo, 37
        // frames (data chunk 222 bytes, block align 6) is 0.839 ms. The stream
        // and its packet must both carry 37 ticks (1 tick = 1 frame), and the
        // container keeps its integer-millisecond field at 0.
        let frames = 37u32;
        let data = build_wav(frames, 44_100, 2, 24);
        let mut ctx = ctx_with(data);
        let mut demuxer = WAVDemuxer::default();
        demuxer.read_header(&mut ctx).unwrap();

        {
            let stream = &ctx.streams[0];
            assert_eq!(
                stream.time_base,
                rsmpeg_util::Rational::new(1, 44_100),
                "time base is 1/sample_rate"
            );
            assert_eq!(stream.duration, 37, "stream ticks must be the frame count");
            assert!(
                (stream.duration_seconds() - 37.0 / 44_100.0).abs() < 1e-12,
                "got {}",
                stream.duration_seconds()
            );
            // A 24-bit WAV is exposed as S32 samples (widened in every packet).
            assert_eq!(stream.codec_params.sample_format, Some(SampleFormat::S32));
            assert_eq!(stream.codec_params.channels, Some(2));
            assert_eq!(stream.codec_params.sample_rate, Some(44_100));
        }
        assert_eq!(ctx.duration, 0, "container duration is whole milliseconds");

        let packet = demuxer.read_frame(&mut ctx).unwrap().expect("one packet");
        assert_eq!(packet.duration, 37, "packet duration is in stream ticks");
        assert_eq!(
            packet.data.len(),
            frames as usize * 8,
            "24-bit stereo blocks are 6 bytes on disk, widened to 2 × 4-byte S32"
        );
    }

    #[test]
    fn read_frame_on_empty_data_chunk_returns_none() {
        let data = build_wav(0, 44_100, 2, 16);
        let mut ctx = ctx_with(data);
        let mut demuxer = WAVDemuxer::default();
        demuxer.read_header(&mut ctx).unwrap();
        assert!(demuxer.read_frame(&mut ctx).unwrap().is_none());
    }

    #[test]
    fn read_header_rejects_non_wav_bytes() {
        let mut ctx = ctx_with(vec![0u8; 64]);
        let mut demuxer = WAVDemuxer::default();
        // Garbage must surface as an error (or an empty stream list), not a panic.
        let _ = demuxer.read_header(&mut ctx);
        assert!(ctx.streams.is_empty() || ctx.streams[0].codec_id == CodecId::Pcm);
    }

    #[test]
    fn read_header_survives_truncated_fmt_chunk() {
        // `fmt ` claims 16 bytes but the file ends right after chunk_size; the
        // short read must be an error, not an out-of-bounds index.
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&100u32.to_le_bytes());
        v.extend_from_slice(b"WAVE");
        v.extend_from_slice(b"fmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        let mut ctx = ctx_with(v);
        let mut demuxer = WAVDemuxer::default();
        assert!(demuxer.read_header(&mut ctx).is_err());
    }

    #[test]
    fn find_data_chunk_skips_other_chunks() {
        // LIST chunk (odd size → needs the RIFF pad byte) before `data`.
        let mut v = Vec::new();
        v.extend_from_slice(b"LIST");
        v.extend_from_slice(&3u32.to_le_bytes());
        v.extend_from_slice(&[1, 2, 3, 0]); // 3 bytes + 1 pad
        v.extend_from_slice(b"data");
        v.extend_from_slice(&8u32.to_le_bytes());
        v.extend_from_slice(&[0u8; 8]);

        let mut io = IOContext::from_buffer(v);
        assert_eq!(find_data_chunk(&mut io).unwrap(), 8);
    }

    #[test]
    fn find_data_chunk_returns_zero_at_eof() {
        let mut io = IOContext::from_buffer(Vec::new());
        assert_eq!(find_data_chunk(&mut io).unwrap(), 0);
    }

    #[test]
    fn time_base_den_reports_unrepresentable_rates() {
        // Valid rates round-trip unchanged.
        assert_eq!(time_base_den(48_000), Some(48_000));
        assert_eq!(time_base_den(1), Some(1));
        // A rate above i32::MAX used to become a negative denominator via
        // `as i32`, then a silent 1/1 fallback (4 samples reading as 4
        // seconds). It must now be reported as unrepresentable.
        assert_eq!(time_base_den(u32::MAX), None);
        assert_eq!(time_base_den(0x8000_0000), None);
        // Zero must not produce a division-by-zero time base.
        assert_eq!(time_base_den(0), None);
    }

    /// Build a minimal WAV whose `fmt ` claims `sample_rate`.
    fn wav_with_rate(sample_rate: u32) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&0u32.to_le_bytes());
        v.extend_from_slice(b"WAVE");
        v.extend_from_slice(b"fmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes()); // PCM
        v.extend_from_slice(&1u16.to_le_bytes()); // mono
        v.extend_from_slice(&sample_rate.to_le_bytes());
        v.extend_from_slice(&0u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes()); // block align
        v.extend_from_slice(&8u16.to_le_bytes()); // bits
        v.extend_from_slice(b"data");
        v.extend_from_slice(&4u32.to_le_bytes());
        v.extend_from_slice(&[0x80, 0x80, 0x80, 0x80]);
        v
    }

    #[test]
    fn unrepresentable_sample_rate_is_an_error_not_a_1_over_1_time_base() {
        // Review finding: a 2^31 Hz file opened fine and reported 1/1, so four
        // samples looked like four seconds. It must be rejected outright.
        let mut ctx = ctx_with(wav_with_rate(0x8000_0000));
        let mut demuxer = WAVDemuxer::default();
        let err = demuxer
            .read_header(&mut ctx)
            .expect_err("an unrepresentable sample rate must fail");
        assert!(matches!(err, RsError::InvalidData(_)), "got {err:?}");
        assert!(
            ctx.streams.is_empty(),
            "no stream may be published for a rejected file"
        );

        // Sample rate 0 is equally unrepresentable.
        let mut ctx = ctx_with(wav_with_rate(0));
        let mut demuxer = WAVDemuxer::default();
        assert!(demuxer.read_header(&mut ctx).is_err());
    }

    #[test]
    fn representable_rate_keeps_an_exact_time_base() {
        // The boundary value i32::MAX is still representable and must be kept.
        let mut ctx = ctx_with(wav_with_rate(i32::MAX as u32));
        let mut demuxer = WAVDemuxer::default();
        demuxer.read_header(&mut ctx).unwrap();
        assert_eq!(
            ctx.streams[0].time_base,
            rsmpeg_util::Rational::new(1, i32::MAX)
        );
        let packet = demuxer
            .read_frame(&mut ctx)
            .unwrap()
            .expect("one packet of audio");
        assert_eq!(packet.time_base, rsmpeg_util::Rational::new(1, i32::MAX));
        assert_eq!(
            packet.duration, 4,
            "4 mono u8 samples = 4 ticks, not seconds"
        );
    }

    #[test]
    fn packet_size_is_bounded_and_never_overflows() {
        // 4e9 samples/packet (rate/50) × 65535 bytes/block would overflow a
        // 32-bit usize and is nonsense on 64-bit; the demuxer must clamp.
        let mut v = wav_with_rate(4_000_000_000u32.min(i32::MAX as u32));
        // Overwrite block_align with 65535 (offset: 12 + 8 + 12 = 32).
        v[32..34].copy_from_slice(&65535u16.to_le_bytes());
        let mut ctx = ctx_with(v);
        let mut demuxer = WAVDemuxer::default();
        // Either a clean rejection or a bounded packet size is acceptable —
        // what must never happen is a usize overflow feeding a huge alloc.
        if demuxer.read_header(&mut ctx).is_ok() {
            assert!(
                demuxer.packet_bytes <= MAX_PACKET_BYTES,
                "packet_bytes must stay bounded, got {}",
                demuxer.packet_bytes
            );
            let packet = demuxer.read_frame(&mut ctx).unwrap().expect("packet");
            assert!(
                packet.data.len() <= MAX_PACKET_BYTES,
                "a single packet must not exceed the bound, got {}",
                packet.data.len()
            );
        }
    }

    #[test]
    fn hostile_sample_rate_and_block_align_do_not_overflow_packet_bytes() {
        // sample_rate = 4e9 is above i32::MAX, so the file must be rejected
        // outright now that the 1/1 fallback is gone (it used to "succeed"
        // with a bogus time base). The point of the test is that nothing
        // panics and no nonsense stream escapes.
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&0u32.to_le_bytes());
        v.extend_from_slice(b"WAVE");
        v.extend_from_slice(b"fmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes()); // PCM
        v.extend_from_slice(&2u16.to_le_bytes()); // 2 channels
        v.extend_from_slice(&4_000_000_000u32.to_le_bytes()); // absurd rate
        v.extend_from_slice(&0u32.to_le_bytes());
        v.extend_from_slice(&65535u16.to_le_bytes()); // absurd block align
        v.extend_from_slice(&16u16.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&0u32.to_le_bytes());

        let mut ctx = ctx_with(v);
        let mut demuxer = WAVDemuxer::default();
        let err = demuxer
            .read_header(&mut ctx)
            .expect_err("4 GHz cannot be represented as a time base");
        assert!(matches!(err, RsError::InvalidData(_)), "got {err:?}");
        assert!(
            ctx.streams.is_empty(),
            "no stream may escape for a rejected file"
        );
    }

    #[test]
    fn wav_seek_positions_to_requested_millisecond_and_clamps() {
        // 1 second of 1000 Hz mono 16-bit PCM = 2000 bytes (2 bytes/ms).
        let mut wav = build_wav(1000, 1000, 1, 16);
        let data_offset = wav.len() - 2000;
        for i in 0..1000u16 {
            let bytes = i.to_le_bytes();
            wav[data_offset + (i as usize) * 2] = bytes[0];
            wav[data_offset + (i as usize) * 2 + 1] = bytes[1];
        }

        let mut ctx = ctx_with(wav);
        let mut demuxer = WAVDemuxer::default();
        demuxer.read_header(&mut ctx).unwrap();

        // Seek to 500 ms -> sample index 500.
        demuxer.seek(&mut ctx, 500).unwrap();
        let pkt = demuxer
            .read_frame(&mut ctx)
            .unwrap()
            .expect("packet at 500ms");
        let first_sample = u16::from_le_bytes([pkt.data[0], pkt.data[1]]);
        assert_eq!(first_sample, 500);

        // Seek back to 0 ms (and negative timestamps clamp to 0).
        demuxer.seek(&mut ctx, -100).unwrap();
        let pkt0 = demuxer
            .read_frame(&mut ctx)
            .unwrap()
            .expect("packet at 0ms");
        let sample0 = u16::from_le_bytes([pkt0.data[0], pkt0.data[1]]);
        assert_eq!(sample0, 0);

        // Seek past end yields EOF without error.
        demuxer.seek(&mut ctx, 5000).unwrap();
        assert!(demuxer.read_frame(&mut ctx).unwrap().is_none());
    }

    /// Build a WAV with arbitrary pre-`fmt` chunks, `fmt ` body and data.
    fn build_wav_custom(
        pre: &[(&[u8; 4], Vec<u8>)],
        fmt: &[u8],
        mid: &[(&[u8; 4], Vec<u8>)],
        data: &[u8],
    ) -> Vec<u8> {
        fn push_chunk(v: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) {
            v.extend_from_slice(id);
            v.extend_from_slice(&(body.len() as u32).to_le_bytes());
            v.extend_from_slice(body);
            if body.len() % 2 == 1 {
                v.push(0);
            }
        }
        let mut body = Vec::new();
        body.extend_from_slice(b"WAVE");
        for (id, b) in pre {
            push_chunk(&mut body, id, b);
        }
        push_chunk(&mut body, b"fmt ", fmt);
        for (id, b) in mid {
            push_chunk(&mut body, id, b);
        }
        push_chunk(&mut body, b"data", data);
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&(body.len() as u32).to_le_bytes());
        v.extend_from_slice(&body);
        v
    }

    /// 16-byte WAVEFORMAT (optionally with a cbSize = 0 trailer).
    fn fmt_body(tag: u16, channels: u16, rate: u32, bits: u16) -> Vec<u8> {
        let bytes_per_sample = u32::from(bits).div_ceil(8);
        let align = bytes_per_sample as u16 * channels;
        let mut v = Vec::new();
        v.extend_from_slice(&tag.to_le_bytes());
        v.extend_from_slice(&channels.to_le_bytes());
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&(rate * u32::from(align)).to_le_bytes());
        v.extend_from_slice(&align.to_le_bytes());
        v.extend_from_slice(&bits.to_le_bytes());
        v
    }

    /// 40-byte WAVEFORMATEXTENSIBLE with the given SubFormat tag.
    fn fmt_extensible(sub_tag: u16, channels: u16, rate: u32, bits: u16) -> Vec<u8> {
        let mut v = fmt_body(0xFFFE, channels, rate, bits);
        v.extend_from_slice(&22u16.to_le_bytes()); // cbSize
        v.extend_from_slice(&bits.to_le_bytes()); // wValidBitsPerSample
        v.extend_from_slice(&0u32.to_le_bytes()); // dwChannelMask
        v.extend_from_slice(&sub_tag.to_le_bytes()); // SubFormat GUID data1 (low)
        v.extend_from_slice(&[
            0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
        ]);
        v
    }

    fn read_packets(ctx: &mut FormatContext, d: &mut WAVDemuxer) -> Vec<Packet> {
        let mut out = Vec::new();
        while let Some(p) = d.read_frame(ctx).unwrap() {
            out.push(p);
            assert!(out.len() < 10_000, "read_frame must terminate");
        }
        out
    }

    #[test]
    fn fmt_chunk_after_junk_bext_and_list_is_found() {
        let data: Vec<u8> = (0..32u16).flat_map(|s| s.to_le_bytes()).collect();
        let wav = build_wav_custom(
            &[
                (b"JUNK", vec![0u8; 28]),
                (b"bext", vec![7u8; 13]), // odd size → padded
                (b"LIST", b"INFOISFT\x05\0\0\0rsmp\0".to_vec()),
            ],
            &fmt_body(1, 2, 8_000, 16),
            &[
                (b"fact", 16u32.to_le_bytes().to_vec()),
                (b"PEAK", vec![1; 3]),
            ],
            &data,
        );
        let mut ctx = ctx_with(wav);
        let mut d = WAVDemuxer::default();
        d.read_header(&mut ctx).unwrap();
        let s = &ctx.streams[0];
        assert_eq!(s.codec_params.channels, Some(2));
        assert_eq!(s.codec_params.sample_rate, Some(8_000));
        assert_eq!(s.codec_params.sample_format, Some(SampleFormat::S16));
        assert_eq!(s.duration, 16, "64 bytes / 4-byte frames");
        let pkts = read_packets(&mut ctx, &mut d);
        let all: Vec<u8> = pkts.iter().flat_map(|p| p.data.to_vec()).collect();
        assert_eq!(
            all, data,
            "payload must be the data chunk, not a skipped chunk"
        );
    }

    #[test]
    fn data_chunk_before_fmt_is_still_used() {
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&0u32.to_le_bytes());
        v.extend_from_slice(b"WAVE");
        v.extend_from_slice(b"data");
        v.extend_from_slice(&4u32.to_le_bytes());
        v.extend_from_slice(&[1, 2, 3, 4]);
        v.extend_from_slice(b"fmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&fmt_body(1, 1, 8_000, 8));
        let mut ctx = ctx_with(v);
        let mut d = WAVDemuxer::default();
        d.read_header(&mut ctx).unwrap();
        let pkts = read_packets(&mut ctx, &mut d);
        assert_eq!(pkts.len(), 1);
        assert_eq!(pkts[0].data.as_ref(), &[1, 2, 3, 4]);
    }

    #[test]
    fn missing_fmt_unsupported_tags_and_zero_channels_are_errors() {
        // RIFF/WAVE with only a data chunk.
        let mut only_data = Vec::new();
        only_data.extend_from_slice(b"RIFF\0\0\0\0WAVEdata\x02\0\0\0\0\0");
        let mut d = WAVDemuxer::default();
        assert!(matches!(
            d.read_header(&mut ctx_with(only_data)),
            Err(RsError::InvalidData(_))
        ));

        // ADPCM (0x0002) is not PCM.
        let adpcm = build_wav_custom(&[], &fmt_body(2, 1, 8_000, 4), &[], &[0; 4]);
        assert!(matches!(
            WAVDemuxer::default().read_header(&mut ctx_with(adpcm)),
            Err(RsError::Unsupported(_))
        ));
        // 16-bit "float" is not a thing.
        let f16 = build_wav_custom(&[], &fmt_body(3, 1, 8_000, 16), &[], &[0; 4]);
        assert!(WAVDemuxer::default()
            .read_header(&mut ctx_with(f16))
            .is_err());

        let zero_ch = build_wav_custom(&[], &fmt_body(1, 0, 8_000, 16), &[], &[0; 4]);
        let mut ctx = ctx_with(zero_ch);
        assert!(matches!(
            WAVDemuxer::default().read_header(&mut ctx),
            Err(RsError::InvalidData(_))
        ));
        assert!(ctx.streams.is_empty());

        // fmt shorter than the 16-byte core.
        let short = build_wav_custom(&[], &[1, 0, 1, 0, 0x40, 0x1F], &[], &[0; 4]);
        assert!(WAVDemuxer::default()
            .read_header(&mut ctx_with(short))
            .is_err());
    }

    #[test]
    fn packets_carry_pts_dts_pos_and_seek_updates_pts() {
        // 1000 Hz mono 16-bit, 1 s. Packets are 20 ms = 20 frames.
        let data: Vec<u8> = (0..1000u16).flat_map(|s| s.to_le_bytes()).collect();
        let wav = build_wav(1000, 1000, 1, 16);
        let header_len = wav.len() - data.len();
        let mut wav = wav;
        wav[header_len..].copy_from_slice(&data);

        let mut ctx = ctx_with(wav);
        let mut d = WAVDemuxer::default();
        d.read_header(&mut ctx).unwrap();
        let pkts = read_packets(&mut ctx, &mut d);
        let mut expected_pts = 0i64;
        for p in &pkts {
            assert_eq!(p.pts, Some(expected_pts));
            assert_eq!(p.dts, Some(expected_pts));
            assert_eq!(p.pos, (header_len as i64) + expected_pts * 2);
            let first = i64::from(u16::from_le_bytes([p.data[0], p.data[1]]));
            assert_eq!(first, expected_pts, "pts must index the first sample");
            expected_pts += p.duration;
        }
        assert_eq!(expected_pts, 1000);

        d.seek(&mut ctx, 250).unwrap();
        let p = d.read_frame(&mut ctx).unwrap().unwrap();
        assert_eq!(p.pts, Some(250));
        assert_eq!(p.pos, header_len as i64 + 500);
        let next = d.read_frame(&mut ctx).unwrap().unwrap();
        assert_eq!(next.pts, Some(250 + p.duration));

        // Seeking past the end clamps pts to the frame count.
        d.seek(&mut ctx, 99_999).unwrap();
        assert!(d.read_frame(&mut ctx).unwrap().is_none());
        d.seek(&mut ctx, 0).unwrap();
        assert_eq!(d.read_frame(&mut ctx).unwrap().unwrap().pts, Some(0));
    }

    #[test]
    fn pcm24_is_widened_to_s32_left_shifted() {
        // Stereo 24-bit: L = 1, -1, 0x7FFFFF ; R = -0x800000, 0x123456, 0
        let samples: [i32; 6] = [1, -0x80_0000, -1, 0x12_3456, 0x7F_FFFF, 0];
        let data: Vec<u8> = samples
            .iter()
            .flat_map(|&s| {
                let b = s.to_le_bytes();
                [b[0], b[1], b[2]]
            })
            .collect();
        let wav = build_wav_custom(&[], &fmt_body(1, 2, 48_000, 24), &[], &data);
        let mut ctx = ctx_with(wav);
        let mut d = WAVDemuxer::default();
        d.read_header(&mut ctx).unwrap();
        assert_eq!(
            ctx.streams[0].codec_params.sample_format,
            Some(SampleFormat::S32)
        );
        assert_eq!(ctx.streams[0].duration, 3);
        let p = d.read_frame(&mut ctx).unwrap().unwrap();
        assert_eq!(p.duration, 3);
        assert_eq!(
            p.data.len(),
            6 * 4,
            "every sample is 4 bytes after widening"
        );
        let got: Vec<i32> = p
            .data
            .chunks_exact(4)
            .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let expected: Vec<i32> = samples.iter().map(|&s| s << 8).collect();
        assert_eq!(got, expected);

        // The PCM decoder now counts samples correctly.
        let mut dec =
            rsmpeg_codec::PCMAudioDecoder::from_params(&ctx.streams[0].codec_params).unwrap();
        use rsmpeg_codec::Decoder;
        let frames = dec.decode(&p).unwrap();
        assert_eq!(frames[0].samples, 3);

        // Seeking stays aligned to the 6-byte on-disk frames.
        d.seek(&mut ctx, 0).unwrap();
        let again = d.read_frame(&mut ctx).unwrap().unwrap();
        assert_eq!(again.data, p.data);
    }

    #[test]
    fn extensible_pcm24_and_float_are_resolved() {
        let data: Vec<u8> = [5i32, -5]
            .iter()
            .flat_map(|&s| {
                let b = s.to_le_bytes();
                [b[0], b[1], b[2]]
            })
            .collect();
        let wav = build_wav_custom(&[], &fmt_extensible(1, 1, 44_100, 24), &[], &data);
        let mut ctx = ctx_with(wav);
        let mut d = WAVDemuxer::default();
        d.read_header(&mut ctx).unwrap();
        assert_eq!(
            ctx.streams[0].codec_params.sample_format,
            Some(SampleFormat::S32)
        );
        let p = d.read_frame(&mut ctx).unwrap().unwrap();
        assert_eq!(&p.data[..4], &(5i32 << 8).to_le_bytes());
        assert_eq!(&p.data[4..], &(-5i32 << 8).to_le_bytes());

        // IEEE float 32-bit, both plain and EXTENSIBLE.
        let f32_data: Vec<u8> = [0.5f32, -0.25]
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        for fmt in [fmt_body(3, 2, 48_000, 32), fmt_extensible(3, 2, 48_000, 32)] {
            let wav = build_wav_custom(&[], &fmt, &[], &f32_data);
            let mut ctx = ctx_with(wav);
            let mut d = WAVDemuxer::default();
            d.read_header(&mut ctx).unwrap();
            let s = &ctx.streams[0];
            assert_eq!(s.codec_id, CodecId::Pcm);
            assert_eq!(s.codec_params.sample_format, Some(SampleFormat::F32));
            assert_eq!(s.duration, 1);
            let p = d.read_frame(&mut ctx).unwrap().unwrap();
            assert_eq!(p.data.as_ref(), f32_data.as_slice());
        }

        // IEEE float 64-bit.
        let f64_data: Vec<u8> = [1.0f64, -1.0, 0.125]
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        let wav = build_wav_custom(&[], &fmt_body(3, 1, 96_000, 64), &[], &f64_data);
        let mut ctx = ctx_with(wav);
        let mut d = WAVDemuxer::default();
        d.read_header(&mut ctx).unwrap();
        assert_eq!(
            ctx.streams[0].codec_params.sample_format,
            Some(SampleFormat::F64)
        );
        assert_eq!(ctx.streams[0].duration, 3);
        let p = d.read_frame(&mut ctx).unwrap().unwrap();
        assert_eq!(p.duration, 3);
        assert_eq!(p.data.as_ref(), f64_data.as_slice());
    }

    #[test]
    fn g711_alaw_and_mulaw_expand_to_s16() {
        use super::super::pcm::{alaw_to_linear, mulaw_to_linear};
        let codes: Vec<u8> = (0..=255u8).collect();
        for (tag, expand) in [
            (6u16, alaw_to_linear as fn(u8) -> i16),
            (7u16, mulaw_to_linear as fn(u8) -> i16),
        ] {
            // Stereo 8 kHz: 128 frames of 2 codes.
            let wav = build_wav_custom(&[], &fmt_body(tag, 2, 8_000, 8), &[], &codes);
            let mut ctx = ctx_with(wav);
            let mut d = WAVDemuxer::default();
            d.read_header(&mut ctx).unwrap();
            let s = &ctx.streams[0];
            assert_eq!(s.codec_id, CodecId::Pcm);
            assert_eq!(s.codec_params.sample_format, Some(SampleFormat::S16));
            assert_eq!(s.duration, 128, "frame count comes from 2-byte disk frames");
            assert_eq!(ctx.duration, 16, "128 frames at 8 kHz = 16 ms");

            let pkts = read_packets(&mut ctx, &mut d);
            let total: i64 = pkts.iter().map(|p| p.duration).sum();
            assert_eq!(total, 128);
            let pcm: Vec<i16> = pkts
                .iter()
                .flat_map(|p| p.data.to_vec())
                .collect::<Vec<u8>>()
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                .collect();
            let expected: Vec<i16> = codes.iter().map(|&c| expand(c)).collect();
            assert_eq!(pcm, expected, "tag {tag:#06x}");

            // 1 ms at 8 kHz = 8 frames = 16 bytes on disk (not 32 decoded).
            d.seek(&mut ctx, 1).unwrap();
            let p = d.read_frame(&mut ctx).unwrap().unwrap();
            assert_eq!(p.pts, Some(8));
            assert_eq!(p.pos, (44 + 16) as i64);
            assert_eq!(i16::from_le_bytes([p.data[0], p.data[1]]), expand(16));
        }
    }

    #[test]
    fn truncated_data_chunk_is_clamped_to_the_file() {
        // `data` claims 1000 bytes but only 10 follow (streamed capture).
        let mut wav = build_wav(5, 8_000, 1, 16);
        let data_size_off = wav.len() - 10 - 4;
        wav[data_size_off..data_size_off + 4].copy_from_slice(&1000u32.to_le_bytes());
        let mut ctx = ctx_with(wav);
        let mut d = WAVDemuxer::default();
        d.read_header(&mut ctx).unwrap();
        assert_eq!(ctx.streams[0].duration, 5);
        let pkts = read_packets(&mut ctx, &mut d);
        assert_eq!(pkts.iter().map(|p| p.duration).sum::<i64>(), 5);
    }
}
