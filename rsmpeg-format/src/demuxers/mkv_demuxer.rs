use crate::format::InputFormat;
use crate::format_context::FormatContext;
use crate::probe::ProbeScore;
use crate::stream::Stream;
use rsmpeg_codec::{CodecId, Packet};
use rsmpeg_util::{RsError, RsResult};
use std::io::SeekFrom;

/// Matroska/WebM demuxer.
///
/// Detects the EBML header and scans for the Segment element.
/// A full EBML tree parser is beyond the scope of this initial implementation;
/// for now we add a placeholder stream to validate the demuxer pipeline.
pub struct MKVDemuxer;

impl InputFormat for MKVDemuxer {
    fn name(&self) -> &'static str {
        "matroska"
    }

    fn description(&self) -> &'static str {
        "Matroska/WebM"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["mkv", "mka", "mks", "webm"]
    }

    fn probe(&self, buf: &[u8]) -> ProbeScore {
        if buf.len() >= 4 && buf[0..4] == [0x1A, 0x45, 0xDF, 0xA3] {
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

        // Seek to start and scan for the Segment element (0x18538067)
        io.seek(SeekFrom::Start(0))?;

        // Scan byte-by-byte for 0x18 followed by 0x53 0x80 0x67
        let mut buf = [0u8; 1];
        let mut found_segment = false;

        // Limit scan to first 1 MB for safety
        let max_scan = 1024 * 1024;
        let mut scanned = 0u64;

        while scanned < max_scan {
            if io.read_exact(&mut buf).is_err() {
                break;
            }
            scanned += 1;

            if buf[0] == 0x18 {
                let mut next = [0u8; 3];
                if io.read_exact(&mut next).is_err() {
                    break;
                }
                scanned += 3;
                if next == [0x53, 0x80, 0x67] {
                    found_segment = true;
                    break;
                }
            }
        }

        if !found_segment {
            tracing::warn!(
                "MKV: no Segment element found within first {} bytes",
                max_scan
            );
        } else {
            tracing::info!("MKV: detected Matroska container");
        }

        // Add a placeholder stream (even without Segment, we detected the format)
        let stream = Stream::new(0, CodecId::Unknown);
        ctx.streams.push(stream);

        Ok(())
    }

    fn read_frame(&mut self, _ctx: &mut FormatContext) -> RsResult<Option<Packet>> {
        Ok(None)
    }

    fn seek(&mut self, _ctx: &mut FormatContext, _ts: i64) -> RsResult<()> {
        Ok(())
    }
}
