//! Background playback engine thread.
//!
//! Spawns a thread that owns Symphonia demux + OpenH264 video decode +
//! rodio audio output.  Video frames are sent via an mpsc channel to the
//! UI; audio samples are fed directly to a rodio [`Sink`].

use std::fs::File;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use openh264::formats::YUVSource;
use rodio::{OutputStream, Sink};
use rsmpeg_cli::codec_detect::{
    classify_track, find_audio_track, find_h264_video_track, find_unsupported_video, TrackKind,
};
use rsmpeg_cli::h264_bitstream::{
    avcc_extradata_to_annex_b, avcc_nal_length_size, extract_avcc_streaming, packet_for_decoder,
    H264BitstreamFormat,
};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error;
use symphonia::core::formats::{FormatOptions, SeekMode, SeekTo, Track};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;

use super::state::{FrameData, PlaybackState};

/// Max pending decoded frames.  When full the engine drops the newest frame
/// instead of blocking demux/audio (keeps A/V flowing smoothly).
const FRAME_QUEUE_CAP: usize = 2;

/// Drop a frame if it is this far behind the wall clock (avoids cascading lag).
const LATE_DROP_SEC: f64 = 0.050;

/// Do not sleep longer than this in one shot so audio demux can resume soon.
const MAX_PACE_SLEEP: Duration = Duration::from_millis(12);

/// Soft cap on rodio queued sources — prevents runaway memory if video stalls.
const MAX_AUDIO_QUEUE_SOURCES: usize = 48;

/// Estimate track duration in seconds from codec parameters.
fn track_duration_sec(t: &Track) -> f64 {
    let n_frames = match t.codec_params.n_frames {
        Some(n) if n > 0 => n as f64,
        _ => return 0.0,
    };
    // Audio: frames / sample rate
    if let Some(sr) = t.codec_params.sample_rate {
        if sr > 0 {
            return n_frames / f64::from(sr);
        }
    }
    // Video / timed tracks: frames × time base
    if let Some(tb) = t.codec_params.time_base {
        let sec_per_tick = tb.numer as f64 / tb.denom.max(1) as f64;
        if sec_per_tick.is_finite() && sec_per_tick > 0.0 {
            return n_frames * sec_per_tick;
        }
    }
    0.0
}

fn create_h264_decoder() -> Option<openh264::decoder::Decoder> {
    match openh264::decoder::Decoder::with_api_config(
        openh264::OpenH264API::from_source(),
        openh264::decoder::DecoderConfig::new()
            .flush_after_decode(openh264::decoder::Flush::NoFlush),
    ) {
        Ok(d) => Some(d),
        Err(e) => {
            eprintln!("  [gui] Warning: could not create H.264 decoder: {:?}", e);
            None
        }
    }
}

/// Convert YUV → RGBA with OpenH264's SIMD path (no intermediate RGB buffer).
fn yuv_frame_to_rgba(yuv: &openh264::decoder::DecodedYUV<'_>) -> Vec<u8> {
    let (w, h) = yuv.dimensions();
    let mut rgba = vec![0u8; w * h * 4];
    yuv.write_rgba8(&mut rgba);
    rgba
}

// ---------------------------------------------------------------------------
// Engine handle
// ---------------------------------------------------------------------------

/// Handle to a running playback engine.
pub struct PlaybackEngine {
    /// Receiver for decoded video frames.
    pub frame_rx: mpsc::Receiver<FrameData>,
    /// Shared playback state (controls play/pause/volume).
    pub state: Arc<Mutex<PlaybackState>>,
    /// Background thread handle.
    pub handle: Option<thread::JoinHandle<()>>,
}

impl PlaybackEngine {
    /// Open a media file and start playback in a background thread.
    pub fn new(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let (frame_tx, frame_rx) = mpsc::sync_channel::<FrameData>(FRAME_QUEUE_CAP);
        let state = Arc::new(Mutex::new(PlaybackState::default()));
        let state_for_engine = state.clone();
        let state_for_error = state.clone();

        let path_owned = path.to_string();
        let handle = thread::spawn(move || {
            if let Err(e) = run_engine(&path_owned, frame_tx, state_for_engine) {
                super::state::lock_state(&state_for_error).status = e.to_string();
                eprintln!("  [gui] Engine error: {}", e);
            }
        });

        Ok(Self {
            frame_rx,
            state,
            handle: Some(handle),
        })
    }

    /// Ask the playback thread to stop at the next safe packet boundary.
    pub fn stop(&self) {
        let mut state = super::state::lock_state(&self.state);
        state.playing = false;
        state.stop_requested = true;
    }
}

impl Drop for PlaybackEngine {
    fn drop(&mut self) {
        self.stop();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Engine implementation
// ---------------------------------------------------------------------------

fn run_engine(
    path: &str,
    frame_tx: mpsc::SyncSender<FrameData>,
    state: Arc<Mutex<PlaybackState>>,
) -> Result<(), Box<dyn std::error::Error>> {
    // ── Open file ──
    let file_path = std::path::Path::new(path);
    let file = Box::new(File::open(file_path)?);
    let mss = MediaSourceStream::new(file, Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = file_path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let fmt_opts = FormatOptions::default();
    let meta_opts = MetadataOptions::default();
    let dec_opts = DecoderOptions::default();

    let probed = symphonia::default::get_probe().format(&hint, mss, &fmt_opts, &meta_opts)?;
    let mut format = probed.format;

    // ── Find tracks (explicit codec detection — never assume NULL ⇒ H.264) ──
    let tracks = format.tracks().to_vec();

    let audio_track: Option<Track> = find_audio_track(&tracks).cloned();
    let mut video_track: Option<Track> = find_h264_video_track(&tracks).cloned();
    let has_audio = audio_track.is_some();

    // Container-level avcC probe (streaming, not whole-file) when Symphonia
    // did not expose a clear H.264 track but the path is MP4-like.
    let mut stream_avcc: Option<Vec<u8>> = None;
    if video_track.is_none() {
        if let Some(unsupported) = find_unsupported_video(&tracks) {
            let msg = format!(
                "Unsupported video codec '{}' — video disabled (audio may continue)",
                unsupported.name()
            );
            eprintln!("  [gui] {}", msg);
            super::state::lock_state(&state).status = msg;
        } else if let Some(avcc) = extract_avcc_streaming(path) {
            // Treat first non-audio track as H.264 only when avcC is proven.
            video_track = tracks
                .iter()
                .find(|t| !matches!(classify_track(t), TrackKind::Audio))
                .cloned();
            stream_avcc = Some(avcc);
            if video_track.is_none() {
                eprintln!("  [gui] Found avcC but no video track object");
            }
        }
    }

    let has_video = video_track.is_some();
    if !has_video && !has_audio {
        return Err("No playable audio or H.264 video tracks found".into());
    }

    // Video timing basis for frame pacing.
    let video_time_base = video_track.as_ref().and_then(|t| t.codec_params.time_base);
    let sec_per_tick = video_time_base
        .map(|tb| tb.numer as f64 / tb.denom.max(1) as f64)
        .filter(|s| s.is_finite() && *s > 0.0);
    let assumed_frame_dur = 1.0 / 30.0;
    let mut playback_start: Option<Instant> = None;
    // Absolute timeline base: first video PTS ever seen (not reset on seek).
    let mut base_video_pts: Option<u64> = None;
    let mut video_frame_index: u64 = 0;
    // After a seek while paused, decode until one frame is painted.
    let mut force_one_frame = false;
    let mut was_playing = true;

    // ── Duration ──
    {
        let mut s = super::state::lock_state(&state);
        let audio_dur = audio_track.as_ref().map(track_duration_sec).unwrap_or(0.0);
        let video_dur = video_track.as_ref().map(track_duration_sec).unwrap_or(0.0);
        s.duration_sec = if audio_dur > 0.0 {
            audio_dur
        } else {
            video_dur
        };
    }

    // ── OpenH264 decoder ──
    let mut h264 = if has_video {
        create_h264_decoder()
    } else {
        None
    };

    // ── Audio decoder ──
    let audio_codec_params = audio_track.as_ref().map(|t| &t.codec_params);
    let mut audio_decoder =
        audio_codec_params.and_then(|cp| symphonia::default::get_codecs().make(cp, &dec_opts).ok());

    let audio_track_id = audio_track.as_ref().map(|t| t.id);

    let audio_channels: u16 = audio_track
        .as_ref()
        .and_then(|t| t.codec_params.channels)
        .map(|cl| cl.count() as u16)
        .unwrap_or(2);

    let audio_rate: u32 = audio_track
        .as_ref()
        .and_then(|t| t.codec_params.sample_rate)
        .unwrap_or(44100);

    // ── rodio audio output ──
    let _rodio_result = OutputStream::try_default();
    let sink: Option<Sink> = _rodio_result
        .as_ref()
        .ok()
        .and_then(|(_, handle)| Sink::try_new(handle).ok());

    if let (Some(sink), true) = (sink.as_ref(), has_audio) {
        let s = super::state::lock_state(&state);
        sink.set_volume(s.volume);
    }

    // ── H.264 bitstream format + SPS/PPS (from track extradata or streaming avcC) ──
    let track_extra = video_track
        .as_ref()
        .and_then(|t| t.codec_params.extra_data.as_ref().map(|e| e.to_vec()));
    let avcc_blob = track_extra.or(stream_avcc);

    let (sps_pps_prefix, bitstream_format): (Option<Vec<u8>>, H264BitstreamFormat) =
        if h264.is_some() {
            if let Some(ref avcc) = avcc_blob {
                match (avcc_nal_length_size(avcc), avcc_extradata_to_annex_b(avcc)) {
                    (Ok(nls), Ok(annex_b)) => (
                        Some(annex_b),
                        H264BitstreamFormat::Avcc {
                            nal_length_size: nls,
                        },
                    ),
                    (Err(e), _) | (_, Err(e)) => {
                        eprintln!("  [gui] avcC parse warning: {}", e);
                        (None, H264BitstreamFormat::Avcc { nal_length_size: 4 })
                    }
                }
            } else {
                // No avcC — assume Annex B elementary stream (do not AVCC-convert).
                (None, H264BitstreamFormat::AnnexB)
            }
        } else {
            (None, H264BitstreamFormat::AnnexB)
        };
    let mut sps_pps_sent = false;

    let seek_track_id = video_track.as_ref().or(audio_track.as_ref()).map(|t| t.id);

    // ── Playback loop ──
    loop {
        // Pause / stop / seek / volume
        {
            let mut s = super::state::lock_state(&state);
            if s.stop_requested {
                break;
            }

            // Pause / resume audio sink + re-anchor wall clock.
            if s.playing && !was_playing {
                let pos = s.position_sec.max(0.0);
                playback_start = Some(Instant::now() - Duration::from_secs_f64(pos));
                if let Some(ref snk) = sink {
                    snk.play();
                }
            } else if !s.playing && was_playing {
                // Pause audio immediately; do not keep demuxing into the queue.
                if let Some(ref snk) = sink {
                    snk.pause();
                }
            }
            was_playing = s.playing;

            // Apply pending seek from the UI timeline.
            if let Some(target) = s.seek_to_sec.take() {
                let target = target.max(0.0);
                let dur = s.duration_sec;
                let target = if dur > 0.0 { target.min(dur) } else { target };
                s.position_sec = target;
                let playing_now = s.playing;
                drop(s);

                let seek_result = format.seek(
                    SeekMode::Coarse,
                    SeekTo::Time {
                        time: Time::from(target),
                        track_id: seek_track_id,
                    },
                );

                match seek_result {
                    Ok(_) => {
                        if let Some(ref mut dec) = audio_decoder {
                            dec.reset();
                        }
                        if has_video {
                            h264 = create_h264_decoder();
                            sps_pps_sent = false;
                        }
                        if let Some(ref snk) = sink {
                            snk.clear();
                            snk.play();
                        }
                        playback_start =
                            Some(Instant::now() - Duration::from_secs_f64(target.max(0.0)));
                        video_frame_index = 0;
                        force_one_frame = true;
                        was_playing = playing_now;
                    }
                    Err(e) => {
                        eprintln!("  [gui] Seek failed: {:?}", e);
                    }
                }

                let s = super::state::lock_state(&state);
                if !s.playing && !force_one_frame && s.status != "ended" {
                    if let Some(ref snk) = sink {
                        snk.pause();
                        snk.set_volume(s.volume);
                    }
                    drop(s);
                    thread::sleep(Duration::from_millis(16));
                    continue;
                }
                if let Some(ref snk) = sink {
                    snk.set_volume(s.volume);
                }
                drop(s);
            } else {
                if !s.playing && !force_one_frame && s.status != "ended" {
                    if let Some(ref snk) = sink {
                        snk.pause();
                        snk.set_volume(s.volume);
                    }
                    drop(s);
                    // Do not demux/decode while paused — prevents audio queue growth.
                    thread::sleep(Duration::from_millis(16));
                    continue;
                }
                if let Some(ref snk) = sink {
                    snk.set_volume(s.volume);
                }
                drop(s);
            }
        }

        // Soft audio backpressure: if the sink is overloaded, yield briefly so
        // rodio can drain instead of growing unbounded.
        if let Some(ref snk) = sink {
            if snk.len() >= MAX_AUDIO_QUEUE_SOURCES {
                thread::sleep(Duration::from_millis(4));
                continue;
            }
        }

        // Read next packet
        let packet = match format.next_packet() {
            Ok(pkt) => pkt,
            Err(Error::ResetRequired) => break,
            Err(Error::IoError(ref e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(_) => break,
        };

        let track_id = packet.track_id();

        // ── Video ──
        let is_video =
            has_video && h264.is_some() && video_track.as_ref().is_some_and(|vt| vt.id == track_id);

        if is_video {
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
                Err(e) => {
                    // One-shot warning; do not spam decode errors for bad NALs.
                    static WARNED: std::sync::atomic::AtomicBool =
                        std::sync::atomic::AtomicBool::new(false);
                    if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                        eprintln!("  [gui] H.264 bitstream convert: {}", e);
                    }
                    Vec::new()
                }
            };

            if !annex_b.is_empty() {
                match h264.as_mut().unwrap().decode(&annex_b) {
                    Ok(Some(yuv)) => {
                        let (w, h) = yuv.dimensions();
                        if w > 0 && h > 0 {
                            // Absolute presentation time (seconds) for this frame.
                            let abs_pos = match sec_per_tick {
                                Some(spt) => {
                                    if base_video_pts.is_none() {
                                        base_video_pts = Some(packet_pts);
                                    }
                                    let base = base_video_pts.unwrap_or(packet_pts);
                                    packet_pts.saturating_sub(base) as f64 * spt
                                }
                                None => video_frame_index as f64 * assumed_frame_dur,
                            };

                            // ── Pace / drop relative to wall clock ──
                            let mut present = true;
                            if !force_one_frame {
                                if playback_start.is_none() {
                                    playback_start = Some(Instant::now());
                                }
                                if let Some(t0) = playback_start {
                                    let elapsed = t0.elapsed().as_secs_f64();
                                    let delta = abs_pos - elapsed;

                                    if delta < -LATE_DROP_SEC {
                                        // Too late — drop to catch up (no convert/send).
                                        present = false;
                                    } else if delta > 0.001 {
                                        // Early — sleep in short slices so audio demux
                                        // can resume soon (avoids long A/V stalls).
                                        let mut remaining = Duration::from_secs_f64(delta.min(0.5));
                                        while remaining > Duration::ZERO {
                                            let slice = remaining.min(MAX_PACE_SLEEP);
                                            thread::sleep(slice);
                                            remaining = remaining.saturating_sub(slice);

                                            // Bail early if user seeked / stopped / paused.
                                            let s = super::state::lock_state(&state);
                                            if s.stop_requested
                                                || s.seek_to_sec.is_some()
                                                || (!s.playing && !force_one_frame)
                                            {
                                                present = false;
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                            video_frame_index += 1;

                            if present {
                                // Direct YUV → RGBA (SIMD when width % 8 == 0).
                                let rgba = yuv_frame_to_rgba(&yuv);

                                // Non-blocking send: if UI is behind, drop this
                                // frame rather than blocking demux/audio.
                                match frame_tx.try_send(FrameData {
                                    rgba,
                                    width: w,
                                    height: h,
                                }) {
                                    Ok(()) | Err(mpsc::TrySendError::Disconnected(_)) => {}
                                    Err(mpsc::TrySendError::Full(_)) => {
                                        // UI still has pending frames; skip this one.
                                    }
                                }
                                force_one_frame = false;

                                let mut s = super::state::lock_state(&state);
                                if s.seek_to_sec.is_none() {
                                    s.position_sec = abs_pos;
                                }
                            } else if force_one_frame {
                                // Still need a scrub preview frame.
                                let rgba = yuv_frame_to_rgba(&yuv);
                                let _ = frame_tx.try_send(FrameData {
                                    rgba,
                                    width: w,
                                    height: h,
                                });
                                force_one_frame = false;
                                let mut s = super::state::lock_state(&state);
                                if s.seek_to_sec.is_none() {
                                    s.position_sec = abs_pos;
                                }
                            } else {
                                // Dropped for catch-up — still advance playhead.
                                let mut s = super::state::lock_state(&state);
                                if s.seek_to_sec.is_none() {
                                    s.position_sec = abs_pos;
                                }
                            }
                        }
                    }
                    Ok(None) => {
                        // OpenH264 buffers internally; no output yet
                    }
                    Err(e) => {
                        eprintln!("  [gui] H.264 decode error: {:?}", e);
                    }
                }
            }
        }

        // ── Audio ──
        let is_audio = has_audio && audio_decoder.is_some() && (audio_track_id == Some(track_id));

        if is_audio {
            // While force-decoding a scrubbed frame while paused, skip audio.
            let playing_now = super::state::lock_state(&state).playing;
            if force_one_frame && !playing_now {
                continue;
            }

            match audio_decoder.as_mut().unwrap().decode(&packet) {
                Ok(audio_buf) => {
                    let spec = *audio_buf.spec();
                    let duration = audio_buf.capacity() as u64;
                    let mut sample_buf = SampleBuffer::<i16>::new(duration, spec);
                    sample_buf.copy_interleaved_ref(audio_buf);
                    let interleaved = sample_buf.samples().to_vec();

                    if !interleaved.is_empty() {
                        if let Some(ref snk) = sink {
                            let source = rodio::buffer::SamplesBuffer::new(
                                audio_channels,
                                audio_rate,
                                interleaved,
                            );
                            snk.append(source);
                        }
                    }
                }
                Err(Error::DecodeError(_)) => { /* skip */ }
                Err(Error::IoError(_)) => { /* skip */ }
                Err(_) => break,
            }
        }
    }

    let stop_requested = super::state::lock_state(&state).stop_requested;

    // ── Flush remaining video frames ──
    if !stop_requested {
        if let Some(ref mut dec) = h264 {
            if let Ok(frames) = dec.flush_remaining() {
                for yuv in &frames {
                    let (w, h) = yuv.dimensions();
                    if w > 0 && h > 0 {
                        let rgba = yuv_frame_to_rgba(yuv);
                        let _ = frame_tx.try_send(FrameData {
                            rgba,
                            width: w,
                            height: h,
                        });
                    }
                }
            }
        }
    }

    // ── Wait for audio to finish ──
    if !stop_requested {
        if let Some(ref snk) = sink {
            snk.sleep_until_end();
        }
    }

    if stop_requested {
        super::state::lock_state(&state).status = "stopped".into();
    } else {
        super::state::lock_state(&state).status = "ended".into();
    }

    Ok(())
}
