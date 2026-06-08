//! Demonstrate the full decode → rescale → resample pipeline concept.
//!
//! This example shows how rsmpeg's components would be wired together
//! to decode media, transform it, and produce output.
//!
//! Usage: cargo run --example pipeline -p rsmpeg

fn main() {
    println!("=== rsmpeg Pipeline Example ===\n");

    // Register built-in components
    rsmpeg::format::format_registry::register_builtin_formats();
    rsmpeg::codec::codec_registry::register_builtin_codecs();

    // 1. Version info
    println!("{}", rsmpeg::version_info());
    println!();

    // 2. Create a format context (probe would detect format from magic bytes)
    println!("--- Format Layer ---");
    let probe_buf = b"\x00\x00\x00\x1Cftypmp42\x00\x00\x00\x00isommp42";
    let results = rsmpeg::format::probe::probe_format(probe_buf);
    for r in &results {
        println!(
            "  Detected: {} ({}) — score: {:?}",
            r.format_name, r.description, r.score
        );
    }
    println!();

    // 3. Show codec registry info
    println!("--- Codec Layer ---");
    let codec_id = rsmpeg::codec::CodecId::Av1;
    println!(
        "  {:?} -> {:?}, type: {:?}",
        codec_id,
        codec_id.name(),
        codec_id.media_type()
    );
    let params = rsmpeg::codec::CodecParameters::new(codec_id);
    println!(
        "  CodecParameters: bit_rate={:?}, width={:?}, height={:?}",
        params.bit_rate, params.width, params.height
    );
    println!();

    // 4. Frame creation (video)
    println!("--- Frame Layer ---");
    let frame = rsmpeg::codec::Frame::new_video(1920, 1080, rsmpeg::util::PixelFormat::Yuv420P);
    println!(
        "  Video frame: {}x{}, format={:?}",
        frame.width, frame.height, frame.pixel_format
    );
    let audio_frame = rsmpeg::codec::Frame::new_audio(
        rsmpeg::util::SampleFormat::F32,
        48000,
        2, // stereo
        1024,
    );
    println!(
        "  Audio frame: {} samples, {}ch, {}Hz, format={:?}",
        audio_frame.samples,
        audio_frame.channels,
        audio_frame.sample_rate,
        audio_frame.sample_format
    );
    println!();

    // 5. Scaler configuration
    println!("--- Scaler Layer ---");
    let scaler_config = rsmpeg::scale::ScalerConfig::new(
        1920,
        1080,
        rsmpeg::util::PixelFormat::Yuv420P,
        1280,
        720,
        rsmpeg::util::PixelFormat::Yuv420P,
    )
    .with_interpolation(rsmpeg::scale::InterpolationMethod::Lanczos);
    let scaler = rsmpeg::scale::Scaler::new(scaler_config).unwrap();
    let scaled = scaler.scale(&frame).unwrap();
    println!(
        "  Scaled: {}x{} -> {}x{}",
        frame.width, frame.height, scaled.width, scaled.height
    );
    println!();

    // 6. Resampler configuration
    println!("--- Resampler Layer ---");
    let resample_config = rsmpeg::resample::ResamplerConfig::new(
        48000,
        44100,
        rsmpeg::util::SampleFormat::F32,
        rsmpeg::util::SampleFormat::S16,
    )
    .with_dither(rsmpeg::resample::DitherMethod::Triangular);
    let resampler = rsmpeg::resample::Resampler::new(resample_config).unwrap();
    let resampled = resampler.resample(&audio_frame).unwrap();
    println!(
        "  Resampled: {}Hz -> {}Hz, format: {:?} -> {:?}",
        audio_frame.sample_rate,
        resampled.sample_rate,
        audio_frame.sample_format,
        resampled.sample_format
    );
    println!();

    // 7. Filter graph
    println!("--- Filter Graph Layer ---");
    let mut graph = rsmpeg::filter::FilterGraph::new();
    let src = graph.add_filter("src", Box::new(rsmpeg::filter::buffer::BufferSrc::new()));
    let scale = graph.add_filter(
        "scale",
        Box::new(rsmpeg::filter::builtin::ScaleFilter {
            width: 640,
            height: 480,
        }),
    );
    let null = graph.add_filter("null", Box::new(rsmpeg::filter::builtin::NullFilter));
    let sink = graph.add_filter("sink", Box::new(rsmpeg::filter::buffer::BufferSink::new()));
    graph.link(src, 0, scale, 0).unwrap();
    graph.link(scale, 0, null, 0).unwrap();
    graph.link(null, 0, sink, 0).unwrap();
    graph.validate().unwrap();
    println!("{}", graph.dump());

    // 8. Stream creation
    println!("--- Stream Layer ---");
    let mut stream = rsmpeg::format::Stream::new(0, rsmpeg::codec::CodecId::Av1);
    stream.time_base = rsmpeg::util::Rational::new(1, 1000);
    stream.duration = 120_000; // 120 seconds
    println!(
        "  Stream #{}: {:?} — duration: {:.2}s",
        stream.index,
        stream.codec_id,
        stream.duration_seconds()
    );
    println!();

    // 9. Color space info
    println!("--- Color Layer ---");
    use rsmpeg::scale::{ColorConversion, ColorRange, ColorSpace};
    let conv = ColorConversion::new(
        ColorSpace::BT709,
        ColorSpace::BT601,
        ColorRange::Limited,
        ColorRange::Full,
    );
    println!(
        "  Color conversion: {:?} -> {:?} ({:?} -> {:?})",
        conv.src_space, conv.dst_space, conv.src_range, conv.dst_range
    );

    println!("\n=== Pipeline Example Complete ===");
}
