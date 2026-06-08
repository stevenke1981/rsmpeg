//! Background playback engine thread.
//!
//! Spawns a thread that owns Symphonia demux + OpenH264 video decode +
//! rodio audio output.  Video frames are sent via an mpsc channel to the
//! UI; audio samples are fed directly to a rodio [`Sink`].

use std::fs::File;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

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

use super::state::{FrameData, PlaybackState};

// ---------------------------------------------------------------------------
// Track detection helpers
// ---------------------------------------------------------------------------

fn track_is_audio(t: &Track) -> bool {
    t.codec_params.codec != CODEC_TYPE_NULL && t.codec_params.sample_rate.is_some()
}

fn track_is_video(t: &Track) -> bool {
    t.codec_params.codec == CODEC_TYPE_NULL
        || (t.codec_params.sample_rate.is_none() && t.codec_params.codec != CODEC_TYPE_NULL)
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
        let (frame_tx, frame_rx) = mpsc::channel::<FrameData>();
        let state = Arc::new(Mutex::new(PlaybackState::default()));
        let state_clone = state.clone();

        let path_owned = path.to_string();
        let handle = thread::spawn(move || {
            if let Err(e) = run_engine(&path_owned, frame_tx, state_clone) {
                eprintln!("  [gui] Engine error: {}", e);
            }
        });

        Ok(Self {
            frame_rx,
            state,
            handle: Some(handle),
        })
    }
}

// ---------------------------------------------------------------------------
// Engine implementation
// ---------------------------------------------------------------------------

fn run_engine(
    path: &str,
    frame_tx: mpsc::Sender<FrameData>,
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

    // ── Find tracks ──
    let tracks = format.tracks().to_vec();

    let video_track: Option<Track> = tracks.iter().find(|t| track_is_video(t)).cloned();
    let audio_track: Option<Track> = tracks.iter().find(|t| track_is_audio(t)).cloned();

    let has_video = video_track.is_some();
    let has_audio = audio_track.is_some();

    // ── Duration ──
    // Symphonia doesn't expose total duration directly from the probe;
    // n_frames is codec-specific.  Try to get duration from the format
    // context if possible.
    {
        let mut s = state.lock().unwrap();
        if let Some(ref t) = video_track.as_ref().or(audio_track.as_ref()) {
            s.duration_sec = t.codec_params.n_frames.unwrap_or(0) as f64;
        }
    }

    // ── OpenH264 decoder ──
    let mut h264 = if has_video {
        match openh264::decoder::Decoder::new() {
            Ok(d) => Some(d),
            Err(e) => {
                eprintln!("  [gui] Warning: could not create H.264 decoder: {:?}", e);
                None
            }
        }
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
    // Keep OutputStream alive for the entire engine lifetime (it drives
    // audio).  If output cannot be created we continue without audio.
    let _rodio_result = OutputStream::try_default();
    let sink: Option<Sink> = _rodio_result
        .as_ref()
        .ok()
        .and_then(|(_, handle)| Sink::try_new(handle).ok());

    if sink.is_some() && has_audio {
        // Apply initial volume
        {
            let s = state.lock().unwrap();
            sink.as_ref().unwrap().set_volume(s.volume);
        }
    }

    // ── Playback loop ──
    loop {
        // Pause check
        {
            let s = state.lock().unwrap();
            if !s.playing && s.status != "ended" {
                drop(s);
                thread::sleep(Duration::from_millis(16));
                continue;
            }
            // Apply volume changes periodically
            if let Some(ref snk) = sink {
                snk.set_volume(s.volume);
            }
            drop(s);
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
        let is_video = has_video
            && h264.is_some()
            && video_track.as_ref().map_or(false, |vt| vt.id == track_id);

        if is_video {
            // AVCC (4-byte length prefix) -> Annex B (start-code prefix)
            let data: &[u8] = &packet.data;
            let mut annex_b = Vec::with_capacity(data.len() + 32);
            let mut i = 0;
            while i + 4 <= data.len() {
                let nal_size =
                    u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize;
                i += 4;
                if i + nal_size <= data.len() {
                    annex_b.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
                    annex_b.extend_from_slice(&data[i..i + nal_size]);
                    i += nal_size;
                } else {
                    break;
                }
            }

            if !annex_b.is_empty() {
                match h264.as_mut().unwrap().decode(&annex_b) {
                    Ok(Some(yuv)) => {
                        let (w, h) = yuv.dimensions();
                        if w > 0 && h > 0 {
                            let rgb_len = (w * h * 3) as usize;
                            let mut rgb_buf = vec![0u8; rgb_len];
                            yuv.write_rgb8(&mut rgb_buf);

                            // RGB -> RGBA
                            let rgba: Vec<u8> = rgb_buf
                                .chunks(3)
                                .flat_map(|c| [c[0], c[1], c[2], 255])
                                .collect();

                            let _ = frame_tx.send(FrameData {
                                rgba,
                                width: w as usize,
                                height: h as usize,
                            });

                            // Rough position update (~30 fps assumed)
                            {
                                let mut s = state.lock().unwrap();
                                s.position_sec += 1.0 / 30.0;
                            }
                        }
                    }
                    Ok(None) => {
                        // OpenH264 buffers internally; no output yet
                    }
                    Err(e) => {
                        // B-frame reordering etc. — skip
                        eprintln!("  [gui] H.264 decode error: {:?}", e);
                    }
                }
            }
        }

        // ── Audio ──
        let is_audio = has_audio
            && audio_decoder.is_some()
            && audio_track_id.map_or(false, |id| id == track_id);

        if is_audio {
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

    // ── Flush remaining video frames ──
    if let Some(ref mut dec) = h264 {
        if let Ok(frames) = dec.flush_remaining() {
            for yuv in &frames {
                let (w, h) = yuv.dimensions();
                if w > 0 && h > 0 {
                    let rgb_len = (w * h * 3) as usize;
                    let mut rgb_buf = vec![0u8; rgb_len];
                    yuv.write_rgb8(&mut rgb_buf);
                    let rgba: Vec<u8> = rgb_buf
                        .chunks(3)
                        .flat_map(|c| [c[0], c[1], c[2], 255])
                        .collect();
                    let _ = frame_tx.send(FrameData {
                        rgba,
                        width: w as usize,
                        height: h as usize,
                    });
                }
            }
        }
    }

    // ── Wait for audio to finish ──
    if let Some(ref snk) = sink {
        snk.sleep_until_end();
    }

    state.lock().unwrap().status = "ended".into();

    Ok(())
}
