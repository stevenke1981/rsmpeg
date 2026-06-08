use crate::format::InputFormat;
use crate::format_context::FormatContext;
use crate::probe::ProbeScore;
use crate::stream::Stream;
use rsmpeg_codec::{CodecId, Packet};
use rsmpeg_util::{MediaType, RsError, RsResult};
use std::io::SeekFrom;

/// AVI (Audio Video Interleave) demuxer.
///
/// Parses the RIFF/AVI header and scans for `strl` (stream list) chunks
/// within the `hdrl` LIST. Each stream's `strh` chunk provides the
/// media type (vids/auds) and codec FourCC.
pub struct AVIDemuxer;

impl InputFormat for AVIDemuxer {
    fn name(&self) -> &'static str {
        "avi"
    }

    fn description(&self) -> &'static str {
        "AVI (Audio Video Interleave)"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["avi"]
    }

    fn probe(&self, buf: &[u8]) -> ProbeScore {
        if buf.len() >= 12 && &buf[0..4] == b"RIFF" && &buf[8..12] == b"AVI " {
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

        // Seek past the 12-byte RIFF/AVI header
        io.seek(SeekFrom::Start(12))?;

        // We need to find the `hdrl` LIST and then parse `strl` chunks.
        // Simplified approach: scan for `strh` (stream header) chunks.
        let mut stream_index = 0usize;

        loop {
            let mut chunk_id = [0u8; 4];
            if io.read_exact(&mut chunk_id).is_err() {
                break;
            }

            let chunk_size = io.read_u32_le()?;
            let padded_size = if chunk_size % 2 == 0 {
                chunk_size as u64
            } else {
                chunk_size as u64 + 1
            };

            match &chunk_id {
                b"strh" => {
                    // Stream header (56 bytes)
                    let fcc_type = io.read_bytes(4)?;
                    let fcc_handler = io.read_bytes(4)?;
                    let _flags = io.read_u32_le()?;
                    let _priority = io.read_u32_le()?;
                    let _initial_frames = io.read_u32_le()?;
                    let _scale = io.read_u32_le()?;
                    let _rate = io.read_u32_le()?;
                    let _start = io.read_u32_le()?;
                    let _length = io.read_u32_le()?;
                    let _suggested_buffer_size = io.read_u32_le()?;
                    let _quality = io.read_u32_le()?;
                    let _sample_size = io.read_u32_le()?;

                    // Determine media type and codec
                    let (media_type, codec_id) = match &fcc_type[..] {
                        b"vids" => (MediaType::Video, fourcc_to_codec_id(&fcc_handler)),
                        b"auds" => (MediaType::Audio, CodecId::Pcm),
                        _ => (MediaType::Data, CodecId::Unknown),
                    };

                    let mut stream = Stream::new(stream_index, codec_id);
                    stream.media_type = media_type;
                    ctx.streams.push(stream);
                    stream_index += 1;

                    // Skip remaining data (strh is typically 56 bytes, we read 48)
                    let remaining = padded_size as i64 - 48;
                    if remaining > 0 {
                        io.seek(SeekFrom::Current(remaining))?;
                    }
                }
                b"strf" => {
                    // Stream format — skip
                    if padded_size > 0 {
                        io.seek(SeekFrom::Current(padded_size as i64))?;
                    }
                }
                b"idx1" | b"movi" => {
                    // Index or movie data — end of header parsing
                    break;
                }
                b"LIST" => {
                    // LIST chunk — the contents contain sub-chunks.
                    // Read list type (4 bytes) and enter.
                    let mut list_type = [0u8; 4];
                    if io.read_exact(&mut list_type).is_ok() {
                        // Subtract the 4 bytes we just read from remaining
                        let list_remaining = padded_size - 4;
                        if list_remaining > 0 {
                            io.seek(SeekFrom::Current(list_remaining as i64))?;
                        }
                    }
                }
                _ => {
                    // Skip unknown chunk
                    if padded_size > 0 {
                        io.seek(SeekFrom::Current(padded_size as i64))?;
                    }
                }
            }

            // Safety: stop on all-zero chunk id
            if chunk_id.iter().all(|&b| b == 0) {
                break;
            }
        }

        tracing::info!("AVI: found {} stream(s)", stream_index);
        Ok(())
    }

    fn read_frame(&mut self, _ctx: &mut FormatContext) -> RsResult<Option<Packet>> {
        Ok(None)
    }

    fn seek(&mut self, _ctx: &mut FormatContext, _ts: i64) -> RsResult<()> {
        Ok(())
    }
}

/// Map a FourCC code to a CodecId.
fn fourcc_to_codec_id(fourcc: &[u8]) -> CodecId {
    match fourcc {
        b"H264" | b"h264" | b"x264" | b"AVC1" | b"avc1" | b"MP4V" | b"mp4v" | b"XVID" | b"xvid"
        | b"DIVX" | b"divx" | b"MP42" | b"MP43" => CodecId::H264,
        b"VP80" => CodecId::Vp8,
        b"VP90" => CodecId::Vp9,
        b"AV01" => CodecId::Av1,
        b"MJPG" | b"mjpg" => CodecId::Mjpeg,
        b"PNG " => CodecId::Png,
        b"BMP " => CodecId::Bmp,
        _ => CodecId::Unknown,
    }
}
