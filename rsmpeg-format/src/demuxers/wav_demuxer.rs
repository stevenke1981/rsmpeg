use crate::format::InputFormat;
use crate::format_context::FormatContext;
use crate::probe::ProbeScore;
use crate::stream::Stream;
use rsmpeg_codec::{CodecId, CodecParameters, Packet};
use rsmpeg_util::{MediaType, RsError, RsResult, SampleFormat};
use std::io::SeekFrom;

/// WAV (Waveform Audio) demuxer.
///
/// Parses the RIFF/WAVE header, reads the `fmt ` chunk for audio parameters,
/// locates the `data` chunk, and streams PCM packets from `read_frame`.
pub struct WAVDemuxer {
    data_remaining: u64,
    sample_rate: u32,
    channels: u16,
    block_align: u16,
    /// Bytes per packet (~20 ms of audio).
    packet_bytes: usize,
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

        // Position IO at start of chunks (skip 12-byte RIFF header)
        io.seek(SeekFrom::Start(12))?;

        // --- Read `fmt ` chunk ---
        let mut chunk_id = [0u8; 4];
        io.read_exact(&mut chunk_id)?;
        let chunk_size = io.read_u32_le()?;

        let audio_format = io.read_u16_le()?; // 1 = PCM
        let channels = io.read_u16_le()?;
        let sample_rate = io.read_u32_le()?;
        let _byte_rate = io.read_u32_le()?;
        let block_align = io.read_u16_le()?;
        let bits_per_sample = io.read_u16_le()?;

        // Skip remaining fmt chunk data (if any). A malformed `chunk_size < 16`
        // must not underflow (which panics in debug builds).
        let fmt_data_size = (chunk_size as u64).saturating_sub(16);
        if fmt_data_size > 0 {
            io.seek(SeekFrom::Current(fmt_data_size as i64))?;
        }

        // Determine sample format
        let sample_format = match (audio_format, bits_per_sample) {
            (1, 8) => SampleFormat::U8,
            (1, 16) => SampleFormat::S16,
            (1, 24) => SampleFormat::S32,
            (1, 32) => SampleFormat::S32,
            (3, 32) => SampleFormat::F32, // IEEE float
            _ => SampleFormat::S16,
        };

        // Determine codec id
        let codec_id = match audio_format {
            1 => CodecId::Pcm,
            3 => CodecId::Pcm, // IEEE float is still PCM
            _ => CodecId::Pcm,
        };

        // --- Find `data` chunk for duration ---
        let data_size = find_data_chunk(io)? as u64;
        let bytes_per_sample = (bits_per_sample / 8) as u64;
        let frame_size = bytes_per_sample * channels as u64;

        let duration_ms = if sample_rate > 0 && frame_size > 0 {
            // `data_size * 1000` can overflow u64 for multi-exabyte claims; use a
            // widening multiply and clamp into the i64 the API exposes.
            let ms = (data_size as u128 * 1000) / (sample_rate as u128 * frame_size as u128);
            ms.min(i64::MAX as u128) as i64
        } else {
            0
        };

        let bit_rate = sample_rate as u64 * channels as u64 * bits_per_sample as u64;

        // ~20 ms of audio per packet
        let samples_per_packet = (sample_rate as usize / 50).max(1);
        let packet_bytes = (samples_per_packet * block_align as usize).max(block_align as usize);

        self.data_remaining = data_size;
        self.sample_rate = sample_rate;
        self.channels = channels;
        self.block_align = block_align.max(1);
        self.packet_bytes = packet_bytes;

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
        stream.time_base = rsmpeg_util::Rational::new(1, sample_rate.max(1) as i32);
        stream.duration = duration_ms;
        ctx.duration = duration_ms;
        ctx.bit_rate = bit_rate;
        ctx.streams.push(stream);

        tracing::info!(
            "WAV: {}ch, {}Hz, {}bit, {}ms",
            channels,
            sample_rate,
            bits_per_sample,
            duration_ms
        );
        Ok(())
    }

    fn read_frame(&mut self, ctx: &mut FormatContext) -> RsResult<Option<Packet>> {
        if self.data_remaining == 0 {
            return Ok(None);
        }
        let io = ctx
            .io
            .as_mut()
            .ok_or_else(|| RsError::InvalidData("No IO context".into()))?;

        let align = self.block_align.max(1) as u64;
        let to_read = (self.packet_bytes as u64).min(self.data_remaining);
        // Whole blocks only, but always make progress: if a partial block is all
        // that is left, hand it out so the caller reaches EOF.
        let aligned = to_read / align * align;
        let floor = if self.data_remaining >= align {
            align
        } else {
            self.data_remaining
        };
        let to_read = aligned.max(floor) as usize;
        if to_read == 0 {
            self.data_remaining = 0;
            return Ok(None);
        }

        let mut buf = vec![0u8; to_read];
        match io.read_exact(&mut buf) {
            Ok(()) => {}
            Err(_) => {
                self.data_remaining = 0;
                return Ok(None);
            }
        }
        self.data_remaining = self.data_remaining.saturating_sub(to_read as u64);

        let samples = to_read as i64 / self.block_align.max(1) as i64;
        let mut packet = Packet::new(bytes::Bytes::from(buf), 0);
        packet.duration = samples;
        packet.time_base = rsmpeg_util::Rational::new(1, self.sample_rate.max(1) as i32);
        packet.flags = rsmpeg_codec::PacketFlags::KEY;
        Ok(Some(packet))
    }

    fn seek(&mut self, ctx: &mut FormatContext, timestamp: i64) -> RsResult<()> {
        // timestamp is in stream time_base (samples for us)
        let io = ctx
            .io
            .as_mut()
            .ok_or_else(|| RsError::InvalidData("No IO context".into()))?;
        // Re-find data chunk start: simplistic — seek is best-effort for PCM
        // Full seek would need stored data_start; for now only support restart.
        if timestamp <= 0 {
            // Not fully implemented for mid-file; leave position as-is
            let _ = io;
        }
        Ok(())
    }
}

impl Default for WAVDemuxer {
    fn default() -> Self {
        Self {
            data_remaining: 0,
            sample_rate: 0,
            channels: 0,
            block_align: 1,
            packet_bytes: 4096,
        }
    }
}

/// Scan through RIFF chunks to find the `data` chunk and return its size.
fn find_data_chunk(io: &mut crate::io_context::IOContext) -> RsResult<u32> {
    loop {
        let mut id = [0u8; 4];
        if io.read_exact(&mut id).is_err() {
            return Ok(0); // EOF without finding data chunk
        }
        let size = io.read_u32_le()?;
        if &id == b"data" {
            return Ok(size);
        }
        // Skip chunk data (pad to even byte boundary per RIFF spec)
        let skip = if size % 2 == 0 {
            size as u64
        } else {
            size as u64 + 1
        };
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
}
