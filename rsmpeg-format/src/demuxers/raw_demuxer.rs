use crate::format::InputFormat;
use crate::format_context::FormatContext;
use crate::probe::ProbeScore;
use crate::stream::Stream;
use rsmpeg_codec::{CodecId, Packet};
use rsmpeg_util::{MediaType, RsResult};

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
