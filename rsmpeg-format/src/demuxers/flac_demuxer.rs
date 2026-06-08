use crate::format::InputFormat;
use crate::format_context::FormatContext;
use crate::probe::ProbeScore;
use crate::stream::Stream;
use rsmpeg_codec::{CodecId, CodecParameters, Packet};
use rsmpeg_util::{MediaType, RsError, RsResult, SampleFormat};
use std::io::SeekFrom;

/// FLAC (Free Lossless Audio Codec) demuxer.
///
/// Parses the fLaC marker and STREAMINFO metadata block
/// to extract audio parameters (sample rate, channels, bit depth).
pub struct FLACDemuxer;

impl InputFormat for FLACDemuxer {
    fn name(&self) -> &'static str {
        "flac"
    }

    fn description(&self) -> &'static str {
        "FLAC (Free Lossless Audio Codec)"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["flac"]
    }

    fn probe(&self, buf: &[u8]) -> ProbeScore {
        if buf.len() >= 4 && &buf[0..4] == b"fLaC" {
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

        // Skip fLaC marker (4 bytes — already verified by probe)
        io.seek(SeekFrom::Start(4))?;

        // --- Metadata block header ---
        //   bit 7   : last-block flag
        //   bits 6-0: block type (0 = STREAMINFO)
        let block_header_byte = io.read_u8()?;
        let _is_last = (block_header_byte & 0x80) != 0;
        let block_type = block_header_byte & 0x7F;

        // Block size (3 bytes, big-endian)
        let block_size = read_u24_be(io)?;

        if block_type != 0 {
            // Not STREAMINFO — skip this block and return a placeholder stream
            tracing::warn!(
                "FLAC: first metadata block is not STREAMINFO (type={})",
                block_type
            );
            if block_size < 34 {
                io.seek(SeekFrom::Current(block_size as i64))?;
            }
            let stream = Stream::new(0, CodecId::Flac);
            ctx.streams.push(stream);
            return Ok(());
        }

        // --- Read STREAMINFO block (34 bytes) ---
        let info = io.read_bytes(34)?;

        // Sample rate: 20 bits spanning bytes 10, 11, upper nibble of byte 12
        //   byte[10] = bits 19-12
        //   byte[11] = bits 11-4
        //   byte[12] upper nibble = bits 3-0
        let sample_rate =
            ((info[10] as u32) << 12) | ((info[11] as u32) << 4) | ((info[12] as u32) >> 4);

        // Channels - 1: 3 bits at byte[12] bits 3-1
        let channels = (((info[12] >> 1) & 0x07) + 1) as u16;

        // Bits per sample - 1: 5 bits at byte[12] bit 0 + byte[13] bits 7-4
        let bps_raw = (((info[12] as u32) & 0x01) << 4) | ((info[13] as u32 >> 4) & 0x0F);
        let bits_per_sample = (bps_raw + 1) as u16;

        // Total samples: 36 bits at byte[12] bits 31-28 + bytes 13-16
        //   Actually: bits 28-31 of bitstream = byte[12] bits 3-0 (lower nibble) + 32 bits from bytes 13-16
        //   But total_samples_raw requires careful bit extraction.
        //   For now, use a simplified approach.
        let total_samples = {
            // Lower nibble of byte[12] (bits 28-31 of bitstream) + bytes 13-16 (32 bits) = 36 bits
            let mut buf = [0u8; 5];
            // byte[12] lower nibble (bits 3-0) → 4 bits
            buf[0] = info[12] & 0x0F;
            // bytes 13-16 → 32 bits
            buf[1..5].copy_from_slice(&info[13..17]);
            // Interpret as 36-bit big-endian value
            u64::from_be_bytes([0, 0, 0, buf[0], buf[1], buf[2], buf[3], buf[4]])
        };

        let duration_ms = if sample_rate > 0 {
            (total_samples * 1000 / sample_rate as u64) as i64
        } else {
            0
        };

        // Determine sample format from bits_per_sample
        let sample_format = match bits_per_sample {
            8 => SampleFormat::U8,
            16 => SampleFormat::S16,
            24 => SampleFormat::S32,
            32 => SampleFormat::S32,
            _ => SampleFormat::S16,
        };

        // --- Build stream ---
        let mut stream = Stream::new(0, CodecId::Flac);
        stream.media_type = MediaType::Audio;
        stream.codec_params = CodecParameters {
            codec_id: CodecId::Flac,
            media_type: MediaType::Audio,
            width: None,
            height: None,
            pixel_format: None,
            sample_format: Some(sample_format),
            sample_rate: Some(sample_rate),
            channels: Some(channels),
            bit_rate: None,
            extradata: None,
        };
        stream.duration = duration_ms;
        ctx.duration = duration_ms;
        ctx.streams.push(stream);

        tracing::info!(
            "FLAC: {}ch, {}Hz, {}bit, {}samples ({}ms)",
            channels,
            sample_rate,
            bits_per_sample,
            total_samples,
            duration_ms
        );

        // Seek to start of audio data (after metadata block)
        io.seek(SeekFrom::Start(4 + 1 + 3 + block_size as u64))?;
        Ok(())
    }

    fn read_frame(&mut self, _ctx: &mut FormatContext) -> RsResult<Option<Packet>> {
        Ok(None)
    }

    fn seek(&mut self, _ctx: &mut FormatContext, _ts: i64) -> RsResult<()> {
        Ok(())
    }
}

/// Read a 24-bit big-endian unsigned integer.
fn read_u24_be(io: &mut crate::io_context::IOContext) -> RsResult<u32> {
    let mut buf = [0u8; 3];
    io.read_exact(&mut buf)?;
    Ok((buf[0] as u32) << 16 | (buf[1] as u32) << 8 | buf[2] as u32)
}
