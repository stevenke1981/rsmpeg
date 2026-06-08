use crate::format_context::FormatContext;
use crate::probe::ProbeScore;
use rsmpeg_codec::Packet;
use rsmpeg_util::RsResult;

/// Demuxer trait, equivalent to FFmpeg's AVInputFormat.
pub trait InputFormat: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn extensions(&self) -> &'static [&'static str];
    fn probe(&self, buf: &[u8]) -> ProbeScore;
    fn read_header(&mut self, ctx: &mut FormatContext) -> RsResult<()>;
    fn read_frame(&mut self, ctx: &mut FormatContext) -> RsResult<Option<Packet>>;
    fn seek(&mut self, ctx: &mut FormatContext, timestamp: i64) -> RsResult<()>;
}

/// Muxer trait.
pub trait OutputFormat: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn extensions(&self) -> &'static [&'static str];
    fn write_header(&mut self, ctx: &mut FormatContext) -> RsResult<()>;
    fn write_frame(&mut self, ctx: &mut FormatContext, packet: &Packet) -> RsResult<()>;
    fn write_trailer(&mut self, ctx: &mut FormatContext) -> RsResult<()>;
}
