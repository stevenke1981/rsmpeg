use crate::format::OutputFormat;
use crate::format_context::FormatContext;
use rsmpeg_codec::{CodecId, Packet};
use rsmpeg_util::{MediaType, RsError, RsResult, SampleFormat};
use std::io::SeekFrom;

/// RIFF header size before the `data` payload: 12-byte RIFF/WAVE header +
/// 24-byte `fmt ` chunk + 8-byte `data` chunk header = 44 bytes.
const WAV_HEADER_SIZE: u64 = 44;

/// Offset of the RIFF chunk size field (total file length minus 8), which is
/// `36 + data_size`.
const RIFF_SIZE_OFFSET: u64 = 4;

/// Size of the header bytes covered by `RIFF_size` after byte 8 (`WAVE` +
/// `fmt ` chunk + `data` header = 44 - 8 = 36).
const RIFF_HEADER_AFTER_SIZE: u64 = WAV_HEADER_SIZE - 8;

/// WAV (Waveform Audio) muxer.
///
/// Writes a standard 44-byte RIFF/WAVE header (`fmt ` + `data`) with
/// placeholder sizes in [`write_header`](OutputFormat::write_header), streams
/// PCM payloads in [`write_frame`](OutputFormat::write_frame), and back-patches
/// `riff_size` and `data_size` in [`write_trailer`](OutputFormat::write_trailer).
#[derive(Debug, Default)]
pub struct WAVMuxer {
    data_bytes: u64,
    data_size_offset: u64,
    header_written: bool,
    trailer_written: bool,
}

impl OutputFormat for WAVMuxer {
    fn name(&self) -> &'static str {
        "wav"
    }

    fn description(&self) -> &'static str {
        "WAV (Waveform Audio)"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["wav"]
    }

    fn write_header(&mut self, ctx: &mut FormatContext) -> RsResult<()> {
        if ctx.streams.len() != 1 {
            return Err(RsError::InvalidData(
                format!(
                    "WAV muxer requires exactly 1 stream, got {}",
                    ctx.streams.len()
                )
                .into(),
            ));
        }

        let stream = &ctx.streams[0];
        if stream.media_type != MediaType::Audio {
            return Err(RsError::InvalidData(
                "WAV muxer requires an audio stream".into(),
            ));
        }
        if stream.codec_id != CodecId::Pcm {
            return Err(RsError::Unsupported(
                format!(
                    "WAV muxer only supports PCM audio, got {:?}",
                    stream.codec_id
                )
                .into(),
            ));
        }

        let sample_rate = stream
            .codec_params
            .sample_rate
            .filter(|&r| r > 0)
            .ok_or_else(|| RsError::InvalidData("WAV muxer: missing or zero sample_rate".into()))?;
        let channels = stream
            .codec_params
            .channels
            .filter(|&c| c > 0)
            .ok_or_else(|| RsError::InvalidData("WAV muxer: missing or zero channels".into()))?;
        let sample_format = stream
            .codec_params
            .sample_format
            .ok_or_else(|| RsError::InvalidData("WAV muxer: missing sample_format".into()))?;

        let (format_tag, bits_per_sample) = match sample_format {
            SampleFormat::U8 => (1u16, 8u16),
            SampleFormat::S16 => (1u16, 16u16),
            SampleFormat::S32 => (1u16, 32u16),
            SampleFormat::F32 => (3u16, 32u16),
            other => {
                return Err(RsError::Unsupported(
                    format!("WAV muxer: unsupported sample format {other:?}").into(),
                ))
            }
        };

        let bytes_per_sample = bits_per_sample / 8;
        let block_align = channels
            .checked_mul(bytes_per_sample)
            .ok_or_else(|| RsError::InvalidData("WAV muxer: block_align overflow".into()))?;
        let byte_rate = sample_rate
            .checked_mul(u32::from(block_align))
            .ok_or_else(|| RsError::InvalidData("WAV muxer: byte_rate overflow".into()))?;

        let io = ctx
            .io
            .as_mut()
            .ok_or_else(|| RsError::InvalidData("No IO context".into()))?;

        io.write_all(b"RIFF")?;
        io.write_u32_le(0)?;
        io.write_all(b"WAVE")?;
        io.write_all(b"fmt ")?;
        io.write_u32_le(16)?;
        io.write_u16_le(format_tag)?;
        io.write_u16_le(channels)?;
        io.write_u32_le(sample_rate)?;
        io.write_u32_le(byte_rate)?;
        io.write_u16_le(block_align)?;
        io.write_u16_le(bits_per_sample)?;
        io.write_all(b"data")?;
        let data_size_offset = io.tell()?;
        io.write_u32_le(0)?;

        self.data_bytes = 0;
        self.data_size_offset = data_size_offset;
        self.header_written = true;
        self.trailer_written = false;
        Ok(())
    }

    fn write_frame(&mut self, ctx: &mut FormatContext, packet: &Packet) -> RsResult<()> {
        if !self.header_written {
            return Err(RsError::InvalidData(
                "WAV muxer: write_header must be called before write_frame".into(),
            ));
        }
        if self.trailer_written {
            return Err(RsError::InvalidData(
                "WAV muxer: cannot write_frame after write_trailer".into(),
            ));
        }
        if packet.stream_index != 0 {
            return Err(RsError::InvalidData(
                format!(
                    "WAV muxer: invalid stream_index {}, expected 0",
                    packet.stream_index
                )
                .into(),
            ));
        }

        let new_total = self
            .data_bytes
            .checked_add(packet.data.len() as u64)
            .ok_or_else(|| RsError::InvalidData("WAV muxer: data size overflow".into()))?;
        match RIFF_HEADER_AFTER_SIZE.checked_add(new_total) {
            Some(riff_total) if riff_total <= u64::from(u32::MAX) => {}
            _ => {
                return Err(RsError::InvalidData(
                    "WAV muxer: RIFF payload exceeds 4 GiB limit".into(),
                ))
            }
        }

        let io = ctx
            .io
            .as_mut()
            .ok_or_else(|| RsError::InvalidData("No IO context".into()))?;
        io.write_all(&packet.data)?;
        self.data_bytes = new_total;
        Ok(())
    }

    fn write_trailer(&mut self, ctx: &mut FormatContext) -> RsResult<()> {
        if !self.header_written {
            return Err(RsError::InvalidData(
                "WAV muxer: write_header must be called before write_trailer".into(),
            ));
        }

        let data_size = u32::try_from(self.data_bytes)
            .map_err(|_| RsError::InvalidData("WAV muxer: data_size exceeds u32".into()))?;
        let riff_size = u32::try_from(RIFF_HEADER_AFTER_SIZE)
            .ok()
            .and_then(|hdr| hdr.checked_add(data_size))
            .ok_or_else(|| RsError::InvalidData("WAV muxer: riff_size exceeds u32".into()))?;

        let io = ctx
            .io
            .as_mut()
            .ok_or_else(|| RsError::InvalidData("No IO context".into()))?;

        let riff_offset = self
            .data_size_offset
            .checked_sub(RIFF_HEADER_AFTER_SIZE)
            .unwrap_or(RIFF_SIZE_OFFSET);
        io.seek(SeekFrom::Start(riff_offset))?;
        io.write_u32_le(riff_size)?;
        io.seek(SeekFrom::Start(self.data_size_offset))?;
        io.write_u32_le(data_size)?;
        io.seek(SeekFrom::End(0))?;
        io.flush()?;

        self.trailer_written = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demuxers::WAVDemuxer;
    use crate::format::InputFormat;
    use crate::io_context::IOContext;
    use crate::stream::Stream;
    use rsmpeg_codec::CodecParameters;
    use rsmpeg_util::Dict;

    fn audio_ctx(sample_rate: u32, channels: u16, sample_format: SampleFormat) -> FormatContext {
        let mut stream = Stream::new(0, CodecId::Pcm);
        stream.media_type = MediaType::Audio;
        stream.codec_params = CodecParameters {
            codec_id: CodecId::Pcm,
            media_type: MediaType::Audio,
            width: None,
            height: None,
            pixel_format: None,
            sample_rate: Some(sample_rate),
            channels: Some(channels),
            sample_format: Some(sample_format),
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
            format_name: Some("wav".into()),
        }
    }

    #[test]
    fn roundtrip_s16_and_f32_patches_riff_and_data_sizes() {
        // S16 stereo
        let s16_samples: Vec<i16> = vec![0, 1000, -1000, 32767, -32768, 42];
        let s16_bytes: Vec<u8> = s16_samples.iter().flat_map(|s| s.to_le_bytes()).collect();

        let mut mux_ctx = audio_ctx(48_000, 2, SampleFormat::S16);
        let mut muxer = WAVMuxer::default();
        muxer.write_header(&mut mux_ctx).unwrap();
        let half = s16_bytes.len() / 2;
        muxer
            .write_frame(
                &mut mux_ctx,
                &Packet::new(bytes::Bytes::copy_from_slice(&s16_bytes[..half]), 0),
            )
            .unwrap();
        muxer
            .write_frame(
                &mut mux_ctx,
                &Packet::new(bytes::Bytes::copy_from_slice(&s16_bytes[half..]), 0),
            )
            .unwrap();
        muxer.write_trailer(&mut mux_ctx).unwrap();

        let bytes = mux_ctx.io.take().unwrap().into_buffer().unwrap();
        assert_eq!(bytes.len(), 44 + s16_bytes.len());
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(
            u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
            36 + s16_bytes.len() as u32
        );
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(
            u32::from_le_bytes(bytes[40..44].try_into().unwrap()),
            s16_bytes.len() as u32
        );

        let mut demux_ctx = FormatContext {
            input: None,
            output: None,
            streams: Vec::new(),
            io: Some(IOContext::from_buffer(bytes)),
            metadata: Dict::new(),
            duration: 0,
            bit_rate: 0,
            filename: None,
            format_name: Some("wav".into()),
        };
        let mut demuxer = WAVDemuxer::default();
        demuxer.read_header(&mut demux_ctx).unwrap();
        assert_eq!(demux_ctx.streams[0].codec_params.sample_rate, Some(48_000));
        assert_eq!(demux_ctx.streams[0].codec_params.channels, Some(2));
        assert_eq!(
            demux_ctx.streams[0].codec_params.sample_format,
            Some(SampleFormat::S16)
        );
        let mut out = Vec::new();
        while let Some(pkt) = demuxer.read_frame(&mut demux_ctx).unwrap() {
            out.extend_from_slice(&pkt.data);
        }
        assert_eq!(out, s16_bytes);

        // F32 mono
        let f32_samples: Vec<f32> = vec![0.25, -0.5, 0.75, -1.0];
        let f32_bytes: Vec<u8> = f32_samples.iter().flat_map(|f| f.to_le_bytes()).collect();
        let mut mux_ctx = audio_ctx(44_100, 1, SampleFormat::F32);
        let mut muxer = WAVMuxer::default();
        muxer.write_header(&mut mux_ctx).unwrap();
        muxer
            .write_frame(
                &mut mux_ctx,
                &Packet::new(bytes::Bytes::from(f32_bytes.clone()), 0),
            )
            .unwrap();
        muxer.write_trailer(&mut mux_ctx).unwrap();

        let bytes = mux_ctx.io.take().unwrap().into_buffer().unwrap();
        assert_eq!(u16::from_le_bytes(bytes[20..22].try_into().unwrap()), 3);
        let mut demux_ctx = FormatContext {
            input: None,
            output: None,
            streams: Vec::new(),
            io: Some(IOContext::from_buffer(bytes)),
            metadata: Dict::new(),
            duration: 0,
            bit_rate: 0,
            filename: None,
            format_name: Some("wav".into()),
        };
        let mut demuxer = WAVDemuxer::default();
        demuxer.read_header(&mut demux_ctx).unwrap();
        assert_eq!(
            demux_ctx.streams[0].codec_params.sample_format,
            Some(SampleFormat::F32)
        );
        let pkt = demuxer.read_frame(&mut demux_ctx).unwrap().unwrap();
        assert_eq!(pkt.data.as_ref(), f32_bytes.as_slice());
    }

    #[test]
    fn rejects_invalid_stream_configurations() {
        let mut empty = audio_ctx(48_000, 2, SampleFormat::S16);
        empty.streams.clear();
        assert!(matches!(
            WAVMuxer::default().write_header(&mut empty),
            Err(RsError::InvalidData(_))
        ));

        let mut planar = audio_ctx(48_000, 2, SampleFormat::F32P);
        assert!(matches!(
            WAVMuxer::default().write_header(&mut planar),
            Err(RsError::Unsupported(_))
        ));

        let mut f64_ctx = audio_ctx(48_000, 2, SampleFormat::F64);
        assert!(matches!(
            WAVMuxer::default().write_header(&mut f64_ctx),
            Err(RsError::Unsupported(_))
        ));
    }
}
