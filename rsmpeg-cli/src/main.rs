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
    command: Commands,
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

    match &cli.command {
        Commands::Probe {
            input,
            json,
            verbose,
        } => cmd_probe(input, *json, *verbose),
        Commands::Transcode {
            input,
            output,
            vcodec,
            acodec,
        } => {
            cmd_transcode(input, output, vcodec.as_deref(), acodec.as_deref());
        }
        Commands::Play { input, info } => cmd_play(input, *info),
        Commands::ListFormats => cmd_list_formats(),
        Commands::ListCodecs => cmd_list_codecs(),
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
                println!("{}", serde_json::to_string_pretty(&info).unwrap());
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
                        if stream.codec_params.bit_rate.unwrap_or(0) > 0 {
                            println!(
                                "     Bitrate:   {} bps",
                                stream.codec_params.bit_rate.unwrap()
                            );
                        }
                        if stream.codec_params.width.unwrap_or(0) > 0 {
                            println!("     Width:     {}", stream.codec_params.width.unwrap());
                        }
                        if stream.codec_params.height.unwrap_or(0) > 0 {
                            println!("     Height:    {}", stream.codec_params.height.unwrap());
                        }
                        if stream.codec_params.sample_rate.unwrap_or(0) > 0 {
                            println!(
                                "     SampleRate: {} Hz",
                                stream.codec_params.sample_rate.unwrap()
                            );
                        }
                        if stream.codec_params.channels.unwrap_or(0) > 0 {
                            println!("     Channels:  {}", stream.codec_params.channels.unwrap());
                        }
                        if let Some(fmt) = stream.codec_params.sample_format {
                            println!("     SampleFmt: {:?}", fmt);
                        }
                        if stream.codec_params.pixel_format.is_some() {
                            println!(
                                "     PixelFmt:  {:?}",
                                stream.codec_params.pixel_format.unwrap()
                            );
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
