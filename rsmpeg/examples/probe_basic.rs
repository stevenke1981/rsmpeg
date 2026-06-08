//! Basic probe example — open a file, read headers, and display stream info.
//!
//! Usage: cargo run --example probe_basic -- <path-to-media-file>

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: probe_basic <media-file>");
        std::process::exit(1);
    }

    // Register built-in components before using them
    rsmpeg::format::format_registry::register_builtin_formats();
    rsmpeg::codec::codec_registry::register_builtin_codecs();

    match rsmpeg::format::FormatContext::open_input(&args[1]) {
        Ok(mut ctx) => {
            // Read container header to discover streams
            if let Err(e) = ctx.read_header() {
                eprintln!("Warning: could not read header: {}", e);
            }

            println!("=== Media Info ===");
            println!("File: {}", ctx.filename.as_deref().unwrap_or("unknown"));
            println!(
                "Format: {}",
                ctx.format_name.as_deref().unwrap_or("unknown")
            );
            println!("Streams: {}", ctx.streams.len());
            for stream in &ctx.streams {
                println!(
                    "  Stream #{}: {:?} ({:?}) — {:.2}s",
                    stream.index,
                    stream.codec_id,
                    stream.media_type,
                    stream.duration_seconds()
                );
            }
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}
