//! Basic probe example — open a file and display stream info.
//!
//! Usage: cargo run --example probe_basic -- <path-to-media-file>

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: probe_basic <media-file>");
        std::process::exit(1);
    }

    match rsmpeg::format::FormatContext::open_input(&args[1]) {
        Ok(ctx) => {
            println!("=== Media Info ===");
            println!("File: {}", ctx.filename.as_deref().unwrap_or("unknown"));
            println!(
                "Format: {}",
                ctx.format_name.as_deref().unwrap_or("unknown")
            );
            println!("Streams: {}", ctx.streams.len());
            for (i, stream) in ctx.streams.iter().enumerate() {
                println!(
                    "  Stream #{}: index={}, codec={:?}, type={:?}",
                    i, stream.index, stream.codec_id, stream.media_type
                );
            }
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}
