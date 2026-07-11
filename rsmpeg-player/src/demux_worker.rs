//! Background demux / decode / output worker (UI-thread free).

use std::fs::File;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use crate::codec_detect::{
    classify_track, find_audio_track, find_h264_video_track, find_unsupported_video, TrackKind,
};
use crate::command::PlayerCommand;
use crate::event::{PlayerEvent, PlayerSnapshot};
use crate::h264_bitstream::{
    avcc_extradata_to_annex_b, avcc_nal_length_size, extract_avcc_streaming, packet_for_decoder,
    H264BitstreamFormat,
};

const LATE_DROP_SEC: f64 = 0.050;
const MAX_PACE_SLEEP: Duration = Duration::from_millis(12);
const MAX_AUDIO_QUEUE_SOURCES: usize = 48;

/// Spawn the playback worker thread.
pub fn spawn_worker(
    path: PathBuf,
    volume: f32,
    cmd_rx: Receiver<PlayerCommand>,
    event_tx: SyncSender<PlayerEvent>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        if let Err(e) = run_worker(path, volume, cmd_rx, event_tx.clone()) {
            let _ = event_tx.try_send(PlayerEvent::Error {
                message: e.to_string(),
                generation: 0,
            });
        }
    })
}

fn emit(tx: &SyncSender<PlayerEvent>, ev: PlayerEvent) {
    let _ = tx.try_send(ev);
}

fn snap(
    playing: bool,
    position: Duration,
    duration: Duration,
    volume: f32,
    generation: u64,
    status: &str,
) -> PlayerEvent {
    PlayerEvent::Snapshot(PlayerSnapshot {
        playing,
        position,
        duration,
        volume,
        generation,
        status: status.into(),
    })
}

#[cfg(all(
    feature = "backend-symphonia",
    feature = "backend-openh264",
    feature = "audio-rodio"
))]
fn run_worker(
    path: PathBuf,
    mut volume: f32,
    cmd_rx: Receiver<PlayerCommand>,
    event_tx: SyncSender<PlayerEvent>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use openh264::formats::YUVSource;
    use rodio::{OutputStream, Sink};
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::errors::Error;
    use symphonia::core::formats::{FormatOptions, SeekMode, SeekTo, Track};
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;
    use symphonia::core::units::Time;

    let mut generation = 1u64;
    let mut playing = true;
    let mut stop = false;
    let mut position = Duration::ZERO;
    let mut force_one_frame = false;
    let mut was_playing = true;

    emit(
        &event_tx,
        snap(
            true,
            position,
            Duration::ZERO,
            volume,
            generation,
            "opening",
        ),
    );

    let file = Box::new(File::open(&path)?);
    let mss = MediaSourceStream::new(file, Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe().format(
        &hint,
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    )?;
    let mut format = probed.format;
    let tracks = format.tracks().to_vec();

    let audio_track: Option<Track> = find_audio_track(&tracks).cloned();
    let mut video_track: Option<Track> = find_h264_video_track(&tracks).cloned();
    let mut stream_avcc = None;
    if video_track.is_none() {
        if let Some(u) = find_unsupported_video(&tracks) {
            emit(
                &event_tx,
                PlayerEvent::Warning {
                    message: format!("Unsupported video codec '{}' — audio only", u.name()),
                    generation,
                },
            );
        } else if let Some(avcc) = extract_avcc_streaming(&path) {
            video_track = tracks
                .iter()
                .find(|t| !matches!(classify_track(t), TrackKind::Audio))
                .cloned();
            stream_avcc = Some(avcc);
        }
    }
    let has_video = video_track.is_some();
    let has_audio = audio_track.is_some();
    if !has_video && !has_audio {
        return Err("No playable audio or H.264 video tracks found".into());
    }

    let track_duration_sec = |t: &Track| -> f64 {
        let n = t.codec_params.n_frames.unwrap_or(0) as f64;
        if n <= 0.0 {
            return 0.0;
        }
        if let Some(sr) = t.codec_params.sample_rate {
            if sr > 0 {
                return n / f64::from(sr);
            }
        }
        if let Some(tb) = t.codec_params.time_base {
            let s = tb.numer as f64 / tb.denom.max(1) as f64;
            if s > 0.0 && s.is_finite() {
                return n * s;
            }
        }
        0.0
    };
    let duration_sec = {
        let a = audio_track.as_ref().map(track_duration_sec).unwrap_or(0.0);
        let v = video_track.as_ref().map(track_duration_sec).unwrap_or(0.0);
        if a > 0.0 {
            a
        } else {
            v
        }
    };
    let duration = Duration::from_secs_f64(duration_sec.max(0.0));

    let sec_per_tick = video_track
        .as_ref()
        .and_then(|t| t.codec_params.time_base)
        .map(|tb| tb.numer as f64 / tb.denom.max(1) as f64)
        .filter(|s| s.is_finite() && *s > 0.0);
    let assumed_frame_dur = 1.0 / 30.0;
    let mut playback_start: Option<Instant> = None;
    let mut base_video_pts: Option<u64> = None;
    let mut video_frame_index = 0u64;

    let mut h264 = if has_video {
        openh264::decoder::Decoder::with_api_config(
            openh264::OpenH264API::from_source(),
            openh264::decoder::DecoderConfig::new()
                .flush_after_decode(openh264::decoder::Flush::NoFlush),
        )
        .ok()
    } else {
        None
    };

    let mut audio_decoder = audio_track.as_ref().and_then(|t| {
        symphonia::default::get_codecs()
            .make(&t.codec_params, &DecoderOptions::default())
            .ok()
    });
    let audio_track_id = audio_track.as_ref().map(|t| t.id);
    let audio_channels = audio_track
        .as_ref()
        .and_then(|t| t.codec_params.channels)
        .map(|c| c.count() as u16)
        .unwrap_or(2);
    let audio_rate = audio_track
        .as_ref()
        .and_then(|t| t.codec_params.sample_rate)
        .unwrap_or(44100);

    let _rodio = OutputStream::try_default();
    let sink = _rodio
        .as_ref()
        .ok()
        .and_then(|(_, h)| Sink::try_new(h).ok());
    if let Some(ref s) = sink {
        s.set_volume(volume);
    }

    let avcc_blob = video_track
        .as_ref()
        .and_then(|t| t.codec_params.extra_data.as_ref().map(|e| e.to_vec()))
        .or(stream_avcc);
    let (sps_pps_prefix, bitstream_format) = if h264.is_some() {
        if let Some(ref avcc) = avcc_blob {
            match (avcc_nal_length_size(avcc), avcc_extradata_to_annex_b(avcc)) {
                (Ok(n), Ok(a)) => (Some(a), H264BitstreamFormat::Avcc { nal_length_size: n }),
                _ => (None, H264BitstreamFormat::Avcc { nal_length_size: 4 }),
            }
        } else {
            (None, H264BitstreamFormat::AnnexB)
        }
    } else {
        (None, H264BitstreamFormat::AnnexB)
    };
    let mut sps_pps_sent = false;
    let seek_track_id = video_track.as_ref().or(audio_track.as_ref()).map(|t| t.id);

    emit(
        &event_tx,
        snap(true, position, duration, volume, generation, "playing"),
    );

    loop {
        // ── Drain commands ──
        loop {
            match cmd_rx.try_recv() {
                Ok(cmd) => {
                    let g = cmd.generation();
                    match cmd {
                        PlayerCommand::Play { .. } => {
                            if !was_playing {
                                playback_start = Some(
                                    Instant::now()
                                        - Duration::from_secs_f64(position.as_secs_f64()),
                                );
                                if let Some(ref s) = sink {
                                    s.play();
                                }
                            }
                            playing = true;
                            was_playing = true;
                            emit(
                                &event_tx,
                                snap(true, position, duration, volume, g, "playing"),
                            );
                        }
                        PlayerCommand::Pause { .. } => {
                            playing = false;
                            was_playing = false;
                            if let Some(ref s) = sink {
                                s.pause();
                            }
                            emit(
                                &event_tx,
                                snap(false, position, duration, volume, g, "paused"),
                            );
                        }
                        PlayerCommand::Stop { .. } | PlayerCommand::Shutdown { .. } => {
                            stop = true;
                            if let Some(ref s) = sink {
                                s.pause();
                                s.clear();
                            }
                        }
                        PlayerCommand::Seek {
                            position: target,
                            generation: g,
                            ..
                        } => {
                            generation = g;
                            let capped = {
                                let t = target.as_secs_f64().max(0.0);
                                if duration_sec > 0.0 {
                                    t.min(duration_sec)
                                } else {
                                    t
                                }
                            };
                            position = Duration::from_secs_f64(capped);
                            let _ = format.seek(
                                SeekMode::Coarse,
                                SeekTo::Time {
                                    time: Time::from(capped),
                                    track_id: seek_track_id,
                                },
                            );
                            if let Some(ref mut d) = audio_decoder {
                                d.reset();
                            }
                            if has_video {
                                h264 = openh264::decoder::Decoder::with_api_config(
                                    openh264::OpenH264API::from_source(),
                                    openh264::decoder::DecoderConfig::new()
                                        .flush_after_decode(openh264::decoder::Flush::NoFlush),
                                )
                                .ok();
                                sps_pps_sent = false;
                            }
                            if let Some(ref s) = sink {
                                s.clear();
                                if playing {
                                    s.play();
                                }
                            }
                            playback_start = Some(Instant::now() - Duration::from_secs_f64(capped));
                            video_frame_index = 0;
                            force_one_frame = true;
                            emit(
                                &event_tx,
                                PlayerEvent::SeekCompleted {
                                    position,
                                    generation: g,
                                },
                            );
                            emit(
                                &event_tx,
                                snap(playing, position, duration, volume, g, "seeked"),
                            );
                        }
                        PlayerCommand::SetVolume {
                            volume: v,
                            generation: g,
                        } => {
                            volume = v.clamp(0.0, 1.0);
                            if let Some(ref s) = sink {
                                s.set_volume(volume);
                            }
                            emit(
                                &event_tx,
                                snap(playing, position, duration, volume, g, "volume"),
                            );
                        }
                        other => {
                            emit(
                                &event_tx,
                                PlayerEvent::Warning {
                                    message: format!(
                                        "command not implemented: {:?}",
                                        other.generation()
                                    ),
                                    generation: other.generation(),
                                },
                            );
                        }
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    stop = true;
                    break;
                }
            }
        }
        if stop {
            break;
        }

        if !playing && !force_one_frame {
            thread::sleep(Duration::from_millis(16));
            continue;
        }

        if let Some(ref s) = sink {
            if s.len() >= MAX_AUDIO_QUEUE_SOURCES {
                thread::sleep(Duration::from_millis(4));
                continue;
            }
            s.set_volume(volume);
        }

        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(Error::ResetRequired) => break,
            Err(Error::IoError(ref e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(_) => break,
        };
        let tid = packet.track_id();

        if has_video && h264.is_some() && video_track.as_ref().is_some_and(|vt| vt.id == tid) {
            let pts = packet.ts();
            let annex_b = match packet_for_decoder(
                &packet.data,
                bitstream_format,
                sps_pps_prefix.as_deref(),
                sps_pps_sent,
            ) {
                Ok(b) => {
                    sps_pps_sent = true;
                    b
                }
                Err(_) => Vec::new(),
            };
            if !annex_b.is_empty() {
                if let Ok(Some(yuv)) = h264.as_mut().unwrap().decode(&annex_b) {
                    let (w, h) = yuv.dimensions();
                    if w > 0 && h > 0 {
                        let abs_pos = match sec_per_tick {
                            Some(spt) => {
                                if base_video_pts.is_none() {
                                    base_video_pts = Some(pts);
                                }
                                pts.saturating_sub(base_video_pts.unwrap_or(pts)) as f64 * spt
                            }
                            None => video_frame_index as f64 * assumed_frame_dur,
                        };
                        let mut present = true;
                        if !force_one_frame {
                            if playback_start.is_none() {
                                playback_start = Some(Instant::now());
                            }
                            if let Some(t0) = playback_start {
                                let delta = abs_pos - t0.elapsed().as_secs_f64();
                                if delta < -LATE_DROP_SEC {
                                    present = false;
                                } else if delta > 0.001 {
                                    let mut rem = Duration::from_secs_f64(delta.min(0.5));
                                    while rem > Duration::ZERO {
                                        thread::sleep(rem.min(MAX_PACE_SLEEP));
                                        rem = rem.saturating_sub(MAX_PACE_SLEEP);
                                        // Check for stop/pause without consuming other cmds fully
                                        if matches!(
                                            cmd_rx.try_recv(),
                                            Ok(PlayerCommand::Stop { .. })
                                                | Ok(PlayerCommand::Shutdown { .. })
                                                | Err(TryRecvError::Disconnected)
                                        ) {
                                            stop = true;
                                            present = false;
                                            break;
                                        }
                                        if !playing && !force_one_frame {
                                            present = false;
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                        video_frame_index += 1;
                        position = Duration::from_secs_f64(abs_pos.max(0.0));
                        if present || force_one_frame {
                            let mut rgba = vec![0u8; w * h * 4];
                            yuv.write_rgba8(&mut rgba);
                            emit(
                                &event_tx,
                                PlayerEvent::VideoFrame {
                                    width: w,
                                    height: h,
                                    rgba,
                                    pts: position,
                                    generation,
                                },
                            );
                            force_one_frame = false;
                            emit(
                                &event_tx,
                                PlayerEvent::PositionChanged {
                                    position,
                                    generation,
                                },
                            );
                        }
                    }
                }
            }
        }

        if has_audio && audio_decoder.is_some() && audio_track_id == Some(tid) {
            if force_one_frame && !playing {
                continue;
            }
            match audio_decoder.as_mut().unwrap().decode(&packet) {
                Ok(buf) => {
                    let spec = *buf.spec();
                    let mut sb = SampleBuffer::<i16>::new(buf.capacity() as u64, spec);
                    sb.copy_interleaved_ref(buf);
                    let samples = sb.samples().to_vec();
                    if !samples.is_empty() {
                        if let Some(ref s) = sink {
                            s.append(rodio::buffer::SamplesBuffer::new(
                                audio_channels,
                                audio_rate,
                                samples,
                            ));
                        }
                    }
                }
                Err(Error::DecodeError(_)) | Err(Error::IoError(_)) => {}
                Err(_) => break,
            }
        }
        if stop {
            break;
        }
    }

    if !stop {
        if let Some(ref mut d) = h264 {
            if let Ok(frames) = d.flush_remaining() {
                for yuv in &frames {
                    let (w, h) = yuv.dimensions();
                    if w > 0 && h > 0 {
                        let mut rgba = vec![0u8; w * h * 4];
                        yuv.write_rgba8(&mut rgba);
                        emit(
                            &event_tx,
                            PlayerEvent::VideoFrame {
                                width: w,
                                height: h,
                                rgba,
                                pts: position,
                                generation,
                            },
                        );
                    }
                }
            }
        }
        if let Some(ref s) = sink {
            s.sleep_until_end();
        }
        emit(&event_tx, PlayerEvent::Ended { generation });
    } else {
        emit(
            &event_tx,
            snap(false, position, duration, volume, generation, "stopped"),
        );
    }
    Ok(())
}

#[cfg(not(all(
    feature = "backend-symphonia",
    feature = "backend-openh264",
    feature = "audio-rodio"
)))]
fn run_worker(
    _path: PathBuf,
    _volume: f32,
    _cmd_rx: Receiver<PlayerCommand>,
    event_tx: SyncSender<PlayerEvent>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    emit(
        &event_tx,
        PlayerEvent::Error {
            message: "playback backends disabled".into(),
            generation: 0,
        },
    );
    Err("backends disabled".into())
}
