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
use symphonia::core::errors::Error;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use rsmpeg_cli::codec_detect::{
    classify_track, find_audio_track, find_h264_video_track, find_unsupported_video, TrackKind,
};
use rsmpeg_cli::h264_bitstream::{
    avcc_extradata_to_annex_b, avcc_nal_length_size, extract_avcc_streaming, packet_for_decoder,
    H264BitstreamFormat,
};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Play a media file with both audio and video.
pub fn play_media(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let path_str = path.to_string();
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

    let audio_track = find_audio_track(&tracks);
    let mut video_track = find_h264_video_track(&tracks);
    let mut stream_avcc: Option<Vec<u8>> = None;

    if video_track.is_none() {
        if let Some(unsupported) = find_unsupported_video(&tracks) {
            eprintln!(
                "  Unsupported video codec '{}' — playing audio only",
                unsupported.name()
            );
        } else if let Some(avcc) = extract_avcc_streaming(&path_str) {
            video_track = tracks
                .iter()
                .find(|t| !matches!(classify_track(t), TrackKind::Audio));
            stream_avcc = Some(avcc);
        }
    }

    let has_video = video_track.is_some();
    let has_audio = audio_track.is_some();

    if !has_video && !has_audio {
        return Err("No playable audio or H.264 video tracks found".into());
    }

    // OpenH264 only for proven H.264
    let mut h264_decoder = if has_video {
        match openh264::decoder::Decoder::with_api_config(
            openh264::OpenH264API::from_source(),
            openh264::decoder::DecoderConfig::new()
                .flush_after_decode(openh264::decoder::Flush::NoFlush),
        ) {
            Ok(d) => Some(d),
            Err(e) => {
                eprintln!("  Warning: could not create H.264 decoder: {:?}", e);
                None
            }
        }
    } else {
        None
    };
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
    let _have_known_video_dims = false;
    let window_width: usize = 640;
    let window_height: usize = 480;

    // Video timing basis for frame pacing.  We present each decoded frame at
    // its presentation timestamp (PTS) so playback runs at the correct real
    // rate and stays in sync with the audio sink.  When PTS/timebase are
    // missing we fall back to the declared frame rate, then 30 fps.
    let video_time_base = video_track.and_then(|t| t.codec_params.time_base);
    let sec_per_tick = video_time_base
        .map(|tb| tb.numer as f64 / tb.denom.max(1) as f64)
        .filter(|s| s.is_finite() && *s > 0.0);
    let assumed_frame_dur = 1.0 / 30.0;

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
            Ok(w) => {
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

    // ── H.264 bitstream format + SPS/PPS (streaming avcC, no whole-file read) ──
    let track_extra =
        video_track.and_then(|t| t.codec_params.extra_data.as_ref().map(|e| e.to_vec()));
    let avcc_blob = track_extra.or(stream_avcc);
    let (sps_pps_prefix, bitstream_format): (Option<Vec<u8>>, H264BitstreamFormat) =
        if h264_decoder.is_some() {
            if let Some(ref avcc) = avcc_blob {
                match (avcc_nal_length_size(avcc), avcc_extradata_to_annex_b(avcc)) {
                    (Ok(nls), Ok(annex_b)) => (
                        Some(annex_b),
                        H264BitstreamFormat::Avcc {
                            nal_length_size: nls,
                        },
                    ),
                    _ => (None, H264BitstreamFormat::Avcc { nal_length_size: 4 }),
                }
            } else {
                (None, H264BitstreamFormat::AnnexB)
            }
        } else {
            (None, H264BitstreamFormat::AnnexB)
        };
    let mut sps_pps_sent = false;

    // Pixel buffer for display (RGBA32 format, 0x00RRGGBB)
    let mut pixel_buffer: Vec<u32> = vec![0; window_width * window_height];

    // Frame pacing state.  `start` is anchored to the first presented frame;
    // subsequent frames sleep until their PTS-relative target time.
    let mut playback_start: Option<Instant> = None;
    let mut first_video_pts: u64 = 0;
    let mut video_frame_index: u64 = 0;

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

        let mut new_frame_decoded = false;

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
            && video_track.is_some_and(|vt| vt.id == packet_track_id);

        if is_video_packet {
            // MP4 stores H.264 in AVCC format (length-prefixed NAL units).
            // OpenH264 handles both AVCC and Annex B, but Annex B is more
            // universal. Convert AVCC → Annex B:
            let packet_pts = packet.ts();
            let data: &[u8] = &packet.data;
            let annex_b = match packet_for_decoder(
                data,
                bitstream_format,
                sps_pps_prefix.as_deref(),
                sps_pps_sent,
            ) {
                Ok(buf) => {
                    sps_pps_sent = true;
                    buf
                }
                Err(_) => Vec::new(),
            };

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

                            // Convert RGB8 (3 bytes/pixel) -> RGBA32 (u32/pixel).
                            // copy_w/copy_h are bounded by w/h, so the source
                            // indices are always in range; the per-pixel bounds
                            // check is dropped for speed.
                            let copy_w = w.min(window_width);
                            let copy_h = h.min(window_height);
                            for y in 0..copy_h {
                                let src_row = y * w;
                                let dst_row = y * window_width;
                                for x in 0..copy_w {
                                    let si = (src_row + x) * 3;
                                    let di = dst_row + x;
                                    pixel_buffer[di] = ((rgb_buf[si] as u32) << 16)
                                        | ((rgb_buf[si + 1] as u32) << 8)
                                        | (rgb_buf[si + 2] as u32);
                                }
                            }

                            // ── Pace this frame to its presentation time ──
                            // Anchor the clock on the first presented frame, then
                            // sleep until the frame's target delay has elapsed so
                            // the video plays at its native rate and stays in sync
                            // with the real-time audio sink.
                            let target_delay = match sec_per_tick {
                                Some(spt) => {
                                    if playback_start.is_none() {
                                        first_video_pts = packet_pts;
                                    }
                                    let ticks = packet_pts.saturating_sub(first_video_pts) as f64;
                                    std::time::Duration::from_secs_f64(ticks * spt)
                                }
                                None => std::time::Duration::from_secs_f64(
                                    video_frame_index as f64 * assumed_frame_dur,
                                ),
                            };
                            if playback_start.is_none() {
                                playback_start = Some(Instant::now());
                            }
                            if let Some(t0) = playback_start {
                                let elapsed = t0.elapsed();
                                if target_delay > elapsed {
                                    thread::sleep(target_delay - elapsed);
                                }
                            }
                            video_frame_index += 1;
                            new_frame_decoded = true;
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
        let is_audio_packet =
            has_audio && audio_decoder.is_some() && (audio_track_id == Some(packet_track_id));

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

        // Update window — render a fresh frame only when one was decoded.
        // For audio-only packets we just pump the event loop (no buffer
        // re-upload, no fps throttle) so the picture stays put and the
        // window stays responsive without slowing the video down.
        if let Some(ref mut w) = window {
            if new_frame_decoded {
                let _ = w.update_with_buffer(&pixel_buffer, window_width, window_height);
            } else {
                let _ = w.update();
            }
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
/// Starts the rodio output immediately and appends each decoded chunk as it
/// arrives, so audio plays in real time and stays aligned with the
/// PTS-paced video instead of buffering the whole file up front.
fn play_audio_from_channel(
    rx: mpsc::Receiver<Vec<i16>>,
    channels: u16,
    sample_rate: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    let (_stream, stream_handle) = OutputStream::try_default()?;
    let sink = Sink::try_new(&stream_handle)?;

    while let Ok(samples) = rx.recv() {
        if samples.is_empty() {
            continue;
        }
        let source = rodio::buffer::SamplesBuffer::new(channels, sample_rate, samples);
        sink.append(source);
    }

    sink.sleep_until_end();
    Ok(())
}

/// Play only audio (no video window) — used when there's no video track.
#[allow(dead_code)]
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

    let track = find_audio_track(format.tracks()).ok_or("No audio track found")?;

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
