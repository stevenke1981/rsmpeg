use crate::format::{InputFormat, OutputFormat};
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
    pub output: Option<Box<dyn OutputFormat>>,
    pub streams: Vec<Stream>,
    pub io: Option<IOContext>,
    pub metadata: Dict,
    pub duration: i64,
    pub bit_rate: u64,
    pub filename: Option<String>,
    pub format_name: Option<String>,
}

fn create_muxer(format_or_ext: &str) -> RsResult<(&'static str, Box<dyn OutputFormat>)> {
    match format_or_ext.to_lowercase().as_str() {
        "wav" => Ok(("wav", Box::new(crate::muxers::WAVMuxer::default()))),
        "y4m" => Ok(("y4m", Box::new(crate::muxers::Y4mMuxer::default()))),
        other => Err(RsError::Unsupported(
            format!("Unsupported output format: '{other}'").into(),
        )),
    }
}

impl FormatContext {
    /// Open a media file for reading and detect the container format.
    ///
    /// This peeks at the first bytes, probes registered demuxers and
    /// magic-byte heuristics, and sets `format_name`. Use [`read_header`]
    /// afterwards to actually parse the container headers and discover streams.
    pub fn open_input(path: impl AsRef<Path>) -> RsResult<Self> {
        let path = path.as_ref();
        let mut io = IOContext::open_file(path)?;

        // Probe with as many bytes as the file actually has: a short file (or
        // an empty one) is a valid input, not an I/O error.
        let probe_buf = io.peek_prefix(2048)?;
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

    /// Open an output file path for muxing, selecting the built-in muxer from
    /// the lowercase file extension.
    pub fn open_output(path: impl AsRef<Path>) -> RsResult<Self> {
        let path = path.as_ref();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        let (format_name, muxer) = create_muxer(&ext)?;
        let io = IOContext::create_file(path)?;

        Ok(FormatContext {
            input: None,
            output: Some(muxer),
            streams: Vec::new(),
            io: Some(io),
            metadata: Dict::new(),
            duration: 0,
            bit_rate: 0,
            filename: Some(path.to_string_lossy().to_string()),
            format_name: Some(format_name.to_string()),
        })
    }

    /// Open an in-memory output buffer for muxing in the named format.
    pub fn open_output_buffer(format_name: &str) -> RsResult<Self> {
        let (canonical_name, muxer) = create_muxer(format_name)?;
        Ok(FormatContext {
            input: None,
            output: Some(muxer),
            streams: Vec::new(),
            io: Some(IOContext::from_output_buffer()),
            metadata: Dict::new(),
            duration: 0,
            bit_rate: 0,
            filename: None,
            format_name: Some(canonical_name.to_string()),
        })
    }

    /// Read the container header to discover streams.
    ///
    /// Must be called after [`open_input`]. Uses the detected `format_name`
    /// to instantiate the appropriate built-in demuxer and parse the headers.
    pub fn read_header(&mut self) -> RsResult<()> {
        let format_name = self.format_name.clone().ok_or_else(|| {
            RsError::InvalidData("No format detected, call open_input first".into())
        })?;

        let mut demuxer: Option<Box<dyn InputFormat>> = None;

        // Match on format name to create a fresh demuxer instance.
        // (Box<dyn InputFormat> is not Clone, so we cannot reuse a registry entry.)
        match format_name.as_str() {
            "mp4" | "mov" | "m4a" | "m4v" => {
                demuxer = Some(Box::new(crate::demuxers::MP4Demuxer::default()));
            }
            "matroska" | "mkv" | "webm" | "mka" | "mks" => {
                demuxer = Some(Box::new(crate::demuxers::MKVDemuxer::default()));
            }
            "avi" => {
                demuxer = Some(Box::new(crate::demuxers::AVIDemuxer::default()));
            }
            "flac" => {
                demuxer = Some(Box::new(crate::demuxers::FLACDemuxer::default()));
            }
            "wav" => {
                demuxer = Some(Box::new(crate::demuxers::WAVDemuxer::default()));
            }
            "rawvideo" | "raw" | "yuv" => {
                demuxer = Some(Box::new(crate::demuxers::RawVideoDemuxer));
            }
            "y4m" => {
                demuxer = Some(Box::new(crate::demuxers::Y4mDemuxer::default()));
            }
            other => {
                tracing::warn!("No built-in demuxer registered for format: {}", other);
            }
        }

        if let Some(mut d) = demuxer {
            d.read_header(self)?;
            self.input = Some(d);
            tracing::info!(
                "Read header for '{}': {} stream(s)",
                format_name,
                self.streams.len()
            );
        }

        Ok(())
    }

    /// Read one frame from the input.
    ///
    /// The demuxer is moved out of the context for the duration of the call so
    /// it can borrow the rest of the context mutably; it is always put back —
    /// including on error — so a failed read never silently loses the demuxer.
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

    /// Seek to a timestamp (container-dependent units).
    pub fn seek(&mut self, timestamp: i64) -> RsResult<()> {
        if let Some(mut input) = self.input.take() {
            let result = input.seek(self, timestamp);
            self.input = Some(input);
            result
        } else {
            Err(RsError::InvalidData(
                "seek called but no input format loaded".into(),
            ))
        }
    }

    /// Write the container header to the output.
    pub fn write_header(&mut self) -> RsResult<()> {
        if let Some(mut output) = self.output.take() {
            let result = output.write_header(self);
            self.output = Some(output);
            result
        } else {
            Err(RsError::InvalidData(
                "write_header called but no output format loaded".into(),
            ))
        }
    }

    /// Write a frame to the output.
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

    /// Write the container trailer and flush the output.
    pub fn write_trailer(&mut self) -> RsResult<()> {
        if let Some(mut output) = self.output.take() {
            let result = output.write_trailer(self);
            self.output = Some(output);
            result
        } else {
            Err(RsError::InvalidData(
                "write_trailer called but no output format loaded".into(),
            ))
        }
    }

    /// Whether a demuxer has been installed by [`read_header`](Self::read_header).
    pub fn has_input(&self) -> bool {
        self.input.is_some()
    }

    /// Whether a muxer has been installed on this context.
    pub fn has_output(&self) -> bool {
        self.output.is_some()
    }

    /// Add a stream to the context.
    pub fn add_stream(&mut self, stream: Stream) {
        self.streams.push(stream);
    }

    /// Return the number of streams.
    pub fn nb_streams(&self) -> usize {
        self.streams.len()
    }

    /// Find the index of the best stream of a given media type.
    pub fn find_best_stream(&self, media_type: rsmpeg_util::MediaType) -> Option<usize> {
        self.streams.iter().position(|s| s.media_type == media_type)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::ProbeScore;
    use rsmpeg_util::{MediaType, RsResult};

    /// Minimal demuxer that always fails, used to prove the context keeps its
    /// demuxer (and therefore its open file handle) across a failed read.
    struct FailingDemuxer;

    impl InputFormat for FailingDemuxer {
        fn name(&self) -> &'static str {
            "failing"
        }
        fn description(&self) -> &'static str {
            "always fails"
        }
        fn extensions(&self) -> &'static [&'static str] {
            &[]
        }
        fn probe(&self, _buf: &[u8]) -> ProbeScore {
            ProbeScore::NoMatch
        }
        fn read_header(&mut self, _ctx: &mut FormatContext) -> RsResult<()> {
            Ok(())
        }
        fn read_frame(&mut self, _ctx: &mut FormatContext) -> RsResult<Option<Packet>> {
            Err(RsError::InvalidData("synthetic read failure".into()))
        }
        fn seek(&mut self, _ctx: &mut FormatContext, _ts: i64) -> RsResult<()> {
            Err(RsError::InvalidData("synthetic seek failure".into()))
        }
    }

    fn ctx_without_input() -> FormatContext {
        FormatContext {
            input: None,
            output: None,
            streams: Vec::new(),
            io: None,
            metadata: Dict::new(),
            duration: 0,
            bit_rate: 0,
            filename: None,
            format_name: None,
        }
    }

    #[test]
    fn read_frame_without_input_is_none_not_an_error() {
        let mut ctx = ctx_without_input();
        assert!(!ctx.has_input());
        assert!(matches!(ctx.read_frame(), Ok(None)));
    }

    #[test]
    fn failed_read_frame_keeps_the_demuxer_installed() {
        // Regression guard: `read_frame` moves the demuxer out of the context
        // (`Box<dyn InputFormat>` cannot be borrowed while `&mut ctx` is passed)
        // and must put it back even when the read fails, otherwise the demuxer
        // — and its buffered state / file handle — is leaked on every error.
        let mut ctx = ctx_without_input();
        ctx.input = Some(Box::new(FailingDemuxer));
        assert!(ctx.has_input());

        for _ in 0..3 {
            let err = ctx.read_frame().unwrap_err();
            assert!(matches!(err, RsError::InvalidData(_)), "got {err:?}");
            assert!(
                ctx.has_input(),
                "the demuxer must survive a failed read_frame"
            );
        }
    }

    #[test]
    fn failed_seek_keeps_the_demuxer_installed() {
        let mut ctx = ctx_without_input();
        ctx.input = Some(Box::new(FailingDemuxer));
        assert!(ctx.seek(100).is_err());
        assert!(ctx.has_input(), "the demuxer must survive a failed seek");
    }

    #[test]
    fn seek_without_input_reports_invalid_data() {
        let mut ctx = ctx_without_input();
        let err = ctx.seek(0).unwrap_err();
        assert!(matches!(err, RsError::InvalidData(_)), "got {err:?}");
    }

    #[test]
    fn write_frame_without_output_is_a_noop() {
        let mut ctx = ctx_without_input();
        assert!(!ctx.has_output());
        let packet = Packet::new(bytes::Bytes::new(), 0);
        assert!(ctx.write_frame(&packet).is_ok());
    }

    #[test]
    fn write_header_and_trailer_without_output_fail() {
        let mut ctx = ctx_without_input();
        assert!(matches!(ctx.write_header(), Err(RsError::InvalidData(_))));
        assert!(matches!(ctx.write_trailer(), Err(RsError::InvalidData(_))));
    }

    #[test]
    fn open_output_rejects_unsupported_extension() {
        let err = FormatContext::open_output("out.mp4").err().unwrap();
        assert!(matches!(err, RsError::Unsupported(_)));
        let err = FormatContext::open_output_buffer("mp4").err().unwrap();
        assert!(matches!(err, RsError::Unsupported(_)));
    }

    #[test]
    fn format_context_open_output_buffer_wav_and_y4m_roundtrip() {
        // WAV via open_output_buffer
        let mut wav_out = FormatContext::open_output_buffer("wav").unwrap();
        assert!(wav_out.has_output());
        assert_eq!(wav_out.format_name.as_deref(), Some("wav"));
        let mut astream = Stream::new(0, rsmpeg_codec::CodecId::Pcm);
        astream.media_type = MediaType::Audio;
        astream.codec_params.sample_rate = Some(16_000);
        astream.codec_params.channels = Some(1);
        astream.codec_params.sample_format = Some(rsmpeg_util::SampleFormat::S16);
        wav_out.add_stream(astream);
        wav_out.write_header().unwrap();
        let pcm = vec![0x10u8, 0x20, 0x30, 0x40];
        wav_out
            .write_frame(&Packet::new(bytes::Bytes::from(pcm.clone()), 0))
            .unwrap();
        wav_out.write_trailer().unwrap();
        let wav_bytes = wav_out.io.take().unwrap().into_buffer().unwrap();
        assert_eq!(u32::from_le_bytes(wav_bytes[4..8].try_into().unwrap()), 40);
        assert_eq!(u32::from_le_bytes(wav_bytes[40..44].try_into().unwrap()), 4);

        // Y4M via open_output + open_input + read_header + read_frame + seek
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let y4m_path =
            std::env::temp_dir().join(format!("rsmpeg_y4m_{}_{nanos}.y4m", std::process::id()));
        {
            let mut y4m_out = FormatContext::open_output(&y4m_path).unwrap();
            assert!(y4m_out.has_output());
            let mut vstream = Stream::new(0, rsmpeg_codec::CodecId::RawVideo);
            vstream.media_type = MediaType::Video;
            vstream.time_base = rsmpeg_util::Rational::new(1, 25);
            vstream.codec_params.width = Some(2);
            vstream.codec_params.height = Some(2);
            vstream.codec_params.pixel_format = Some(rsmpeg_util::PixelFormat::Yuv420P);
            y4m_out.add_stream(vstream);
            y4m_out.write_header().unwrap();
            y4m_out
                .write_frame(&Packet::new(bytes::Bytes::from(vec![1u8; 6]), 0))
                .unwrap();
            y4m_out
                .write_frame(&Packet::new(bytes::Bytes::from(vec![2u8; 6]), 0))
                .unwrap();
            y4m_out.write_trailer().unwrap();
        }
        {
            let mut y4m_in = FormatContext::open_input(&y4m_path).unwrap();
            assert_eq!(y4m_in.format_name.as_deref(), Some("y4m"));
            y4m_in.read_header().unwrap();
            assert!(y4m_in.has_input());
            assert_eq!(y4m_in.streams[0].duration, 2);
            let p0 = y4m_in.read_frame().unwrap().unwrap();
            assert_eq!(p0.data.as_ref(), &[1u8; 6]);
            y4m_in.seek(40).unwrap();
            let p1 = y4m_in.read_frame().unwrap().unwrap();
            assert_eq!(p1.pts, Some(1));
            assert_eq!(p1.data.as_ref(), &[2u8; 6]);
        }
        let _ = std::fs::remove_file(&y4m_path);
    }

    #[test]
    fn add_stream_and_find_best_stream() {
        let mut ctx = ctx_without_input();
        ctx.add_stream(Stream::new(0, rsmpeg_codec::CodecId::H264));
        ctx.streams[0].media_type = MediaType::Video;
        ctx.add_stream(Stream::new(1, rsmpeg_codec::CodecId::Aac));
        ctx.streams[1].media_type = MediaType::Audio;
        assert_eq!(ctx.nb_streams(), 2);
        assert_eq!(ctx.find_best_stream(MediaType::Video), Some(0));
        assert_eq!(ctx.find_best_stream(MediaType::Audio), Some(1));
        assert_eq!(ctx.find_best_stream(MediaType::Subtitle), None);
    }
}
