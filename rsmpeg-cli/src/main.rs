#![forbid(unsafe_code)]

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use clap::{Parser, Subcommand};

mod gui;
mod playback;

#[derive(Parser)]
#[command(
    name = "rsmpeg",
    version,
    about = "Pure Rust multimedia framework (FFmpeg equivalent)"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Probe media file and show stream info
    Probe {
        /// Path to the media file
        input: String,
        /// Show JSON output
        #[arg(short, long)]
        json: bool,
        /// Verbose output (show all codec parameters)
        #[arg(short, long)]
        verbose: bool,
    },
    /// Transcode media file from one format to another
    Transcode {
        /// Input file path
        input: String,
        /// Output file path (.wav or .y4m)
        output: String,
        /// Video codec to use (e.g. rawvideo)
        #[arg(long)]
        vcodec: Option<String>,
        /// Audio codec to use (e.g. pcm_u8, pcm_s16le, pcm_s32le, pcm_f32le)
        #[arg(long)]
        acodec: Option<String>,
        /// Target video width (for .y4m output)
        #[arg(long)]
        width: Option<u32>,
        /// Target video height (for .y4m output)
        #[arg(long)]
        height: Option<u32>,
        /// Target audio sample rate in Hz (for .wav output)
        #[arg(long)]
        sample_rate: Option<u32>,
        /// Target audio channel count (for .wav output)
        #[arg(long)]
        channels: Option<u16>,
        /// Optional maximum frames to transcode
        #[arg(long)]
        max_frames: Option<usize>,
    },
    /// Play media file through system speakers
    Play {
        /// Input file path
        input: String,
        /// List tracks and exit without playing
        #[arg(short, long)]
        info: bool,
    },
    /// List registered formats
    ListFormats,
    /// List registered codecs
    ListCodecs,
    /// Decode media using pure-Rust decoders (FLAC, MJPEG, PCM, RawVideo, SRT/WebVTT)
    Decode {
        /// Input media or subtitle file path
        input: String,
        /// Optional output path (.wav for audio, .ppm for first video frame)
        #[arg(short, long)]
        output: Option<String>,
        /// Optional maximum packets/frames to decode
        #[arg(long)]
        max_frames: Option<usize>,
    },
    /// Launch the AcmeUI Native graphical media player
    Gui {
        /// Optional media file to open on startup
        input: Option<String>,
    },
}

fn main() {
    tracing_subscriber::fmt::init();

    // Register built-in components so the registries are populated
    rsmpeg::format::format_registry::register_builtin_formats();
    rsmpeg::codec::codec_registry::register_builtin_codecs();

    let cli = Cli::parse();

    // No subcommand → default to launching the GUI
    let command = cli.command.unwrap_or(Commands::Gui { input: None });

    match command {
        Commands::Probe {
            input,
            json,
            verbose,
        } => cmd_probe(&input, json, verbose),
        Commands::Transcode {
            input,
            output,
            vcodec,
            acodec,
            width,
            height,
            sample_rate,
            channels,
            max_frames,
        } => {
            let opts = TranscodeOptions {
                vcodec,
                acodec,
                width,
                height,
                sample_rate,
                channels,
                max_frames,
            };
            cmd_transcode(&input, &output, &opts);
        }
        Commands::Play { input, info } => cmd_play(&input, info),
        Commands::ListFormats => cmd_list_formats(),
        Commands::ListCodecs => cmd_list_codecs(),
        Commands::Decode {
            input,
            output,
            max_frames,
        } => cmd_decode(&input, output.as_deref(), max_frames),
        Commands::Gui { input } => cmd_gui(input.as_deref()),
    }
}

fn cmd_probe(input: &str, json: bool, verbose: bool) {
    match rsmpeg::format::FormatContext::open_input(input) {
        Ok(mut ctx) => {
            if let Err(e) = ctx.read_header() {
                eprintln!("Warning: could not read header: {}", e);
            }

            if json {
                let streams: Vec<serde_json::Value> = ctx
                    .streams
                    .iter()
                    .map(|s| {
                        serde_json::json!({
                            "index": s.index,
                            "codec": format!("{:?}", s.codec_id),
                            "media_type": format!("{:?}", s.media_type),
                            "duration_ms": (s.duration_seconds() * 1000.0).round() as i64,
                            "duration_ticks": s.duration,
                            "duration_sec": s.duration_seconds(),
                            "width": s.codec_params.width,
                            "height": s.codec_params.height,
                            "sample_rate": s.codec_params.sample_rate,
                            "channels": s.codec_params.channels,
                        })
                    })
                    .collect();

                let info = serde_json::json!({
                    "filename": ctx.filename,
                    "format": ctx.format_name,
                    "nb_streams": ctx.streams.len(),
                    "duration_ms": ctx.duration,
                    "bit_rate": ctx.bit_rate,
                    "streams": streams,
                });
                match serde_json::to_string_pretty(&info) {
                    Ok(text) => println!("{}", text),
                    Err(e) => {
                        eprintln!("Error serialising media info to JSON: {}", e);
                        std::process::exit(1);
                    }
                }
            } else {
                println!();
                println!("═══ Media Info ═══");
                println!(
                    "  File:    {}",
                    ctx.filename.as_deref().unwrap_or("unknown")
                );
                println!(
                    "  Format:  {}",
                    ctx.format_name.as_deref().unwrap_or("unknown")
                );
                if ctx.duration > 0 {
                    println!(
                        "  Duration: {:.3}s ({} ms)",
                        ctx.duration as f64 / 1000.0,
                        ctx.duration
                    );
                }
                if ctx.bit_rate > 0 {
                    println!("  Bitrate: {} bps", ctx.bit_rate);
                }
                println!("  Streams: {}", ctx.streams.len());
                println!();

                for stream in &ctx.streams {
                    let icon = match stream.media_type {
                        rsmpeg::util::MediaType::Video => "🎬",
                        rsmpeg::util::MediaType::Audio => "🎵",
                        rsmpeg::util::MediaType::Subtitle => "📝",
                        _ => "📦",
                    };
                    let codec_name = format!("{:?}", stream.codec_id);
                    let media_name = format!("{:?}", stream.media_type);

                    if verbose {
                        println!("  {} Stream #{}", icon, stream.index);
                        println!("     Codec:     {}", codec_name);
                        println!("     Type:      {}", media_name);
                        println!(
                            "     Duration:  {:.3}s ({} ticks)",
                            stream.duration_seconds(),
                            stream.duration
                        );
                        // `if let` + shadowed value: the previous
                        // `unwrap_or(0) > 0` guard followed by `unwrap()` was
                        // correct but easy to break when the guard changed.
                        if let Some(bit_rate) = stream.codec_params.bit_rate.filter(|b| *b > 0) {
                            println!("     Bitrate:   {} bps", bit_rate);
                        }
                        if let Some(width) = stream.codec_params.width.filter(|w| *w > 0) {
                            println!("     Width:     {}", width);
                        }
                        if let Some(height) = stream.codec_params.height.filter(|h| *h > 0) {
                            println!("     Height:    {}", height);
                        }
                        if let Some(sample_rate) =
                            stream.codec_params.sample_rate.filter(|s| *s > 0)
                        {
                            println!("     SampleRate: {} Hz", sample_rate);
                        }
                        if let Some(channels) = stream.codec_params.channels.filter(|c| *c > 0) {
                            println!("     Channels:  {}", channels);
                        }
                        if let Some(fmt) = stream.codec_params.sample_format {
                            println!("     SampleFmt: {:?}", fmt);
                        }
                        if let Some(pixel_format) = stream.codec_params.pixel_format {
                            println!("     PixelFmt:  {:?}", pixel_format);
                        }
                        if !stream.metadata.is_empty() {
                            for (k, v) in stream.metadata.iter() {
                                println!("     Metadata:  {} = {}", k, v);
                            }
                        }
                    } else {
                        println!(
                            "  {} Stream #{}: {} ({}) — {:.3}s",
                            icon,
                            stream.index,
                            codec_name,
                            media_name,
                            stream.duration_seconds()
                        );
                    }
                    println!();
                }
            }
        }
        Err(e) => {
            eprintln!("Error probing file: {}", e);
            std::process::exit(1);
        }
    }
}

fn cmd_list_formats() {
    println!();
    println!("═══ Registered Demuxers ═══");
    let registry = rsmpeg::format::format_registry::global_format_registry();
    match registry.read() {
        Ok(reg) => {
            for demuxer in reg.demuxers() {
                println!(
                    "  {} — {} ({})",
                    demuxer.name(),
                    demuxer.description(),
                    demuxer.extensions().join(", ")
                );
            }
            println!("  (Total: {} demuxers)", reg.demuxers().len());
            println!();
            println!("═══ Registered Muxers ═══");
            for muxer in reg.muxers() {
                println!(
                    "  {} — {} ({})",
                    muxer.name(),
                    muxer.description(),
                    muxer.extensions().join(", ")
                );
            }
            println!("  (Total: {} muxers)", reg.muxers().len());
        }
        Err(e) => {
            eprintln!("Error reading format registry: {}", e);
            std::process::exit(1);
        }
    }
}

fn cmd_list_codecs() {
    println!();
    println!("═══ Registered Codecs ═══");
    let registry = rsmpeg::codec::codec_registry::global_codec_registry();
    match registry.read() {
        Ok(reg) => {
            for codec in reg.list() {
                let caps = if codec.capabilities().can_decode && codec.capabilities().can_encode {
                    "dec/enc"
                } else if codec.capabilities().can_decode {
                    "decode"
                } else if codec.capabilities().can_encode {
                    "encode"
                } else {
                    "none"
                };
                println!("  {} — {} [{}]", codec.name(), codec.long_name(), caps);
            }
            println!("  (Total: {} codecs)", reg.len());
        }
        Err(e) => {
            eprintln!("Error reading codec registry: {}", e);
            std::process::exit(1);
        }
    }
}

#[derive(Debug, Default, Clone)]
struct TranscodeOptions {
    vcodec: Option<String>,
    acodec: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    sample_rate: Option<u32>,
    channels: Option<u16>,
    max_frames: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TranscodeSummary {
    input_format: String,
    output_format: String,
    frames_written: usize,
    audio_samples: usize,
    sample_rate: u32,
    channels: u16,
    width: usize,
    height: usize,
}

fn channel_layout_for(channels: u16) -> Result<rsmpeg::ChannelLayout, String> {
    match channels {
        1 => Ok(rsmpeg::ChannelLayout::MONO),
        2 => Ok(rsmpeg::ChannelLayout::STEREO),
        6 => Ok(rsmpeg::ChannelLayout::_5POINT1),
        8 => Ok(rsmpeg::ChannelLayout::_7POINT1),
        other => Err(format!(
            "unsupported channel count {} (supported: 1, 2, 6, 8)",
            other
        )),
    }
}

fn parse_wav_acodec(acodec: &str) -> Result<rsmpeg::SampleFormat, String> {
    match acodec.to_ascii_lowercase().as_str() {
        "pcm_u8" | "u8" => Ok(rsmpeg::SampleFormat::U8),
        "pcm_s16le" | "s16" => Ok(rsmpeg::SampleFormat::S16),
        "pcm_s32le" | "s32" => Ok(rsmpeg::SampleFormat::S32),
        "pcm_f32le" | "f32" => Ok(rsmpeg::SampleFormat::F32),
        other => Err(format!(
            "unsupported audio codec '{}' for WAV output (supported: pcm_u8, pcm_s16le, pcm_s32le, pcm_f32le)",
            other
        )),
    }
}

fn transcode_media(
    input: &str,
    output: &str,
    opts: &TranscodeOptions,
) -> Result<TranscodeSummary, String> {
    let ext = Path::new(output)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();

    match ext.as_str() {
        "wav" => transcode_to_wav(input, output, opts),
        "y4m" => transcode_to_y4m(input, output, opts),
        _ => Err(format!(
            "unsupported output format for '{}' (supported pure-Rust muxers: .wav, .y4m)",
            output
        )),
    }
}

fn transcode_to_wav(
    input: &str,
    output: &str,
    opts: &TranscodeOptions,
) -> Result<TranscodeSummary, String> {
    let requested_fmt = opts.acodec.as_deref().map(parse_wav_acodec).transpose()?;
    if let Some(0) = opts.sample_rate {
        return Err("target --sample-rate must be greater than 0".into());
    }
    if let Some(ch) = opts.channels {
        channel_layout_for(ch)?;
    }

    let mut in_ctx = rsmpeg::format::FormatContext::open_input(input)
        .map_err(|e| format!("failed to open input '{}': {}", input, e))?;
    in_ctx
        .read_header()
        .map_err(|e| format!("failed to read header for '{}': {}", input, e))?;
    let input_format = in_ctx
        .format_name
        .clone()
        .unwrap_or_else(|| "unknown".to_string());

    let Some((stream_idx, src_rate_hdr, src_ch_hdr, mut decoder)) =
        init_pure_rust_audio_decoder(&in_ctx.streams)?
    else {
        let detected: Vec<String> = in_ctx
            .streams
            .iter()
            .map(|s| format!("#{} {} ({:?})", s.index, s.codec_id.name(), s.media_type))
            .collect();
        return Err(format!(
            "no pure-Rust decodable audio stream found in '{}' (supported: flac, pcm; detected: {})",
            input,
            if detected.is_empty() {
                "none".to_string()
            } else {
                detected.join(", ")
            }
        ));
    };

    let src_fmt_hdr = in_ctx
        .streams
        .iter()
        .find(|s| s.index == stream_idx)
        .and_then(|s| s.codec_params.sample_format);

    let target_fmt = requested_fmt.unwrap_or(match src_fmt_hdr {
        Some(
            f @ (rsmpeg::SampleFormat::U8
            | rsmpeg::SampleFormat::S16
            | rsmpeg::SampleFormat::S32
            | rsmpeg::SampleFormat::F32),
        ) => f,
        _ => rsmpeg::SampleFormat::S16,
    });
    let target_rate = opts.sample_rate.unwrap_or(src_rate_hdr);
    if target_rate == 0 {
        return Err("missing or zero audio sample rate in input stream".into());
    }
    let target_channels = opts.channels.unwrap_or(src_ch_hdr);
    let dst_layout = channel_layout_for(target_channels)?;

    let mut out_ctx = rsmpeg::format::FormatContext::open_output(output)
        .map_err(|e| format!("failed to open output '{}': {}", output, e))?;
    let mut out_stream = rsmpeg::format::Stream::new(0, rsmpeg::CodecId::Pcm);
    out_stream.media_type = rsmpeg::util::MediaType::Audio;
    out_stream.time_base = rsmpeg::Rational::new(1, target_rate as i32);
    out_stream.codec_params.codec_id = rsmpeg::CodecId::Pcm;
    out_stream.codec_params.media_type = rsmpeg::util::MediaType::Audio;
    out_stream.codec_params.sample_rate = Some(target_rate);
    out_stream.codec_params.channels = Some(target_channels);
    out_stream.codec_params.sample_format = Some(target_fmt);
    out_ctx.add_stream(out_stream);
    out_ctx
        .write_header()
        .map_err(|e| format!("failed to write WAV header to '{}': {}", output, e))?;

    struct WavWriterState {
        out_ctx: rsmpeg::format::FormatContext,
        encoder: rsmpeg::codec::PCMAudioEncoder,
        resampler_cache: Option<(u32, u16, rsmpeg::SampleFormat, rsmpeg::resample::Resampler)>,
        target_rate: u32,
        target_channels: u16,
        target_fmt: rsmpeg::SampleFormat,
        dst_layout: rsmpeg::ChannelLayout,
        max_frames: Option<usize>,
        frames_written: usize,
        audio_samples: usize,
    }

    impl WavWriterState {
        fn at_limit(&self) -> bool {
            self.max_frames.is_some_and(|m| self.frames_written >= m)
        }

        fn process(&mut self, frame: rsmpeg::Frame) -> Result<(), String> {
            if self.at_limit() || frame.samples == 0 {
                return Ok(());
            }
            let src_rate = if frame.sample_rate > 0 {
                frame.sample_rate
            } else {
                self.target_rate
            };
            let src_ch = if frame.channels > 0 {
                frame.channels
            } else {
                self.target_channels
            };
            let needs_resample = src_rate != self.target_rate
                || src_ch != self.target_channels
                || frame.sample_format != self.target_fmt
                || frame.sample_format.is_planar();

            let out_frame = if needs_resample {
                let key = (src_rate, src_ch, frame.sample_format);
                let rebuild = self
                    .resampler_cache
                    .as_ref()
                    .is_none_or(|(r, c, f, _)| (*r, *c, *f) != key);
                if rebuild {
                    let src_layout = channel_layout_for(src_ch)?;
                    let cfg = rsmpeg::resample::ResamplerConfig::new(
                        src_rate,
                        self.target_rate,
                        frame.sample_format,
                        self.target_fmt,
                    )
                    .with_channel_layouts(src_layout, self.dst_layout);
                    let resampler = rsmpeg::resample::Resampler::new(cfg)
                        .map_err(|e| format!("failed to create audio resampler: {}", e))?;
                    self.resampler_cache = Some((src_rate, src_ch, frame.sample_format, resampler));
                }
                let (_, _, _, resampler) = self
                    .resampler_cache
                    .as_ref()
                    .expect("resampler initialized");
                resampler
                    .resample(&frame)
                    .map_err(|e| format!("audio resample error: {}", e))?
            } else {
                frame
            };

            if out_frame.samples == 0 {
                return Ok(());
            }
            let sample_count = out_frame.samples;
            let packets = rsmpeg::codec::Encoder::encode(&mut self.encoder, &out_frame)
                .map_err(|e| format!("PCM encode error: {}", e))?;
            for pkt in packets {
                self.out_ctx
                    .write_frame(&pkt)
                    .map_err(|e| format!("failed to write WAV packet: {}", e))?;
            }
            self.frames_written += 1;
            self.audio_samples += sample_count;
            Ok(())
        }
    }

    let mut state = WavWriterState {
        out_ctx,
        encoder: rsmpeg::codec::PCMAudioEncoder::new(target_fmt),
        resampler_cache: None,
        target_rate,
        target_channels,
        target_fmt,
        dst_layout,
        max_frames: opts.max_frames,
        frames_written: 0,
        audio_samples: 0,
    };

    while !state.at_limit() {
        let Some(pkt) = in_ctx
            .read_frame()
            .map_err(|e| format!("error reading input packet: {}", e))?
        else {
            break;
        };
        if pkt.stream_index != stream_idx {
            continue;
        }
        let frames = decoder
            .decode(&pkt)
            .map_err(|e| format!("audio decode error: {}", e))?;
        for frame in frames {
            state.process(frame)?;
            if state.at_limit() {
                break;
            }
        }
    }

    if !state.at_limit() {
        let flushed = decoder
            .flush()
            .map_err(|e| format!("audio decoder flush error: {}", e))?;
        for frame in flushed {
            state.process(frame)?;
            if state.at_limit() {
                break;
            }
        }
    }

    if let Some((_, _, _, mut resampler)) = state.resampler_cache.take() {
        if let Some(rem) = resampler
            .flush()
            .map_err(|e| format!("audio resampler flush error: {}", e))?
        {
            if rem.samples > 0 && !state.at_limit() {
                let packets = rsmpeg::codec::Encoder::encode(&mut state.encoder, &rem)
                    .map_err(|e| format!("PCM encode error: {}", e))?;
                for pkt in packets {
                    state
                        .out_ctx
                        .write_frame(&pkt)
                        .map_err(|e| format!("failed to write WAV packet: {}", e))?;
                }
                state.frames_written += 1;
                state.audio_samples += rem.samples;
            }
        }
    }

    for pkt in rsmpeg::codec::Encoder::flush(&mut state.encoder)
        .map_err(|e| format!("PCM encoder flush error: {}", e))?
    {
        state
            .out_ctx
            .write_frame(&pkt)
            .map_err(|e| format!("failed to write WAV packet: {}", e))?;
    }

    state
        .out_ctx
        .write_trailer()
        .map_err(|e| format!("failed to write WAV trailer to '{}': {}", output, e))?;

    let frames_written = state.frames_written;
    let audio_samples = state.audio_samples;

    Ok(TranscodeSummary {
        input_format,
        output_format: "wav".to_string(),
        frames_written,
        audio_samples,
        sample_rate: target_rate,
        channels: target_channels,
        width: 0,
        height: 0,
    })
}

fn packed_rgb_to_yuv420p(frame: &rsmpeg::Frame) -> Result<rsmpeg::Frame, String> {
    let w = frame.width;
    let h = frame.height;
    if w == 0 || h == 0 {
        return Err("cannot convert zero-dimension RGB frame to YUV420P".into());
    }
    let (bpp, r_idx, g_idx, b_idx) = match frame.pixel_format {
        rsmpeg::PixelFormat::Rgb24 => (3usize, 0usize, 1usize, 2usize),
        rsmpeg::PixelFormat::Bgr24 => (3usize, 2usize, 1usize, 0usize),
        rsmpeg::PixelFormat::Rgba => (4usize, 0usize, 1usize, 2usize),
        rsmpeg::PixelFormat::Bgra => (4usize, 2usize, 1usize, 0usize),
        other => {
            return Err(format!(
                "packed_rgb_to_yuv420p does not support {:?}",
                other
            ))
        }
    };
    let row_bytes = w
        .checked_mul(bpp)
        .ok_or_else(|| "RGB frame row size overflow".to_string())?;
    let stride = frame
        .linesize
        .first()
        .copied()
        .filter(|&ls| ls >= row_bytes)
        .unwrap_or(row_bytes);
    let plane = frame
        .data
        .first()
        .ok_or_else(|| "RGB frame has no plane data".to_string())?;
    let needed = (h - 1)
        .checked_mul(stride)
        .and_then(|o| o.checked_add(row_bytes))
        .ok_or_else(|| "RGB frame buffer size overflow".to_string())?;
    if plane.len() < needed {
        return Err("RGB frame buffer is smaller than declared geometry".into());
    }

    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    let mut y_plane = vec![0u8; w * h];
    let mut u_plane = vec![0u8; cw * ch];
    let mut v_plane = vec![0u8; cw * ch];

    for y in 0..h {
        let row = &plane[y * stride..y * stride + row_bytes];
        let y_row = &mut y_plane[y * w..(y + 1) * w];
        for (x, px) in row.chunks_exact(bpp).enumerate() {
            let r = i32::from(px[r_idx]);
            let g = i32::from(px[g_idx]);
            let b = i32::from(px[b_idx]);
            let y_val = ((66 * r + 129 * g + 25 * b + 128) >> 8) + 16;
            y_row[x] = y_val.clamp(16, 235) as u8;
        }
    }

    for cy in 0..ch {
        let y0 = cy * 2;
        let y1 = (y0 + 1).min(h - 1);
        for cx in 0..cw {
            let x0 = cx * 2;
            let x1 = (x0 + 1).min(w - 1);
            let coords = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)];
            let (mut sum_r, mut sum_g, mut sum_b) = (0i32, 0i32, 0i32);
            for (px_x, px_y) in coords {
                let off = px_y * stride + px_x * bpp;
                sum_r += i32::from(plane[off + r_idx]);
                sum_g += i32::from(plane[off + g_idx]);
                sum_b += i32::from(plane[off + b_idx]);
            }
            let r = (sum_r + 2) >> 2;
            let g = (sum_g + 2) >> 2;
            let b = (sum_b + 2) >> 2;
            let u_val = ((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128;
            let v_val = ((112 * r - 94 * g - 18 * b + 128) >> 8) + 128;
            let c_idx = cy * cw + cx;
            u_plane[c_idx] = u_val.clamp(16, 240) as u8;
            v_plane[c_idx] = v_val.clamp(16, 240) as u8;
        }
    }

    let mut out = rsmpeg::Frame::new_video(w, h, rsmpeg::PixelFormat::Yuv420P);
    out.data = vec![y_plane, u_plane, v_plane];
    out.linesize = vec![w, cw, cw];
    out.pts = frame.pts;
    out.duration = frame.duration;
    out.time_base = frame.time_base;
    out.key_frame = frame.key_frame;
    out.pict_type = frame.pict_type;
    Ok(out)
}

fn semi_planar_to_yuv420p(frame: &rsmpeg::Frame) -> Result<rsmpeg::Frame, String> {
    let w = frame.width;
    let h = frame.height;
    if w == 0 || h == 0 {
        return Err("cannot convert zero-dimension NV12/NV21 frame to YUV420P".into());
    }
    if frame.data.len() < 2 {
        return Err("NV12/NV21 frame requires 2 planes".into());
    }
    let is_nv21 = match frame.pixel_format {
        rsmpeg::PixelFormat::Nv12 => false,
        rsmpeg::PixelFormat::Nv21 => true,
        other => return Err(format!("expected NV12/NV21 frame, got {:?}", other)),
    };
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    let uv_row_bytes = cw * 2;
    let y_stride = frame
        .linesize
        .first()
        .copied()
        .filter(|&ls| ls >= w)
        .unwrap_or(w);
    let uv_stride = frame
        .linesize
        .get(1)
        .copied()
        .filter(|&ls| ls >= uv_row_bytes)
        .unwrap_or(uv_row_bytes);

    let y_src = &frame.data[0];
    let uv_src = &frame.data[1];
    if y_src.len() < (h - 1) * y_stride + w || uv_src.len() < (ch - 1) * uv_stride + uv_row_bytes {
        return Err("NV12/NV21 plane buffer is smaller than declared geometry".into());
    }

    let mut y_out = Vec::with_capacity(w * h);
    for row in 0..h {
        let start = row * y_stride;
        y_out.extend_from_slice(&y_src[start..start + w]);
    }
    let mut u_out = Vec::with_capacity(cw * ch);
    let mut v_out = Vec::with_capacity(cw * ch);
    for row in 0..ch {
        let start = row * uv_stride;
        for pair in uv_src[start..start + uv_row_bytes].chunks_exact(2) {
            if is_nv21 {
                v_out.push(pair[0]);
                u_out.push(pair[1]);
            } else {
                u_out.push(pair[0]);
                v_out.push(pair[1]);
            }
        }
    }

    let mut out = rsmpeg::Frame::new_video(w, h, rsmpeg::PixelFormat::Yuv420P);
    out.data = vec![y_out, u_out, v_out];
    out.linesize = vec![w, cw, cw];
    out.pts = frame.pts;
    out.duration = frame.duration;
    out.time_base = frame.time_base;
    out.key_frame = frame.key_frame;
    out.pict_type = frame.pict_type;
    Ok(out)
}

fn pack_video_frame_planes(frame: &rsmpeg::Frame) -> Result<rsmpeg::Frame, String> {
    let w = frame.width;
    let h = frame.height;
    let specs: Vec<(usize, usize)> = match frame.pixel_format {
        rsmpeg::PixelFormat::Yuv420P => {
            let cw = w.div_ceil(2);
            let ch = h.div_ceil(2);
            vec![(w, h), (cw, ch), (cw, ch)]
        }
        rsmpeg::PixelFormat::Yuv422P => {
            let cw = w.div_ceil(2);
            vec![(w, h), (cw, h), (cw, h)]
        }
        rsmpeg::PixelFormat::Yuv444P => vec![(w, h), (w, h), (w, h)],
        rsmpeg::PixelFormat::Gray8 => vec![(w, h)],
        other => {
            return Err(format!(
                "unsupported Y4M pixel format {:?} when packing planes",
                other
            ))
        }
    };
    if frame.data.len() < specs.len() {
        return Err(format!(
            "video frame {:?} expected {} planes, got {}",
            frame.pixel_format,
            specs.len(),
            frame.data.len()
        ));
    }
    let mut packed_data = Vec::with_capacity(specs.len());
    let mut packed_linesize = Vec::with_capacity(specs.len());
    for (idx, (pw, ph)) in specs.into_iter().enumerate() {
        let src = &frame.data[idx];
        let stride = frame
            .linesize
            .get(idx)
            .copied()
            .filter(|&ls| ls >= pw)
            .unwrap_or(pw);
        let needed = ph
            .checked_sub(1)
            .and_then(|r| r.checked_mul(stride))
            .and_then(|o| o.checked_add(pw))
            .ok_or_else(|| "video plane size overflow".to_string())?;
        if src.len() < needed {
            return Err(format!(
                "video plane {} is smaller than {}x{} (stride {})",
                idx, pw, ph, stride
            ));
        }
        if stride == pw && src.len() == pw * ph {
            packed_data.push(src.clone());
        } else {
            let mut dst = Vec::with_capacity(pw * ph);
            for row in 0..ph {
                let start = row * stride;
                dst.extend_from_slice(&src[start..start + pw]);
            }
            packed_data.push(dst);
        }
        packed_linesize.push(pw);
    }

    let mut out = frame.clone();
    out.data = packed_data;
    out.linesize = packed_linesize;
    Ok(out)
}

fn prepare_y4m_frame(
    frame: &rsmpeg::Frame,
    target_w: usize,
    target_h: usize,
    default_time_base: rsmpeg::Rational,
) -> Result<rsmpeg::Frame, String> {
    if frame.width == 0 || frame.height == 0 {
        return Err("decoded video frame has zero dimensions".into());
    }
    let resize = frame.width != target_w || frame.height != target_h;
    let y4m_frame = match frame.pixel_format {
        rsmpeg::PixelFormat::Rgb24
        | rsmpeg::PixelFormat::Bgr24
        | rsmpeg::PixelFormat::Rgba
        | rsmpeg::PixelFormat::Bgra => {
            let yuv = packed_rgb_to_yuv420p(frame)?;
            if resize {
                let cfg = rsmpeg::scale::ScalerConfig::new(
                    yuv.width as u32,
                    yuv.height as u32,
                    rsmpeg::PixelFormat::Yuv420P,
                    target_w as u32,
                    target_h as u32,
                    rsmpeg::PixelFormat::Yuv420P,
                );
                let scaler = rsmpeg::scale::Scaler::new(cfg)
                    .map_err(|e| format!("failed to initialize video scaler: {}", e))?;
                scaler
                    .scale(&yuv)
                    .map_err(|e| format!("video scale error: {}", e))?
            } else {
                yuv
            }
        }
        rsmpeg::PixelFormat::Nv12 | rsmpeg::PixelFormat::Nv21 => {
            let yuv = semi_planar_to_yuv420p(frame)?;
            if resize {
                let cfg = rsmpeg::scale::ScalerConfig::new(
                    yuv.width as u32,
                    yuv.height as u32,
                    rsmpeg::PixelFormat::Yuv420P,
                    target_w as u32,
                    target_h as u32,
                    rsmpeg::PixelFormat::Yuv420P,
                );
                let scaler = rsmpeg::scale::Scaler::new(cfg)
                    .map_err(|e| format!("failed to initialize video scaler: {}", e))?;
                scaler
                    .scale(&yuv)
                    .map_err(|e| format!("video scale error: {}", e))?
            } else {
                yuv
            }
        }
        rsmpeg::PixelFormat::Yuv420P => {
            if resize {
                let cfg = rsmpeg::scale::ScalerConfig::new(
                    frame.width as u32,
                    frame.height as u32,
                    rsmpeg::PixelFormat::Yuv420P,
                    target_w as u32,
                    target_h as u32,
                    rsmpeg::PixelFormat::Yuv420P,
                );
                let scaler = rsmpeg::scale::Scaler::new(cfg)
                    .map_err(|e| format!("failed to initialize video scaler: {}", e))?;
                scaler
                    .scale(frame)
                    .map_err(|e| format!("video scale error: {}", e))?
            } else {
                frame.clone()
            }
        }
        rsmpeg::PixelFormat::Yuv422P
        | rsmpeg::PixelFormat::Yuv444P
        | rsmpeg::PixelFormat::Gray8 => {
            if resize {
                let cfg = rsmpeg::scale::ScalerConfig::new(
                    frame.width as u32,
                    frame.height as u32,
                    frame.pixel_format,
                    target_w as u32,
                    target_h as u32,
                    rsmpeg::PixelFormat::Rgba,
                );
                let scaler = rsmpeg::scale::Scaler::new(cfg)
                    .map_err(|e| format!("failed to initialize video scaler: {}", e))?;
                let rgba = scaler
                    .scale(frame)
                    .map_err(|e| format!("video scale error: {}", e))?;
                packed_rgb_to_yuv420p(&rgba)?
            } else {
                frame.clone()
            }
        }
        other => {
            return Err(format!(
                "unsupported video pixel format {:?} for Y4M output",
                other
            ))
        }
    };

    let mut packed = pack_video_frame_planes(&y4m_frame)?;
    if packed.time_base.den == 0 {
        packed.time_base = default_time_base;
    }
    Ok(packed)
}

fn transcode_to_y4m(
    input: &str,
    output: &str,
    opts: &TranscodeOptions,
) -> Result<TranscodeSummary, String> {
    if let Some(ref vcodec) = opts.vcodec {
        if !vcodec.eq_ignore_ascii_case("rawvideo") {
            return Err(format!(
                "unsupported video codec '{}' for Y4M output (supported: rawvideo)",
                vcodec
            ));
        }
    }
    if opts.width == Some(0) || opts.height == Some(0) {
        return Err("target --width and --height must be greater than 0".into());
    }

    let mut in_ctx = rsmpeg::format::FormatContext::open_input(input)
        .map_err(|e| format!("failed to open input '{}': {}", input, e))?;
    in_ctx
        .read_header()
        .map_err(|e| format!("failed to read header for '{}': {}", input, e))?;
    let input_format = in_ctx
        .format_name
        .clone()
        .unwrap_or_else(|| "unknown".to_string());

    let Some((stream_idx, mut decoder)) = init_pure_rust_video_decoder(&in_ctx.streams)? else {
        let detected: Vec<String> = in_ctx
            .streams
            .iter()
            .map(|s| format!("#{} {} ({:?})", s.index, s.codec_id.name(), s.media_type))
            .collect();
        return Err(format!(
            "no pure-Rust decodable video stream found in '{}' (supported: mjpeg, rawvideo; detected: {})",
            input,
            if detected.is_empty() {
                "none".to_string()
            } else {
                detected.join(", ")
            }
        ));
    };

    let stream_tb = in_ctx
        .streams
        .iter()
        .find(|s| s.index == stream_idx)
        .map(|s| {
            if s.time_base.num > 0 && s.time_base.den > 0 {
                s.time_base
            } else if s.avg_frame_rate.num > 0 && s.avg_frame_rate.den > 0 {
                rsmpeg::Rational::new(s.avg_frame_rate.den, s.avg_frame_rate.num)
            } else {
                rsmpeg::Rational::new(1, 25)
            }
        })
        .unwrap_or_else(|| rsmpeg::Rational::new(1, 25));

    struct Y4mWriterState<'a> {
        output: &'a str,
        stream_tb: rsmpeg::Rational,
        req_width: Option<u32>,
        req_height: Option<u32>,
        max_frames: Option<usize>,
        out_ctx: Option<rsmpeg::format::FormatContext>,
        out_dims: Option<(usize, usize, rsmpeg::PixelFormat)>,
        encoder: rsmpeg::codec::RawVideoEncoder,
        frames_written: usize,
    }

    impl Y4mWriterState<'_> {
        fn at_limit(&self) -> bool {
            self.max_frames.is_some_and(|m| self.frames_written >= m)
        }

        fn write(&mut self, frame: rsmpeg::Frame) -> Result<(), String> {
            if self.at_limit() {
                return Ok(());
            }
            let target_w = self.req_width.map(|w| w as usize).unwrap_or(frame.width);
            let target_h = self.req_height.map(|h| h as usize).unwrap_or(frame.height);
            let prepared = prepare_y4m_frame(&frame, target_w, target_h, self.stream_tb)?;

            if self.out_ctx.is_none() {
                let mut ctx = rsmpeg::format::FormatContext::open_output(self.output)
                    .map_err(|e| format!("failed to open output '{}': {}", self.output, e))?;
                let mut out_stream = rsmpeg::format::Stream::new(0, rsmpeg::CodecId::RawVideo);
                out_stream.media_type = rsmpeg::util::MediaType::Video;
                out_stream.time_base = self.stream_tb;
                out_stream.codec_params.codec_id = rsmpeg::CodecId::RawVideo;
                out_stream.codec_params.media_type = rsmpeg::util::MediaType::Video;
                out_stream.codec_params.width = Some(prepared.width);
                out_stream.codec_params.height = Some(prepared.height);
                out_stream.codec_params.pixel_format = Some(prepared.pixel_format);
                ctx.add_stream(out_stream);
                ctx.write_header().map_err(|e| {
                    format!("failed to write Y4M header to '{}': {}", self.output, e)
                })?;
                self.out_dims = Some((prepared.width, prepared.height, prepared.pixel_format));
                self.out_ctx = Some(ctx);
            } else if let Some((ew, eh, efmt)) = self.out_dims {
                if (prepared.width, prepared.height, prepared.pixel_format) != (ew, eh, efmt) {
                    return Err(format!(
                        "video format changed mid-stream from {}x{} {:?} to {}x{} {:?}",
                        ew, eh, efmt, prepared.width, prepared.height, prepared.pixel_format
                    ));
                }
            }

            let ctx = self.out_ctx.as_mut().expect("output context opened");
            let packets = rsmpeg::codec::Encoder::encode(&mut self.encoder, &prepared)
                .map_err(|e| format!("raw video encode error: {}", e))?;
            for pkt in packets {
                ctx.write_frame(&pkt)
                    .map_err(|e| format!("failed to write Y4M packet: {}", e))?;
            }
            self.frames_written += 1;
            Ok(())
        }
    }

    let mut state = Y4mWriterState {
        output,
        stream_tb,
        req_width: opts.width,
        req_height: opts.height,
        max_frames: opts.max_frames,
        out_ctx: None,
        out_dims: None,
        encoder: rsmpeg::codec::RawVideoEncoder::new(),
        frames_written: 0,
    };

    while !state.at_limit() {
        let Some(pkt) = in_ctx
            .read_frame()
            .map_err(|e| format!("error reading input packet: {}", e))?
        else {
            break;
        };
        if pkt.stream_index != stream_idx {
            continue;
        }
        let frames = decoder
            .decode(&pkt)
            .map_err(|e| format!("video decode error: {}", e))?;
        for frame in frames {
            state.write(frame)?;
            if state.at_limit() {
                break;
            }
        }
    }

    if !state.at_limit() {
        let flushed = decoder
            .flush()
            .map_err(|e| format!("video decoder flush error: {}", e))?;
        for frame in flushed {
            state.write(frame)?;
            if state.at_limit() {
                break;
            }
        }
    }

    let Some(mut ctx) = state.out_ctx else {
        return Err(format!("no video frames decoded from '{}'", input));
    };

    for pkt in rsmpeg::codec::Encoder::flush(&mut state.encoder)
        .map_err(|e| format!("raw video encoder flush error: {}", e))?
    {
        ctx.write_frame(&pkt)
            .map_err(|e| format!("failed to write Y4M packet: {}", e))?;
    }
    ctx.write_trailer()
        .map_err(|e| format!("failed to write Y4M trailer to '{}': {}", output, e))?;

    let (width, height, _) = state.out_dims.unwrap_or((0, 0, rsmpeg::PixelFormat::None));
    let frames_written = state.frames_written;
    Ok(TranscodeSummary {
        input_format,
        output_format: "y4m".to_string(),
        frames_written,
        audio_samples: 0,
        sample_rate: 0,
        channels: 0,
        width,
        height,
    })
}

fn cmd_transcode(input: &str, output: &str, opts: &TranscodeOptions) {
    match transcode_media(input, output, opts) {
        Ok(summary) => {
            if summary.output_format == "wav" {
                println!(
                    "Transcoded {} ({}) -> {} ({}): {} frames, {} samples ({} Hz, {} ch)",
                    input,
                    summary.input_format,
                    output,
                    summary.output_format,
                    summary.frames_written,
                    summary.audio_samples,
                    summary.sample_rate,
                    summary.channels,
                );
            } else {
                println!(
                    "Transcoded {} ({}) -> {} ({}): {} frames ({}x{})",
                    input,
                    summary.input_format,
                    output,
                    summary.output_format,
                    summary.frames_written,
                    summary.width,
                    summary.height,
                );
            }
        }
        Err(e) => {
            eprintln!("Error transcoding '{}' -> '{}': {}", input, output, e);
            std::process::exit(1);
        }
    }
}

fn cmd_play(input: &str, info_only: bool) {
    println!();
    println!("═══ Playing Media ═══");
    println!("  File: {}", input);

    // Show file info using rsmpeg's probe
    match rsmpeg::format::FormatContext::open_input(input) {
        Ok(mut ctx) => {
            let _ = ctx.read_header();
            for stream in &ctx.streams {
                let media_icon = match stream.media_type {
                    rsmpeg::util::MediaType::Video => "V",
                    rsmpeg::util::MediaType::Audio => "A",
                    _ => "?",
                };
                println!(
                    "  [{}] Stream #{}: {:?} ({:?})",
                    media_icon, stream.index, stream.codec_id, stream.media_type
                );
            }
        }
        Err(_) => {
            println!("  (Note: file not probed by rsmpeg, Symphonia will handle detection)");
        }
    }

    if info_only {
        return;
    }

    // Use Symphonia + rodio for audio, OpenH264 + minifb for video
    match playback::play_media(input) {
        Ok(()) => {
            println!("  Playback complete.");
        }
        Err(e) => {
            eprintln!("  Playback error: {}", e);
            std::process::exit(1);
        }
    }
}

/// Launch the AcmeUI Native GUI player.
fn cmd_gui(input: Option<&str>) {
    match gui::run_gui(input) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("GUI error: {}", e);
            std::process::exit(1);
        }
    }
}

fn format_subtitle_duration(d: Duration) -> String {
    let total_secs = d.as_secs();
    let hours = total_secs / 3600;
    let mins = (total_secs % 3600) / 60;
    let secs = total_secs % 60;
    let millis = d.subsec_millis();
    format!("{:02}:{:02}:{:02}.{:03}", hours, mins, secs, millis)
}

fn cmd_decode_subtitle(input: &str, output: Option<&str>, max_frames: Option<usize>) {
    match rsmpeg::codec::parse_subtitle_file(Path::new(input)) {
        Ok(track) => {
            println!();
            println!("═══ Subtitle Decode ═══");
            println!("  File:   {}", input);
            println!("  Format: {}", track.format.name());
            println!("  Cues:   {}", track.cues.len());
            let limit = max_frames.map_or(5, |m| m.min(5));
            for cue in track.cues.iter().take(limit) {
                println!(
                    "  [{} -> {}] {}",
                    format_subtitle_duration(cue.start),
                    format_subtitle_duration(cue.end),
                    cue.text.replace('\n', " ")
                );
            }
            if output.is_some() {
                eprintln!("  Note: --output is ignored for subtitle files");
            }
        }
        Err(e) => {
            eprintln!("Error parsing subtitle file '{}': {}", input, e);
            std::process::exit(1);
        }
    }
}

type VideoDecoderHandle = (usize, Box<dyn rsmpeg::codec::Decoder>);
type AudioDecoderHandle = (usize, u32, u16, Box<dyn rsmpeg::codec::Decoder>);

fn init_pure_rust_video_decoder(
    streams: &[rsmpeg::Stream],
) -> Result<Option<VideoDecoderHandle>, String> {
    let Some(stream) = streams.iter().find(|s| {
        matches!(
            s.codec_id,
            rsmpeg::CodecId::Mjpeg | rsmpeg::CodecId::RawVideo
        )
    }) else {
        return Ok(None);
    };

    let dec: Box<dyn rsmpeg::codec::Decoder> = match stream.codec_id {
        rsmpeg::CodecId::Mjpeg => Box::new(
            rsmpeg::codec::MjpegDecoder::from_params(&stream.codec_params)
                .map_err(|e| format!("failed to initialize MJPEG decoder: {}", e))?,
        ),
        rsmpeg::CodecId::RawVideo => {
            let mut d = rsmpeg::codec::RawVideoDecoder::new();
            d.set_parameters(stream.codec_params.clone());
            Box::new(d)
        }
        _ => unreachable!(),
    };
    Ok(Some((stream.index, dec)))
}

fn init_pure_rust_audio_decoder(
    streams: &[rsmpeg::Stream],
) -> Result<Option<AudioDecoderHandle>, String> {
    let Some(stream) = streams
        .iter()
        .find(|s| matches!(s.codec_id, rsmpeg::CodecId::Flac | rsmpeg::CodecId::Pcm))
    else {
        return Ok(None);
    };

    let dec: Box<dyn rsmpeg::codec::Decoder> = match stream.codec_id {
        rsmpeg::CodecId::Flac => Box::new(
            rsmpeg::codec::FlacDecoder::from_params(&stream.codec_params)
                .map_err(|e| format!("failed to initialize FLAC decoder: {}", e))?,
        ),
        rsmpeg::CodecId::Pcm => Box::new(
            rsmpeg::codec::PCMAudioDecoder::from_params(&stream.codec_params)
                .map_err(|e| format!("failed to initialize PCM decoder: {}", e))?,
        ),
        _ => unreachable!(),
    };

    let sample_rate = stream.codec_params.sample_rate.unwrap_or(0);
    let channels = stream.codec_params.channels.unwrap_or(0);
    Ok(Some((stream.index, sample_rate, channels, dec)))
}

/// Run `frame` through [`rsmpeg::scale::Scaler`] at its native size and return
/// the tightly packed `dst` pixels (`bytes_per_pixel` per pixel, no row padding).
fn scale_frame_to_packed(
    frame: &rsmpeg::Frame,
    dst: rsmpeg::PixelFormat,
    bytes_per_pixel: usize,
) -> Option<Vec<u8>> {
    let w = u32::try_from(frame.width).ok()?;
    let h = u32::try_from(frame.height).ok()?;
    let config = rsmpeg::scale::ScalerConfig::new(w, h, frame.pixel_format, w, h, dst);
    let scaler = rsmpeg::scale::Scaler::new(config).ok()?;
    let out = scaler.scale(frame).ok()?;

    let row_bytes = frame.width.checked_mul(bytes_per_pixel)?;
    let plane = out.data.first()?;
    let stride = out.linesize.first().copied().unwrap_or(row_bytes);
    if stride < row_bytes {
        return None;
    }
    let mut packed = Vec::with_capacity(row_bytes.checked_mul(frame.height)?);
    for y in 0..frame.height {
        let start = y.checked_mul(stride)?;
        packed.extend_from_slice(plane.get(start..start.checked_add(row_bytes)?)?);
    }
    Some(packed)
}

fn frame_to_rgb24(frame: &rsmpeg::Frame) -> Option<Vec<u8>> {
    if frame.width == 0 || frame.height == 0 || frame.data.is_empty() {
        return None;
    }
    match frame.pixel_format {
        rsmpeg::PixelFormat::Yuv420P
        | rsmpeg::PixelFormat::Yuv422P
        | rsmpeg::PixelFormat::Yuv444P
        | rsmpeg::PixelFormat::Nv12
        | rsmpeg::PixelFormat::Nv21 => scale_frame_to_packed(frame, rsmpeg::PixelFormat::Rgb24, 3),
        _ => packed_frame_to_rgb24(frame),
    }
}

/// Convert an already-packed RGB24/BGR24/RGBA/BGRA/Gray8 frame, honouring
/// `linesize` padding.
fn packed_frame_to_rgb24(frame: &rsmpeg::Frame) -> Option<Vec<u8>> {
    let w = frame.width;
    let h = frame.height;
    let plane = &frame.data[0];
    let row_bytes = match frame.pixel_format {
        rsmpeg::PixelFormat::Rgb24 | rsmpeg::PixelFormat::Bgr24 => w.checked_mul(3)?,
        rsmpeg::PixelFormat::Rgba | rsmpeg::PixelFormat::Bgra => w.checked_mul(4)?,
        rsmpeg::PixelFormat::Gray8 => w,
        _ => return None,
    };
    let stride = frame.linesize.first().copied().unwrap_or(row_bytes);
    if stride < row_bytes {
        return None;
    }
    let needed = h
        .checked_sub(1)?
        .checked_mul(stride)?
        .checked_add(row_bytes)?;
    if plane.len() < needed {
        return None;
    }

    let mut rgb = Vec::with_capacity(w.checked_mul(h)?.checked_mul(3)?);
    match frame.pixel_format {
        rsmpeg::PixelFormat::Rgb24 => {
            if stride == row_bytes {
                rgb.extend_from_slice(&plane[..w * h * 3]);
            } else {
                for y in 0..h {
                    let row_start = y * stride;
                    rgb.extend_from_slice(&plane[row_start..row_start + w * 3]);
                }
            }
        }
        rsmpeg::PixelFormat::Bgr24 => {
            for y in 0..h {
                let row = &plane[y * stride..y * stride + w * 3];
                for px in row.chunks_exact(3) {
                    rgb.push(px[2]);
                    rgb.push(px[1]);
                    rgb.push(px[0]);
                }
            }
        }
        rsmpeg::PixelFormat::Rgba => {
            for y in 0..h {
                let row = &plane[y * stride..y * stride + w * 4];
                for px in row.chunks_exact(4) {
                    rgb.push(px[0]);
                    rgb.push(px[1]);
                    rgb.push(px[2]);
                }
            }
        }
        rsmpeg::PixelFormat::Bgra => {
            for y in 0..h {
                let row = &plane[y * stride..y * stride + w * 4];
                for px in row.chunks_exact(4) {
                    rgb.push(px[2]);
                    rgb.push(px[1]);
                    rgb.push(px[0]);
                }
            }
        }
        rsmpeg::PixelFormat::Gray8 => {
            for y in 0..h {
                let row = &plane[y * stride..y * stride + w];
                for &gray in row {
                    rgb.push(gray);
                    rgb.push(gray);
                    rgb.push(gray);
                }
            }
        }
        _ => return None,
    }
    Some(rgb)
}

fn write_ppm_to<W: Write>(
    writer: &mut W,
    width: usize,
    height: usize,
    rgb24: &[u8],
) -> std::io::Result<()> {
    if width == 0 || height == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "PPM dimensions must be non-zero",
        ));
    }
    let expected = width
        .checked_mul(height)
        .and_then(|px| px.checked_mul(3))
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "PPM buffer size overflow")
        })?;
    if rgb24.len() < expected {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "PPM RGB24 buffer is smaller than width * height * 3",
        ));
    }
    write!(writer, "P6\n{} {}\n255\n", width, height)?;
    writer.write_all(&rgb24[..expected])
}

fn write_ppm_file(path: &Path, width: usize, height: usize, rgb24: &[u8]) -> std::io::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut writer = std::io::BufWriter::new(file);
    write_ppm_to(&mut writer, width, height, rgb24)?;
    writer.flush()
}

fn append_pcm_samples_i16(frame: &rsmpeg::Frame, out: &mut Vec<i16>) {
    let Some(plane) = frame.data.first() else {
        return;
    };
    let expected = frame.samples.saturating_mul(usize::from(frame.channels));
    let max_items = if expected > 0 { expected } else { usize::MAX };
    match frame.sample_format {
        rsmpeg::SampleFormat::S16 => {
            for chunk in plane.chunks_exact(2).take(max_items) {
                out.push(i16::from_le_bytes([chunk[0], chunk[1]]));
            }
        }
        rsmpeg::SampleFormat::S32 => {
            for chunk in plane.chunks_exact(4).take(max_items) {
                let sample = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                out.push((sample >> 16) as i16);
            }
        }
        rsmpeg::SampleFormat::U8 => {
            for &b in plane.iter().take(max_items) {
                out.push((i16::from(b) - 128) << 8);
            }
        }
        rsmpeg::SampleFormat::F32 => {
            for chunk in plane.chunks_exact(4).take(max_items) {
                let sample = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                let clamped = if sample.is_finite() {
                    sample.clamp(-1.0, 1.0)
                } else {
                    0.0
                };
                out.push((clamped * 32767.0) as i16);
            }
        }
        _ => {}
    }
}

fn write_wav_to<W: Write>(
    writer: &mut W,
    sample_rate: u32,
    channels: u16,
    samples: &[i16],
) -> std::io::Result<()> {
    if sample_rate == 0 || channels == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "WAV sample_rate and channels must be non-zero",
        ));
    }
    let bits_per_sample: u16 = 16;
    let block_align = channels.checked_mul(bits_per_sample / 8).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "WAV block_align overflow")
    })?;
    let byte_rate = sample_rate
        .checked_mul(u32::from(block_align))
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "WAV byte_rate overflow")
        })?;
    let data_bytes_usize = samples.len().checked_mul(2).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "WAV data size overflow")
    })?;
    let data_size = u32::try_from(data_bytes_usize).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "WAV payload exceeds 4 GiB RIFF limit",
        )
    })?;
    let riff_size = 36u32.checked_add(data_size).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "WAV RIFF chunk exceeds 4 GiB limit",
        )
    })?;

    writer.write_all(b"RIFF")?;
    writer.write_all(&riff_size.to_le_bytes())?;
    writer.write_all(b"WAVE")?;
    writer.write_all(b"fmt ")?;
    writer.write_all(&16u32.to_le_bytes())?;
    writer.write_all(&1u16.to_le_bytes())?;
    writer.write_all(&channels.to_le_bytes())?;
    writer.write_all(&sample_rate.to_le_bytes())?;
    writer.write_all(&byte_rate.to_le_bytes())?;
    writer.write_all(&block_align.to_le_bytes())?;
    writer.write_all(&bits_per_sample.to_le_bytes())?;
    writer.write_all(b"data")?;
    writer.write_all(&data_size.to_le_bytes())?;
    for &sample in samples {
        writer.write_all(&sample.to_le_bytes())?;
    }
    Ok(())
}

fn write_wav_file(
    path: &Path,
    sample_rate: u32,
    channels: u16,
    samples: &[i16],
) -> std::io::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut writer = std::io::BufWriter::new(file);
    write_wav_to(&mut writer, sample_rate, channels, samples)?;
    writer.flush()
}

struct DecodeStats {
    video_frames: usize,
    audio_frames: usize,
    total_audio_samples: usize,
    audio_sample_rate: u32,
    audio_channels: u16,
    pcm_samples: Vec<i16>,
    ppm_written: bool,
    wrote_path: Option<String>,
    want_ppm: bool,
    want_wav: bool,
    output: Option<String>,
    max_frames: Option<usize>,
}

impl DecodeStats {
    fn at_limit(&self) -> bool {
        self.max_frames
            .is_some_and(|limit| self.video_frames + self.audio_frames >= limit)
    }

    fn handle_video_frames(&mut self, frames: &[rsmpeg::Frame]) {
        for frame in frames {
            self.handle_video_frame(frame);
            if self.at_limit() {
                break;
            }
        }
    }

    fn handle_video_frame(&mut self, frame: &rsmpeg::Frame) {
        if self.at_limit() {
            return;
        }
        self.video_frames += 1;
        if self.video_frames == 1 {
            println!(
                "  Video:  {}x{} ({})",
                frame.width,
                frame.height,
                frame.pixel_format.name()
            );
        }
        if self.want_ppm && !self.ppm_written {
            if let (Some(ref out_path), Some(rgb)) = (&self.output, frame_to_rgb24(frame)) {
                if let Err(e) = write_ppm_file(Path::new(out_path), frame.width, frame.height, &rgb)
                {
                    eprintln!("Error writing PPM to '{}': {}", out_path, e);
                    std::process::exit(1);
                }
                self.ppm_written = true;
                self.wrote_path = Some(out_path.clone());
            }
        }
    }

    fn handle_audio_frames(&mut self, frames: &[rsmpeg::Frame]) {
        for frame in frames {
            self.handle_audio_frame(frame);
            if self.at_limit() {
                break;
            }
        }
    }

    fn handle_audio_frame(&mut self, frame: &rsmpeg::Frame) {
        if self.at_limit() {
            return;
        }
        self.audio_frames += 1;
        self.total_audio_samples += frame.samples;
        if frame.sample_rate > 0 {
            self.audio_sample_rate = frame.sample_rate;
        }
        if frame.channels > 0 {
            self.audio_channels = frame.channels;
        }
        if self.want_wav {
            append_pcm_samples_i16(frame, &mut self.pcm_samples);
        }
    }
}

fn cmd_decode(input: &str, output: Option<&str>, max_frames: Option<usize>) {
    let is_subtitle = Path::new(input)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("srt") || ext.eq_ignore_ascii_case("vtt"));
    if is_subtitle {
        cmd_decode_subtitle(input, output, max_frames);
        return;
    }

    let mut ctx = match rsmpeg::format::FormatContext::open_input(input) {
        Ok(ctx) => ctx,
        Err(e) => {
            eprintln!("Error opening input '{}': {}", input, e);
            std::process::exit(1);
        }
    };
    if let Err(e) = ctx.read_header() {
        eprintln!("Error reading header for '{}': {}", input, e);
        std::process::exit(1);
    }

    let mut video_dec = match init_pure_rust_video_decoder(&ctx.streams) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    };
    let mut audio_dec = match init_pure_rust_audio_decoder(&ctx.streams) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    };

    if video_dec.is_none() && audio_dec.is_none() {
        let detected: Vec<String> = ctx
            .streams
            .iter()
            .map(|s| format!("#{} {} ({:?})", s.index, s.codec_id.name(), s.media_type))
            .collect();
        let summary = if detected.is_empty() {
            "no streams detected".to_string()
        } else {
            detected.join(", ")
        };
        eprintln!(
            "Error: no pure-Rust decodable stream found in '{}' (supported: flac, pcm, mjpeg, rawvideo; detected: {})",
            input, summary
        );
        std::process::exit(1);
    }

    println!();
    println!("═══ Pure-Rust Decode ═══");
    println!(
        "  File:   {} ({})",
        input,
        ctx.format_name.as_deref().unwrap_or("unknown")
    );

    let out_lower = output.map(|p| p.to_ascii_lowercase());
    let want_ppm = out_lower
        .as_deref()
        .is_some_and(|p| p.ends_with(".ppm") || (!p.ends_with(".wav") && audio_dec.is_none()));
    let want_wav = out_lower
        .as_deref()
        .is_some_and(|p| p.ends_with(".wav") || (!p.ends_with(".ppm") && video_dec.is_none()));

    let initial_sr = audio_dec.as_ref().map_or(0, |a| a.1);
    let initial_ch = audio_dec.as_ref().map_or(0, |a| a.2);
    let mut stats = DecodeStats {
        video_frames: 0,
        audio_frames: 0,
        total_audio_samples: 0,
        audio_sample_rate: initial_sr,
        audio_channels: initial_ch,
        pcm_samples: Vec::new(),
        ppm_written: false,
        wrote_path: None,
        want_ppm,
        want_wav,
        output: output.map(ToOwned::to_owned),
        max_frames,
    };

    while !stats.at_limit() {
        let pkt = match ctx.read_frame() {
            Ok(Some(pkt)) => pkt,
            Ok(None) => break,
            Err(e) => {
                eprintln!("Error reading frame: {}", e);
                std::process::exit(1);
            }
        };

        if let Some((v_idx, ref mut dec)) = video_dec {
            if pkt.stream_index == v_idx {
                match dec.decode(&pkt) {
                    Ok(frames) => stats.handle_video_frames(&frames),
                    Err(e) => {
                        eprintln!("Error decoding video packet: {}", e);
                        std::process::exit(1);
                    }
                }
                continue;
            }
        }

        if let Some((a_idx, _, _, ref mut dec)) = audio_dec {
            if pkt.stream_index == a_idx {
                match dec.decode(&pkt) {
                    Ok(frames) => stats.handle_audio_frames(&frames),
                    Err(e) => {
                        eprintln!("Error decoding audio packet: {}", e);
                        std::process::exit(1);
                    }
                }
            }
        }
    }

    if !stats.at_limit() {
        if let Some((_, ref mut dec)) = video_dec {
            match dec.flush() {
                Ok(frames) => stats.handle_video_frames(&frames),
                Err(e) => {
                    eprintln!("Error flushing video decoder: {}", e);
                    std::process::exit(1);
                }
            }
        }
    }

    if !stats.at_limit() {
        if let Some((_, _, _, ref mut dec)) = audio_dec {
            match dec.flush() {
                Ok(frames) => stats.handle_audio_frames(&frames),
                Err(e) => {
                    eprintln!("Error flushing audio decoder: {}", e);
                    std::process::exit(1);
                }
            }
        }
    }

    if stats.want_wav
        && stats.audio_frames > 0
        && stats.audio_sample_rate > 0
        && stats.audio_channels > 0
    {
        if let Some(ref out_path) = stats.output {
            if let Err(e) = write_wav_file(
                Path::new(out_path),
                stats.audio_sample_rate,
                stats.audio_channels,
                &stats.pcm_samples,
            ) {
                eprintln!("Error writing WAV to '{}': {}", out_path, e);
                std::process::exit(1);
            }
            stats.wrote_path = Some(out_path.clone());
        }
    }

    if let Some(ref written) = stats.wrote_path {
        println!(
            "  Decoded {} video frames, {} audio frames ({} samples at {} Hz, {} ch) -> wrote {}",
            stats.video_frames,
            stats.audio_frames,
            stats.total_audio_samples,
            stats.audio_sample_rate,
            stats.audio_channels,
            written
        );
    } else {
        println!(
            "  Decoded {} video frames, {} audio frames ({} samples at {} Hz, {} ch)",
            stats.video_frames,
            stats.audio_frames,
            stats.total_audio_samples,
            stats.audio_sample_rate,
            stats.audio_channels
        );
        if let Some(out_path) = output {
            eprintln!(
                "  Warning: no matching decoded output written to '{}'",
                out_path
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_wav_writes_standard_16bit_pcm_header_and_samples() {
        let samples = [0i16, 1024, -2048, i16::MAX];
        let mut buf = Vec::new();
        write_wav_to(&mut buf, 48_000, 2, &samples).expect("wav write should succeed");

        assert_eq!(buf.len(), 44 + samples.len() * 2);
        assert_eq!(&buf[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]), 36 + 8);
        assert_eq!(&buf[8..12], b"WAVE");
        assert_eq!(&buf[12..16], b"fmt ");
        assert_eq!(u32::from_le_bytes([buf[16], buf[17], buf[18], buf[19]]), 16);
        assert_eq!(u16::from_le_bytes([buf[20], buf[21]]), 1);
        assert_eq!(u16::from_le_bytes([buf[22], buf[23]]), 2);
        assert_eq!(
            u32::from_le_bytes([buf[24], buf[25], buf[26], buf[27]]),
            48_000
        );
        assert_eq!(
            u32::from_le_bytes([buf[28], buf[29], buf[30], buf[31]]),
            48_000 * 4
        );
        assert_eq!(u16::from_le_bytes([buf[32], buf[33]]), 4);
        assert_eq!(u16::from_le_bytes([buf[34], buf[35]]), 16);
        assert_eq!(&buf[36..40], b"data");
        assert_eq!(u32::from_le_bytes([buf[40], buf[41], buf[42], buf[43]]), 8);

        let decoded: Vec<i16> = buf[44..]
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(decoded, samples);
    }

    #[test]
    fn write_wav_rejects_zero_rate_or_channels() {
        let mut buf = Vec::new();
        assert!(write_wav_to(&mut buf, 0, 2, &[0]).is_err());
        assert!(write_wav_to(&mut buf, 44_100, 0, &[0]).is_err());
    }

    #[test]
    fn write_ppm_writes_valid_p6_header_and_rgb_bytes() {
        let rgb = [255u8, 0, 0, 0, 255, 0];
        let mut buf = Vec::new();
        write_ppm_to(&mut buf, 2, 1, &rgb).expect("ppm write should succeed");

        let mut expected = b"P6\n2 1\n255\n".to_vec();
        expected.extend_from_slice(&rgb);
        assert_eq!(buf, expected);
    }

    #[test]
    fn write_ppm_rejects_short_buffer_or_zero_dimensions() {
        let mut buf = Vec::new();
        assert!(write_ppm_to(&mut buf, 2, 2, &[0u8; 6]).is_err());
        assert!(write_ppm_to(&mut buf, 0, 2, &[]).is_err());
        assert!(write_ppm_to(&mut buf, 2, 0, &[]).is_err());
    }

    #[test]
    fn append_pcm_samples_converts_u8_s16_and_s32() {
        let mut f_u8 = rsmpeg::Frame::new_audio(rsmpeg::SampleFormat::U8, 8000, 1, 3);
        f_u8.data[0] = vec![0, 128, 255];
        let mut out = Vec::new();
        append_pcm_samples_i16(&f_u8, &mut out);
        assert_eq!(out, vec![-32768, 0, 32512]);

        let mut f_s16 = rsmpeg::Frame::new_audio(rsmpeg::SampleFormat::S16, 44100, 1, 2);
        f_s16.data[0].clear();
        f_s16.data[0].extend_from_slice(&(-1234i16).to_le_bytes());
        f_s16.data[0].extend_from_slice(&5678i16.to_le_bytes());
        out.clear();
        append_pcm_samples_i16(&f_s16, &mut out);
        assert_eq!(out, vec![-1234, 5678]);

        let mut f_s32 = rsmpeg::Frame::new_audio(rsmpeg::SampleFormat::S32, 48000, 1, 2);
        f_s32.data[0].clear();
        f_s32.data[0].extend_from_slice(&0x1234_0000i32.to_le_bytes());
        f_s32.data[0].extend_from_slice(&(-0x0001_0000i32).to_le_bytes());
        out.clear();
        append_pcm_samples_i16(&f_s32, &mut out);
        assert_eq!(out, vec![0x1234, -1]);
    }

    #[test]
    fn frame_to_rgb24_handles_rgb24_rgba_and_gray8() {
        let mut f_rgb = rsmpeg::Frame::new_video(2, 1, rsmpeg::PixelFormat::Rgb24);
        f_rgb.data[0] = vec![1, 2, 3, 4, 5, 6];
        assert_eq!(frame_to_rgb24(&f_rgb), Some(vec![1, 2, 3, 4, 5, 6]));

        let mut f_rgba = rsmpeg::Frame::new_video(2, 1, rsmpeg::PixelFormat::Rgba);
        f_rgba.data[0] = vec![10, 20, 30, 255, 40, 50, 60, 128];
        assert_eq!(frame_to_rgb24(&f_rgba), Some(vec![10, 20, 30, 40, 50, 60]));

        let mut f_gray = rsmpeg::Frame::new_video(2, 1, rsmpeg::PixelFormat::Gray8);
        f_gray.data[0] = vec![77, 200];
        assert_eq!(
            frame_to_rgb24(&f_gray),
            Some(vec![77, 77, 77, 200, 200, 200])
        );
    }

    /// Build a packed frame whose plane 0 carries filler bytes (`0xEE`) after
    /// every `row_bytes` of pixel data, so `linesize` padding must be skipped.
    fn padded_packed_frame(
        w: usize,
        h: usize,
        pix: rsmpeg::PixelFormat,
        row_bytes: usize,
        stride: usize,
        rows: &[&[u8]],
    ) -> rsmpeg::Frame {
        let mut frame = rsmpeg::Frame::new_video(w, h, pix);
        let mut plane = vec![0xEE; stride * h];
        for (y, row) in rows.iter().enumerate() {
            assert_eq!(row.len(), row_bytes);
            plane[y * stride..y * stride + row_bytes].copy_from_slice(row);
        }
        frame.data = vec![plane];
        frame.linesize = vec![stride];
        frame
    }

    #[test]
    fn frame_to_rgb24_packed_formats_ignore_padded_linesize() {
        let expected = Some(vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);

        let rgb = padded_packed_frame(
            2,
            2,
            rsmpeg::PixelFormat::Rgb24,
            6,
            10,
            &[&[1, 2, 3, 4, 5, 6], &[7, 8, 9, 10, 11, 12]],
        );
        assert_eq!(frame_to_rgb24(&rgb), expected);

        let bgr = padded_packed_frame(
            2,
            2,
            rsmpeg::PixelFormat::Bgr24,
            6,
            10,
            &[&[3, 2, 1, 6, 5, 4], &[9, 8, 7, 12, 11, 10]],
        );
        assert_eq!(frame_to_rgb24(&bgr), expected);

        let rgba = padded_packed_frame(
            2,
            2,
            rsmpeg::PixelFormat::Rgba,
            8,
            12,
            &[&[1, 2, 3, 255, 4, 5, 6, 255], &[7, 8, 9, 0, 10, 11, 12, 0]],
        );
        assert_eq!(frame_to_rgb24(&rgba), expected);

        let bgra = padded_packed_frame(
            2,
            2,
            rsmpeg::PixelFormat::Bgra,
            8,
            12,
            &[&[3, 2, 1, 255, 6, 5, 4, 255], &[9, 8, 7, 0, 12, 11, 10, 0]],
        );
        assert_eq!(frame_to_rgb24(&bgra), expected);

        let gray = padded_packed_frame(
            2,
            2,
            rsmpeg::PixelFormat::Gray8,
            2,
            5,
            &[&[10, 20], &[30, 40]],
        );
        assert_eq!(
            frame_to_rgb24(&gray),
            Some(vec![10, 10, 10, 20, 20, 20, 30, 30, 30, 40, 40, 40])
        );
    }

    /// Build a 2×2 YUV frame with explicit plane buffers and strides.
    fn yuv_frame(
        pix: rsmpeg::PixelFormat,
        planes: Vec<Vec<u8>>,
        linesize: Vec<usize>,
    ) -> rsmpeg::Frame {
        let mut frame = rsmpeg::Frame::new_video(2, 2, pix);
        frame.data = planes;
        frame.linesize = linesize;
        frame
    }

    #[test]
    fn frame_to_rgb24_converts_planar_and_semi_planar_yuv_via_scaler() {
        // Limited-range Y 16 is black and Y 235 is white; neutral chroma (128)
        // keeps every pixel on the grey axis.
        let black_white = vec![0, 0, 0, 255, 255, 255, 0, 0, 0, 255, 255, 255];
        let y_plane = vec![16, 235, 16, 235];

        let yuv420 = yuv_frame(
            rsmpeg::PixelFormat::Yuv420P,
            vec![y_plane.clone(), vec![128], vec![128]],
            vec![2, 1, 1],
        );
        assert_eq!(frame_to_rgb24(&yuv420), Some(black_white.clone()));

        // Padded strides: Y has 2 filler bytes per row, chroma has 2 per row.
        let yuv420_padded = yuv_frame(
            rsmpeg::PixelFormat::Yuv420P,
            vec![
                vec![16, 235, 0xEE, 0xEE, 16, 235, 0xEE, 0xEE],
                vec![128, 0xEE, 0xEE],
                vec![128, 0xEE, 0xEE],
            ],
            vec![4, 3, 3],
        );
        assert_eq!(frame_to_rgb24(&yuv420_padded), Some(black_white.clone()));

        let nv12 = yuv_frame(
            rsmpeg::PixelFormat::Nv12,
            vec![y_plane.clone(), vec![128, 128]],
            vec![2, 2],
        );
        assert_eq!(frame_to_rgb24(&nv12), Some(black_white.clone()));

        let nv21 = yuv_frame(
            rsmpeg::PixelFormat::Nv21,
            vec![y_plane.clone(), vec![128, 128]],
            vec![2, 2],
        );
        assert_eq!(frame_to_rgb24(&nv21), Some(black_white.clone()));

        let yuv422 = yuv_frame(
            rsmpeg::PixelFormat::Yuv422P,
            vec![y_plane.clone(), vec![128, 128], vec![128, 128]],
            vec![2, 1, 1],
        );
        assert_eq!(frame_to_rgb24(&yuv422), Some(black_white.clone()));

        let yuv444 = yuv_frame(
            rsmpeg::PixelFormat::Yuv444P,
            vec![y_plane, vec![128; 4], vec![128; 4]],
            vec![2, 2, 2],
        );
        assert_eq!(frame_to_rgb24(&yuv444), Some(black_white));
    }

    #[test]
    fn append_pcm_samples_f32_clamps_and_maps_non_finite_to_zero() {
        let samples = [-1.0f32, 0.0, 1.0, 2.5, -2.5, f32::NAN, f32::INFINITY];
        let mut frame =
            rsmpeg::Frame::new_audio(rsmpeg::SampleFormat::F32, 48_000, 1, samples.len());
        frame.data[0].clear();
        for s in samples {
            frame.data[0].extend_from_slice(&s.to_le_bytes());
        }

        let mut out = Vec::new();
        append_pcm_samples_i16(&frame, &mut out);
        assert_eq!(out, vec![-32767, 0, 32767, 32767, -32767, 0, 0]);
    }

    fn empty_decode_stats(max_frames: Option<usize>) -> DecodeStats {
        DecodeStats {
            video_frames: 0,
            audio_frames: 0,
            total_audio_samples: 0,
            audio_sample_rate: 0,
            audio_channels: 0,
            pcm_samples: Vec::new(),
            ppm_written: false,
            wrote_path: None,
            want_ppm: false,
            want_wav: false,
            output: None,
            max_frames,
        }
    }

    fn stereo_audio_frame() -> rsmpeg::Frame {
        rsmpeg::Frame::new_audio(rsmpeg::SampleFormat::S16, 48_000, 2, 4)
    }

    fn tiny_video_frame() -> rsmpeg::Frame {
        rsmpeg::Frame::new_video(2, 2, rsmpeg::PixelFormat::Rgb24)
    }

    #[test]
    fn decode_stats_stops_counting_once_max_frames_reached() {
        let mut stats = empty_decode_stats(Some(3));
        stats.handle_audio_frames(&[stereo_audio_frame(), stereo_audio_frame()]);
        assert!(!stats.at_limit());
        assert_eq!(stats.audio_frames, 2);

        // Only one of the two video frames fits under the limit of 3.
        stats.handle_video_frames(&[tiny_video_frame(), tiny_video_frame()]);
        assert_eq!(stats.video_frames, 1);
        assert_eq!(stats.audio_frames, 2);
        assert!(stats.at_limit());
        assert_eq!(stats.total_audio_samples, 8);

        // Once at the limit, further frames of either kind are ignored.
        stats.handle_audio_frames(&[stereo_audio_frame()]);
        stats.handle_video_frame(&tiny_video_frame());
        assert_eq!(stats.video_frames, 1);
        assert_eq!(stats.audio_frames, 2);
        assert_eq!(stats.total_audio_samples, 8);
    }

    #[test]
    fn decode_stats_without_limit_counts_every_frame() {
        let mut stats = empty_decode_stats(None);
        stats.handle_audio_frames(&[
            stereo_audio_frame(),
            stereo_audio_frame(),
            stereo_audio_frame(),
        ]);
        assert!(!stats.at_limit());
        assert_eq!(stats.audio_frames, 3);
        assert_eq!(stats.total_audio_samples, 12);
    }

    fn unique_temp_path(stem: &str, ext: &str) -> std::path::PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "rsmpeg_cli_{stem}_{}_{nanos}_{id}.{ext}",
            std::process::id()
        ))
    }

    #[test]
    fn transcode_wav_resamples_rate_and_channels_roundtrip() {
        rsmpeg::format::format_registry::register_builtin_formats();
        rsmpeg::codec::codec_registry::register_builtin_codecs();

        let in_path = unique_temp_path("in_audio", "wav");
        let out_path = unique_temp_path("out_audio", "wav");

        // 441 stereo S16 frames at 44.1 kHz (10 ms) -> exact 480 mono frames at 48 kHz.
        let src_frames = 441usize;
        let mut samples = Vec::with_capacity(src_frames * 2);
        for i in 0..src_frames {
            let v = ((i as i32 * 73) % 16000) as i16;
            samples.push(v);
            samples.push(-v / 2);
        }
        write_wav_file(&in_path, 44_100, 2, &samples).expect("write input wav");

        let summary = transcode_media(
            &in_path.to_string_lossy(),
            &out_path.to_string_lossy(),
            &TranscodeOptions {
                acodec: Some("pcm_s16le".to_string()),
                sample_rate: Some(48_000),
                channels: Some(1),
                ..Default::default()
            },
        )
        .expect("wav transcode should succeed");

        assert_eq!(summary.input_format, "wav");
        assert_eq!(summary.output_format, "wav");
        assert_eq!(summary.sample_rate, 48_000);
        assert_eq!(summary.channels, 1);
        assert_eq!(summary.audio_samples, 480);

        let mut probe = rsmpeg::format::FormatContext::open_input(&out_path).expect("open out wav");
        assert_eq!(probe.format_name.as_deref(), Some("wav"));
        probe.read_header().expect("read out wav header");
        assert_eq!(probe.streams.len(), 1);
        let stream = &probe.streams[0];
        assert_eq!(stream.codec_id, rsmpeg::CodecId::Pcm);
        assert_eq!(stream.codec_params.sample_rate, Some(48_000));
        assert_eq!(stream.codec_params.channels, Some(1));
        assert_eq!(
            stream.codec_params.sample_format,
            Some(rsmpeg::SampleFormat::S16)
        );

        let mut dec =
            rsmpeg::codec::PCMAudioDecoder::from_params(&stream.codec_params).expect("pcm decoder");
        let mut decoded_samples = 0usize;
        let mut any_nonzero = false;
        while let Some(pkt) = probe.read_frame().expect("read out wav packet") {
            for frame in rsmpeg::codec::Decoder::decode(&mut dec, &pkt).expect("decode out wav") {
                decoded_samples += frame.samples;
                if frame
                    .data
                    .first()
                    .is_some_and(|d| d.iter().any(|&b| b != 0))
                {
                    any_nonzero = true;
                }
            }
        }
        assert_eq!(decoded_samples, 480);
        assert!(any_nonzero, "resampled WAV should carry non-silent signal");

        let _ = std::fs::remove_file(&in_path);
        let _ = std::fs::remove_file(&out_path);
    }

    #[test]
    fn transcode_y4m_scales_yuv420p_roundtrip() {
        rsmpeg::format::format_registry::register_builtin_formats();
        rsmpeg::codec::codec_registry::register_builtin_codecs();

        let in_path = unique_temp_path("in_video", "y4m");
        let out_path = unique_temp_path("out_video", "y4m");

        // Write a 2-frame 8x8 Yuv420P .y4m input via FormatContext + Y4mMuxer.
        {
            let mut mux =
                rsmpeg::format::FormatContext::open_output(&in_path).expect("open in y4m");
            let mut stream = rsmpeg::format::Stream::new(0, rsmpeg::CodecId::RawVideo);
            stream.media_type = rsmpeg::util::MediaType::Video;
            stream.time_base = rsmpeg::Rational::new(1, 24);
            stream.codec_params.width = Some(8);
            stream.codec_params.height = Some(8);
            stream.codec_params.pixel_format = Some(rsmpeg::PixelFormat::Yuv420P);
            mux.add_stream(stream);
            mux.write_header().expect("write in y4m header");

            // 8x8 Yuv420P = 64 + 16 + 16 = 96 bytes per frame.
            let f0 = vec![16u8; 64]
                .into_iter()
                .chain(std::iter::repeat_n(128u8, 32))
                .collect::<Vec<u8>>();
            let f1 = vec![235u8; 64]
                .into_iter()
                .chain(std::iter::repeat_n(128u8, 32))
                .collect::<Vec<u8>>();
            mux.write_frame(&rsmpeg::Packet::new(f0.into(), 0))
                .expect("write f0");
            mux.write_frame(&rsmpeg::Packet::new(f1.into(), 0))
                .expect("write f1");
            mux.write_trailer().expect("write in y4m trailer");
        }

        let summary = transcode_media(
            &in_path.to_string_lossy(),
            &out_path.to_string_lossy(),
            &TranscodeOptions {
                vcodec: Some("rawvideo".to_string()),
                width: Some(16),
                height: Some(16),
                ..Default::default()
            },
        )
        .expect("y4m transcode should succeed");

        assert_eq!(summary.input_format, "y4m");
        assert_eq!(summary.output_format, "y4m");
        assert_eq!(summary.frames_written, 2);
        assert_eq!((summary.width, summary.height), (16, 16));

        let mut probe = rsmpeg::format::FormatContext::open_input(&out_path).expect("open out y4m");
        assert_eq!(probe.format_name.as_deref(), Some("y4m"));
        probe.read_header().expect("read out y4m header");
        assert_eq!(probe.streams.len(), 1);
        let stream = &probe.streams[0];
        assert_eq!(stream.codec_id, rsmpeg::CodecId::RawVideo);
        assert_eq!(stream.codec_params.width, Some(16));
        assert_eq!(stream.codec_params.height, Some(16));
        assert_eq!(
            stream.codec_params.pixel_format,
            Some(rsmpeg::PixelFormat::Yuv420P)
        );
        assert_eq!(stream.duration, 2);

        let mut dec = rsmpeg::codec::RawVideoDecoder::new();
        dec.set_parameters(stream.codec_params.clone());
        let mut decoded_frames = Vec::new();
        while let Some(pkt) = probe.read_frame().expect("read out y4m packet") {
            let frames = rsmpeg::codec::Decoder::decode(&mut dec, &pkt).expect("decode out y4m");
            decoded_frames.extend(frames);
        }
        assert_eq!(decoded_frames.len(), 2);
        assert_eq!(
            (decoded_frames[0].width, decoded_frames[0].height),
            (16, 16)
        );
        assert_eq!(decoded_frames[0].data[0].len(), 256);
        assert!(decoded_frames[0].data[0].iter().all(|&y| y == 16));
        assert!(decoded_frames[1].data[0].iter().all(|&y| y == 235));

        let _ = std::fs::remove_file(&in_path);
        let _ = std::fs::remove_file(&out_path);
    }

    #[test]
    fn packed_rgb_to_yuv420p_converts_black_and_white() {
        let mut rgb = rsmpeg::Frame::new_video(2, 2, rsmpeg::PixelFormat::Rgb24);
        // Top row black, bottom row white.
        rgb.data[0] = vec![0, 0, 0, 0, 0, 0, 255, 255, 255, 255, 255, 255];
        let yuv = packed_rgb_to_yuv420p(&rgb).expect("rgb to yuv420p");
        assert_eq!(yuv.pixel_format, rsmpeg::PixelFormat::Yuv420P);
        assert_eq!(yuv.data[0], vec![16, 16, 235, 235]);
        assert_eq!(yuv.data[1], vec![128]);
        assert_eq!(yuv.data[2], vec![128]);
    }

    #[test]
    fn transcode_media_rejects_unsupported_target_extension() {
        let err = transcode_media("input.wav", "output.mp4", &TranscodeOptions::default())
            .expect_err("mp4 output should be rejected");
        assert!(err.contains(".wav") && err.contains(".y4m"));
    }
}
