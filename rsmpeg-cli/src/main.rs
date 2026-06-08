use clap::{Parser, Subcommand};

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
    /// Play media file (basic playback)
    Play {
        /// Input file path
        input: String,
    },
}

fn main() {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    match &cli.command {
        Commands::Probe { input, json } => cmd_probe(input, *json),
        Commands::Transcode {
            input,
            output,
            vcodec,
            acodec,
        } => {
            cmd_transcode(input, output, vcodec.as_deref(), acodec.as_deref());
        }
        Commands::Play { input } => cmd_play(input),
    }
}

fn cmd_probe(input: &str, json: bool) {
    match rsmpeg::format::FormatContext::open_input(input) {
        Ok(ctx) => {
            if json {
                let info = serde_json::json!({
                    "filename": ctx.filename,
                    "format": ctx.format_name,
                    "nb_streams": ctx.streams.len(),
                    "duration": ctx.duration,
                    "streams": ctx.streams.iter().map(|s| serde_json::json!({
                        "index": s.index,
                        "codec": format!("{:?}", s.codec_id),
                        "media_type": format!("{:?}", s.media_type),
                        "duration": s.duration,
                    })).collect::<Vec<_>>(),
                });
                println!("{}", serde_json::to_string_pretty(&info).unwrap());
            } else {
                println!("File: {}", ctx.filename.as_deref().unwrap_or("unknown"));
                println!(
                    "Format: {}",
                    ctx.format_name.as_deref().unwrap_or("unknown")
                );
                println!("Streams: {}", ctx.streams.len());
                for stream in &ctx.streams {
                    println!(
                        "  Stream #{}: {:?} ({:?})",
                        stream.index, stream.codec_id, stream.media_type
                    );
                }
            }
        }
        Err(e) => {
            eprintln!("Error probing file: {}", e);
            std::process::exit(1);
        }
    }
}

fn cmd_transcode(input: &str, output: &str, _vcodec: Option<&str>, _acodec: Option<&str>) {
    eprintln!("Transcoding {} -> {}", input, output);
    match rsmpeg::format::FormatContext::open_input(input) {
        Ok(ctx) => {
            println!(
                "Input: {} ({} streams)",
                ctx.filename.as_deref().unwrap_or("?"),
                ctx.streams.len()
            );
            println!("Transcoding to: {}", output);
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

fn cmd_play(input: &str) {
    eprintln!("Playing: {}", input);
    match rsmpeg::format::FormatContext::open_input(input) {
        Ok(ctx) => {
            println!(
                "Input: {} ({} streams)",
                ctx.filename.as_deref().unwrap_or("?"),
                ctx.streams.len()
            );
            println!("Playback: audio device output not yet implemented (skeleton).");
            println!(
                "Run `rsmpeg probe \"{}\"` to inspect the file instead.",
                input
            );
        }
        Err(e) => {
            eprintln!("Error opening input: {}", e);
            std::process::exit(1);
        }
    }
}
