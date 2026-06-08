//! Media playback engine — H.264 video via OpenH264 + window via minifb,
//! audio via Symphonia + rodio, synchronized output.

use std::fs::File;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Instant;

use openh264::formats::YUVSource;
use rodio::{OutputStream, Sink};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::codecs::CODEC_TYPE_NULL;
use symphonia::core::errors::Error;
use symphonia::core::formats::FormatOptions;
use symphonia::core::formats::Track;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

// ---------------------------------------------------------------------------
// Track heuristics — Symphonia 0.5 only supports audio codecs natively,
// so video tracks appear as "unknown" (CODEC_TYPE_NULL).
// ---------------------------------------------------------------------------

fn track_is_audio(t: &Track) -> bool {
    t.codec_params.codec != CODEC_TYPE_NULL && t.codec_params.sample_rate.is_some()
}

fn track_is_video(t: &Track) -> bool {
    // Symphonia doesn't know H.264 — video tracks have CODEC_TYPE_NULL
    // but no audio parameters.
    t.codec_params.codec == CODEC_TYPE_NULL
        || (t.codec_params.sample_rate.is_none() && t.codec_params.codec != CODEC_TYPE_NULL)
}

fn find_audio_track<'a>(tracks: &'a [Track]) -> Option<&'a Track> {
    tracks.iter().find(|t| track_is_audio(t))
}

fn find_video_track<'a>(tracks: &'a [Track]) -> Option<&'a Track> {
    tracks.iter().find(|t| track_is_video(t))
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Play a media file with both audio and video.
pub fn play_media(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let path = std::path::Path::new(path);
    let file = Box::new(File::open(path)?);

    // Create the media source stream for Symphonia
    let mss = MediaSourceStream::new(file, Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let fmt_opts: FormatOptions = Default::default();
    let meta_opts: MetadataOptions = Default::default();

    // Probe the format
    let probed = symphonia::default::get_probe().format(&hint, mss, &fmt_opts, &meta_opts)?;
    let mut format = probed.format;

    let tracks = format.tracks().to_vec();

    let has_video = find_video_track(&tracks).is_some();
    let has_audio = find_audio_track(&tracks).is_some();

    if !has_video && !has_audio {
        return Err("No playable audio or video tracks found".into());
    }

    // Setup OpenH264 decoder if we have video
    let mut h264_decoder = if has_video {
        match openh264::decoder::Decoder::new() {
            Ok(d) => Some(d),
            Err(e) => {
                eprintln!("  Warning: could not create H.264 decoder: {:?}", e);
                None
            }
        }
    } else {
        None
    };

    // Audio params
    let audio_track = find_audio_track(&tracks);
    let audio_sample_rate = audio_track
        .and_then(|t| t.codec_params.sample_rate)
        .unwrap_or(0);
    let audio_channels = audio_track
        .and_then(|t| t.codec_params.channels)
        .map(|cl| cl.count() as u16)
        .unwrap_or(0);

    println!(
        "  Tracks: {} total (video={}, audio={})",
        tracks.len(),
        if has_video { 1 } else { 0 },
        if has_audio { 1 } else { 0 }
    );
    if has_audio {
        println!(
            "  Audio: {} Hz, {} channels",
            audio_sample_rate, audio_channels
        );
    }

    // Create audio decoder
    let dec_opts: DecoderOptions = Default::default();
    let audio_track_params = audio_track.map(|t| &t.codec_params);
    let mut audio_decoder = if let Some(cp) = audio_track_params {
        match symphonia::default::get_codecs().make(cp, &dec_opts) {
            Ok(d) => Some(d),
            Err(e) => {
                eprintln!("  Warning: could not create audio decoder: {}", e);
                None
            }
        }
    } else {
        None
    };
    let audio_track_id = audio_track.map(|t| t.id);

    // Start audio output thread
    let (audio_tx, audio_rx) = mpsc::channel::<Vec<i16>>();
    let audio_handle = if has_audio {
        let sample_rate = audio_sample_rate;
        let channels = audio_channels;
        Some(thread::spawn(move || {
            if let Err(e) = play_audio_from_channel(audio_rx, channels, sample_rate) {
                eprintln!("  Audio output error: {}", e);
            }
        }))
    } else {
        None
    };

    // Video window — use generous default size; OpenH264 will decode at
    // the actual stream resolution regardless.
    let video_track = find_video_track(&tracks);
    let _have_known_video_dims = false;
    let window_width: usize = 640;
    let window_height: usize = 480;

    let file_name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();

    let mut window = if has_video && h264_decoder.is_some() {
        match minifb::Window::new(
            &format!("rsmpeg - {}", file_name),
            window_width,
            window_height,
            minifb::WindowOptions::default(),
        ) {
            Ok(mut w) => {
                w.set_target_fps(60);
                println!(
                    "  Video window: {}x{} (stream may differ)",
                    window_width, window_height
                );
                Some(w)
            }
            Err(e) => {
                eprintln!("  Warning: could not create window: {}", e);
                None
            }
        }
    } else if has_video && h264_decoder.is_none() {
        println!("  Video track present but no decoder available — audio only");
        None
    } else {
        None
    };

    // Pixel buffer for display (RGBA32 format, 0x00RRGGBB)
    let mut pixel_buffer: Vec<u32> = vec![0; window_width * window_height];

    // Playback loop — read packets, decode, display
    let mut video_frame_count = 0u64;
    let start_time = Instant::now();

    'playback: loop {
        // Check for window close
        if let Some(ref w) = window {
            if !w.is_open() || w.is_key_down(minifb::Key::Escape) {
                break 'playback;
            }
        }

        // Read next packet
        let packet = match format.next_packet() {
            Ok(pkt) => pkt,
            Err(Error::ResetRequired) => {
                break;
            }
            Err(Error::IoError(ref e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                break;
            }
            Err(_) => break,
        };

        let packet_track_id = packet.track_id();

        // Process video packet
        let is_video_packet = has_video
            && h264_decoder.is_some()
            && video_track.map_or(false, |vt| vt.id == packet_track_id);

        if is_video_packet {
            // MP4 stores H.264 in AVCC format (4-byte length prefixes).
            // OpenH264 handles both AVCC and Annex B, but Annex B is more
            // universal. Convert AVCC → Annex B:
            let data: &[u8] = &packet.data;
            let mut annex_b = Vec::with_capacity(data.len() + 32);
            let mut i = 0;
            while i + 4 <= data.len() {
                let nal_size =
                    u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize;
                i += 4;
                if i + nal_size <= data.len() {
                    // Write start code
                    annex_b.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
                    // Write NAL unit
                    annex_b.extend_from_slice(&data[i..i + nal_size]);
                    i += nal_size;
                } else {
                    break;
                }
            }

            // Decode H.264
            if !annex_b.is_empty() {
                match h264_decoder.as_mut().unwrap().decode(&annex_b) {
                    Ok(Some(yuv)) => {
                        video_frame_count += 1;

                        let (w, h) = yuv.dimensions();
                        if w > 0 && h > 0 {
                            // Allocate RGB buffer and convert using OpenH264's fast path
                            let rgb_len = w * h * 3;
                            let mut rgb_buf = vec![0u8; rgb_len];
                            yuv.write_rgb8(&mut rgb_buf);

                            // Convert RGB8 (3 bytes per pixel) to RGBA32 (u32 per pixel)
                            let copy_w = w.min(window_width);
                            let copy_h = h.min(window_height);
                            for y in 0..copy_h {
                                for x in 0..copy_w {
                                    let src_idx = (y * w + x) * 3;
                                    let dst_idx = y * window_width + x;
                                    if src_idx + 2 < rgb_buf.len() {
                                        let r = rgb_buf[src_idx] as u32;
                                        let g = rgb_buf[src_idx + 1] as u32;
                                        let b = rgb_buf[src_idx + 2] as u32;
                                        pixel_buffer[dst_idx] = (r << 16) | (g << 8) | b;
                                    }
                                }
                            }
                        }
                    }
                    Ok(None) => {
                        // OpenH264 buffers frames internally; no output ready yet
                    }
                    Err(e) => {
                        // Decode errors are common with B-frames/reordering;
                        // just skip them.
                        eprintln!("  H.264 decode error: {:?}", e);
                    }
                }
            }
        }

        // Process audio packet
        let is_audio_packet = has_audio
            && audio_decoder.is_some()
            && audio_track_id.map_or(false, |id| id == packet_track_id);

        if is_audio_packet {
            match audio_decoder.as_mut().unwrap().decode(&packet) {
                Ok(audio_buf) => {
                    // Convert to interleaved i16 using SampleBuffer
                    let spec = *audio_buf.spec();
                    let duration = audio_buf.capacity() as u64;
                    let mut sample_buf = SampleBuffer::<i16>::new(duration, spec);
                    sample_buf.copy_interleaved_ref(audio_buf);
                    let interleaved = sample_buf.samples().to_vec();

                    if !interleaved.is_empty() {
                        let _ = audio_tx.send(interleaved);
                    }
                }
                Err(Error::DecodeError(_)) => {
                    // Skip invalid packets
                }
                Err(Error::IoError(_)) => {
                    // I/O error during decode
                }
                Err(_) => break,
            }
        }

        // Update window
        if let Some(ref mut w) = window {
            let _ = w.update_with_buffer(&pixel_buffer, window_width, window_height);
        }
    }

    // Flush remaining frames from the decoder
    if let Some(ref mut dec) = h264_decoder {
        if let Ok(frames) = dec.flush_remaining() {
            for _yuv in &frames {
                video_frame_count += 1;
            }
        }
    }

    // Cleanup — drop sender so the audio thread can finish
    drop(audio_tx);
    if let Some(handle) = audio_handle {
        let _ = handle.join();
    }

    let elapsed = start_time.elapsed();
    if video_frame_count > 0 {
        println!(
            "  Playback complete: {} video frames in {:.1}s ({:.0} fps)",
            video_frame_count,
            elapsed.as_secs_f64(),
            video_frame_count as f64 / elapsed.as_secs_f64()
        );
    } else {
        println!("  Playback complete.");
    }

    Ok(())
}

/// Play audio from a channel feed (runs in a separate thread).
/// Collects all samples from the channel, then plays them through rodio.
fn play_audio_from_channel(
    rx: mpsc::Receiver<Vec<i16>>,
    channels: u16,
    sample_rate: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    // Collect all audio samples from the channel
    let mut all_samples: Vec<i16> = Vec::new();
    while let Ok(samples) = rx.recv() {
        all_samples.extend(samples);
    }

    if !all_samples.is_empty() {
        let (_stream, stream_handle) = OutputStream::try_default()?;
        let sink = Sink::try_new(&stream_handle)?;
        let source = rodio::buffer::SamplesBuffer::new(channels, sample_rate, all_samples);
        sink.append(source);
        sink.sleep_until_end();
    }

    Ok(())
}

/// Play only audio (no video window) — used when there's no video track.
pub fn play_audio_file(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let path = Path::new(path);
    let file = Box::new(File::open(path)?);

    let mss = MediaSourceStream::new(file, Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let fmt_opts: FormatOptions = Default::default();
    let meta_opts: MetadataOptions = Default::default();
    let dec_opts: DecoderOptions = Default::default();

    let probed = symphonia::default::get_probe().format(&hint, mss, &fmt_opts, &meta_opts)?;
    let mut format = probed.format;

    let track = format
        .tracks()
        .iter()
        .find(|t| track_is_audio(t))
        .ok_or("No audio track found")?;

    let sample_rate = track
        .codec_params
        .sample_rate
        .ok_or("Unknown sample rate")?;
    let channels = track
        .codec_params
        .channels
        .ok_or("Unknown channel count")?
        .count() as u16;
    let track_id = track.id;

    println!("  Audio: {} Hz, {} channels", sample_rate, channels);

    let mut decoder = symphonia::default::get_codecs().make(&track.codec_params, &dec_opts)?;
    let mut samples: Vec<i16> = Vec::new();

    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(Error::ResetRequired) => break,
            Err(Error::IoError(ref e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                break;
            }
            Err(_) => break,
        };

        if packet.track_id() != track_id {
            continue;
        }

        match decoder.decode(&packet) {
            Ok(audio_buf) => {
                let spec = *audio_buf.spec();
                let duration = audio_buf.capacity() as u64;
                let mut sample_buf = SampleBuffer::<i16>::new(duration, spec);
                sample_buf.copy_interleaved_ref(audio_buf);
                samples.extend_from_slice(sample_buf.samples());
            }
            Err(Error::DecodeError(_)) => continue,
            Err(Error::IoError(_)) => continue,
            Err(_) => break,
        }
    }

    if samples.is_empty() {
        return Err("No audio samples decoded".into());
    }

    let duration = samples.len() as f64 / (sample_rate as f64 * channels as f64);
    println!("  Decoded: {} samples ({:.1}s)", samples.len(), duration);

    let (_stream, stream_handle) = OutputStream::try_default()?;
    let sink = Sink::try_new(&stream_handle)?;
    let source = rodio::buffer::SamplesBuffer::new(channels, sample_rate, samples);
    sink.append(source);

    println!("  Playing... (Ctrl+C to stop)");
    sink.sleep_until_end();

    Ok(())
}
