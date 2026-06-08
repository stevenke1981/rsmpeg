//! Audio playback engine using Symphonia + rodio.
//!
//! Provides cross-platform audio output for media files using
//! pure-Rust decoders (no FFI, no system codecs).

use std::fs::File;
use std::path::Path;

use rodio::{OutputStream, Sink};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Decode an audio file into interleaved i16 samples + metadata.
pub fn decode_audio(path: &str) -> Result<DecodedAudio, Box<dyn std::error::Error>> {
    let path = Path::new(path);
    let file = Box::new(File::open(path)?);

    // Create the media source stream.
    let mss = MediaSourceStream::new(file, Default::default());

    // Create a probe hint using the file's extension.
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    // Use the default options.
    let fmt_opts: FormatOptions = Default::default();
    let meta_opts: MetadataOptions = Default::default();
    let dec_opts: DecoderOptions = Default::default();

    // Probe the media source for a supported format.
    let probed = symphonia::default::get_probe().format(&hint, mss, &fmt_opts, &meta_opts)?;

    let mut format = probed.format;

    // Find the first audio track with a known (decodeable) codec.
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
        .ok_or("No supported audio track found")?;

    // Read audio parameters.
    let sample_rate = track
        .codec_params
        .sample_rate
        .ok_or("Unknown sample rate")?;
    let channels = track
        .codec_params
        .channels
        .ok_or("Unknown channel count")?
        .count() as u16;

    // Create a decoder for the track.
    let mut decoder = symphonia::default::get_codecs().make(&track.codec_params, &dec_opts)?;

    let track_id = track.id;
    let mut samples: Vec<i16> = Vec::new();

    // Decode loop.
    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(Error::ResetRequired) => {
                // Track list changed (chained OGG). Not handled here.
                break;
            }
            Err(Error::IoError(ref e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                break;
            }
            Err(_) => break,
        };

        // Skip packets not belonging to the selected track.
        if packet.track_id() != track_id {
            continue;
        }

        // Decode the packet into audio samples.
        match decoder.decode(&packet) {
            Ok(audio_buf) => {
                // Convert the decoded audio buffer to interleaved i16 samples.
                let spec = *audio_buf.spec();
                let duration = audio_buf.capacity() as u64;
                let mut sample_buf = SampleBuffer::<i16>::new(duration, spec);
                sample_buf.copy_interleaved_ref(audio_buf);
                samples.extend_from_slice(sample_buf.samples());
            }
            Err(Error::DecodeError(_)) => {
                // Skip invalid packets.
                continue;
            }
            Err(Error::IoError(_)) => {
                continue;
            }
            Err(_) => break,
        }
    }

    if samples.is_empty() {
        return Err("No audio samples decoded".into());
    }

    Ok(DecodedAudio {
        samples,
        sample_rate,
        channels,
    })
}

/// Decoded audio data in interleaved i16 format.
pub struct DecodedAudio {
    pub samples: Vec<i16>,
    pub sample_rate: u32,
    pub channels: u16,
}

/// Play an audio file through system speakers.
pub fn play_audio_file(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let decoded = decode_audio(path)?;

    println!(
        "  Audio: {} Hz, {} channels",
        decoded.sample_rate, decoded.channels
    );
    println!(
        "  Decoded: {} samples ({:.1}s)",
        decoded.samples.len(),
        decoded.samples.len() as f64 / (decoded.sample_rate as f64 * decoded.channels as f64)
    );

    // Play via rodio.
    let (_stream, stream_handle) = OutputStream::try_default()?;
    let sink = Sink::try_new(&stream_handle)?;
    let source =
        rodio::buffer::SamplesBuffer::new(decoded.channels, decoded.sample_rate, decoded.samples);
    sink.append(source);

    println!("  Playing... (Ctrl+C to stop)");
    sink.sleep_until_end();

    Ok(())
}
