use crate::format::InputFormat;
use crate::format_registry::global_format_registry;
use crate::io_context::IOContext;
use crate::probe::probe_format;
use crate::stream::Stream;
use rsmpeg_codec::Packet;
use rsmpeg_util::{Dict, RsError, RsResult};
use std::path::Path;

/// FormatContext — demuxing/muxing context, equivalent to FFmpeg's AVFormatContext.
pub struct FormatContext {
    pub input: Option<Box<dyn InputFormat>>,
    pub output: Option<Box<dyn crate::format::OutputFormat>>,
    pub streams: Vec<Stream>,
    pub io: Option<IOContext>,
    pub metadata: Dict,
    pub duration: i64,
    pub bit_rate: u64,
    pub filename: Option<String>,
    pub format_name: Option<String>,
}

impl FormatContext {
    pub fn open_input(path: impl AsRef<Path>) -> RsResult<Self> {
        let path = path.as_ref();
        let mut io = IOContext::open_file(path)?;

        let probe_buf = io.peek(2048)?;
        let mut ctx = FormatContext {
            input: None,
            output: None,
            streams: Vec::new(),
            io: Some(io),
            metadata: Dict::new(),
            duration: 0,
            bit_rate: 0,
            filename: Some(path.to_string_lossy().to_string()),
            format_name: None,
        };

        // Try to find a demuxer from registry
        let registry = global_format_registry()
            .read()
            .map_err(|_| RsError::Bug("format registry lock poisoned".into()))?;
        if let Some(demuxer) = registry.probe_demuxer(&probe_buf) {
            ctx.format_name = Some(demuxer.name().to_string());
        }

        // Fallback to magic byte probe
        let probe_results = probe_format(&probe_buf);
        if let Some(best) = probe_results.first() {
            if ctx.format_name.is_none() {
                ctx.format_name = Some(best.format_name.to_string());
            }
        }

        Ok(ctx)
    }

    pub fn open_output(path: impl AsRef<Path>) -> RsResult<Self> {
        let path = path.as_ref();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        let registry = global_format_registry()
            .read()
            .map_err(|_| RsError::Bug("format registry lock poisoned".into()))?;
        let muxer = registry.find_muxer(&ext);

        Ok(FormatContext {
            input: None,
            output: None,
            streams: Vec::new(),
            io: None,
            metadata: Dict::new(),
            duration: 0,
            bit_rate: 0,
            filename: Some(path.to_string_lossy().to_string()),
            format_name: muxer.map(|m| m.name().to_string()),
        })
    }

    pub fn read_frame(&mut self) -> RsResult<Option<Packet>> {
        if let Some(mut input) = self.input.take() {
            let result = input.read_frame(self);
            self.input = Some(input);
            result
        } else {
            tracing::warn!("read_frame called but no input format loaded");
            Ok(None)
        }
    }

    pub fn write_frame(&mut self, packet: &Packet) -> RsResult<()> {
        if let Some(mut output) = self.output.take() {
            let result = output.write_frame(self, packet);
            self.output = Some(output);
            result
        } else {
            tracing::warn!("write_frame called but no output format loaded");
            Ok(())
        }
    }

    pub fn add_stream(&mut self, stream: Stream) {
        self.streams.push(stream);
    }

    pub fn nb_streams(&self) -> usize {
        self.streams.len()
    }

    pub fn find_best_stream(&self, media_type: rsmpeg_util::MediaType) -> Option<usize> {
        self.streams.iter().position(|s| s.media_type == media_type)
    }
}
