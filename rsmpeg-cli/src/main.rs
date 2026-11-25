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
        /// Output file path
        output: String,
        /// Video codec to use
        #[arg(long)]
        vcodec: Option<String>,
        /// Audio codec to use
        #[arg(long)]
        acodec: Option<String>,
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
    /// Launch the egui graphical media player
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
        } => {
            cmd_transcode(&input, &output, vcodec.as_deref(), acodec.as_deref());
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
                            "duration_ms": s.duration,
                            "duration_sec": s.duration_seconds(),
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
                            "     Duration:  {} ms ({:.3}s)",
                            stream.duration,
                            stream.duration_seconds()
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

fn cmd_transcode(input: &str, output: &str, _vcodec: Option<&str>, _acodec: Option<&str>) {
    match rsmpeg::format::FormatContext::open_input(input) {
        Ok(mut ctx) => {
            let _ = ctx.read_header();
            println!(
                "Input: {} ({}) — {} streams",
                ctx.filename.as_deref().unwrap_or("?"),
                ctx.format_name.as_deref().unwrap_or("?"),
                ctx.streams.len()
            );
            println!("Output: {}", output);
            println!("Note: Full transcoding pipeline not yet implemented.");
            println!(
                "This is a skeleton — actual frame processing will be added in a future release."
            );
        }
        Err(e) => {
            eprintln!("Error opening input: {}", e);
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

/// Launch the egui GUI player.
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

fn frame_to_rgb24(frame: &rsmpeg::Frame) -> Option<Vec<u8>> {
    let w = frame.width;
    let h = frame.height;
    if w == 0 || h == 0 || frame.data.is_empty() {
        return None;
    }
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
    let mut file = std::fs::File::create(path)?;
    write_ppm_to(&mut file, width, height, rgb24)
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
    let mut file = std::fs::File::create(path)?;
    write_wav_to(&mut file, sample_rate, channels, samples)
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
}
