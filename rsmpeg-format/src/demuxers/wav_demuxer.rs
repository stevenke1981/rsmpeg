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

        // Skip remaining fmt chunk data (if any)
        let fmt_data_size = chunk_size as u64 - 16;
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
            (data_size * 1000 / (sample_rate as u64 * frame_size)) as i64
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

        let to_read = (self.packet_bytes as u64).min(self.data_remaining) as usize;
        // Align to block boundary
        let to_read = (to_read / self.block_align as usize * self.block_align as usize).max(
            if self.data_remaining as usize >= self.block_align as usize {
                self.block_align as usize
            } else {
                self.data_remaining as usize
            },
        );
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
        if skip > 0 {
            if io.seek(SeekFrom::Current(skip as i64)).is_err() {
                return Ok(0);
            }
        }
    }
}
