//! Native demux path: `rsmpeg-format` packets → backend OpenH264 / Symphonia decode.
//!
//! Used when `prefer_native_pipeline` is true and the container yields a real
//! sample index (currently non-fragmented MP4, and WAV PCM).

use std::sync::mpsc::{Receiver, SyncSender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use rsmpeg_codec::CodecId;
use rsmpeg_format::FormatContext;
use rsmpeg_util::MediaType;

use crate::command::PlayerCommand;
use crate::event::{PlayerEvent, PlayerSnapshot};

const LATE_DROP_SEC: f64 = 0.050;
const MAX_PACE_SLEEP: Duration = Duration::from_millis(12);
const MAX_AUDIO_QUEUE_SOURCES: usize = 48;

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

/// Try native demux. Returns `Ok(())` if the session finished on this path.
/// Returns `Err` when native demux is unavailable so the caller can fall back.
pub fn try_run_native(
    path: &std::path::Path,
    volume: f32,
    cmd_rx: &Receiver<PlayerCommand>,
    event_tx: &SyncSender<PlayerEvent>,
) -> Result<(), String> {
    #[cfg(not(all(
        feature = "backend-symphonia",
        feature = "backend-openh264",
        feature = "audio-rodio"
    )))]
    {
        let _ = (path, volume, cmd_rx, event_tx);
        return Err("backends disabled".into());
    }

    #[cfg(all(
        feature = "backend-symphonia",
        feature = "backend-openh264",
        feature = "audio-rodio"
    ))]
    {
        run_native_inner(path, volume, cmd_rx, event_tx)
    }
}

/// Convert interleaved S16 LE plane bytes to `i16` samples for rodio.
#[cfg(all(
    feature = "backend-symphonia",
    feature = "backend-openh264",
    feature = "audio-rodio"
))]
fn s16_plane_to_i16(plane: &[u8]) -> Vec<i16> {
    plane
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect()
}

#[cfg(all(
    feature = "backend-symphonia",
    feature = "backend-openh264",
    feature = "audio-rodio"
))]
fn run_native_inner(
    path: &std::path::Path,
    mut volume: f32,
    cmd_rx: &Receiver<PlayerCommand>,
    event_tx: &SyncSender<PlayerEvent>,
) -> Result<(), String> {
    use rodio::{OutputStream, Sink};
    use rsmpeg_codec::{DecodeStatus, Decoder};

    use crate::backend::symphonia_audio::SymphoniaAudioDecoder;
    use crate::backend::OpenH264Decoder;
    use crate::video_convert::yuv420p_frame_to_rgba;

    emit(
        event_tx,
        snap(
            true,
            Duration::ZERO,
            Duration::ZERO,
            volume,
            1,
            "opening-native",
        ),
    );

    let mut ctx = FormatContext::open_input(path).map_err(|e| e.to_string())?;
    ctx.read_header().map_err(|e| e.to_string())?;

    let format_name = ctx.format_name.clone().unwrap_or_default();
    let native_ok = matches!(format_name.as_str(), "mp4" | "mov" | "m4a" | "m4v" | "wav");
    if !native_ok {
        return Err(format!(
            "native demux not preferred for format '{format_name}'"
        ));
    }

    // Peek first packet — proves sample index exists.
    let first = ctx
        .read_frame()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "native demux produced no packets (fragmented or empty stbl)".to_string())?;
    let mut pending: Option<rsmpeg_codec::Packet> = Some(first);

    let video_si = ctx
        .streams
        .iter()
        .position(|s| s.media_type == MediaType::Video && s.codec_id == CodecId::H264);
    let audio_si = ctx.streams.iter().position(|s| {
        s.media_type == MediaType::Audio
            && matches!(
                s.codec_id,
                CodecId::Aac | CodecId::Pcm | CodecId::Mp3 | CodecId::Alac
            )
    });

    if video_si.is_none() && audio_si.is_none() {
        return Err("native demux: no H.264 video or supported audio stream".into());
    }

    if let Some(u) = ctx.streams.iter().find(|s| {
        s.media_type == MediaType::Video
            && s.codec_id != CodecId::H264
            && s.codec_id != CodecId::Unknown
    }) {
        emit(
            event_tx,
            PlayerEvent::Warning {
                message: format!(
                    "Unsupported video codec '{}' — skipping video",
                    u.codec_id.name()
                ),
                generation: 1,
            },
        );
    }

    let duration_ms = ctx.duration.max(0) as u64;
    let duration = Duration::from_millis(duration_ms);
    let duration_sec = duration.as_secs_f64();

    let mut generation = 1u64;
    let mut playing = true;
    let mut stop = false;
    let mut position = Duration::ZERO;
    let mut force_one_frame = false;
    let mut was_playing = true;
    let mut playback_start: Option<Instant> = None;
    let mut video_frame_index = 0u64;
    let assumed_frame_dur = 1.0 / 30.0;

    // ── Video decoder (OpenH264 backend) ──
    let mut video_dec: Option<OpenH264Decoder> = if let Some(si) = video_si {
        match OpenH264Decoder::from_params(&ctx.streams[si].codec_params) {
            Ok(d) => Some(d),
            Err(e) => {
                emit(
                    event_tx,
                    PlayerEvent::Warning {
                        message: format!("native path: failed to open OpenH264 decoder: {e}"),
                        generation: 1,
                    },
                );
                None
            }
        }
    } else {
        None
    };

    // ── Audio decoder (Symphonia packet-in backend) ──
    let (mut audio_dec, mut audio_channels, mut audio_rate) = if let Some(si) = audio_si {
        let stream = &ctx.streams[si];
        let rate = stream.codec_params.sample_rate.unwrap_or(48_000);
        let ch = stream.codec_params.channels.unwrap_or(2);
        if !SymphoniaAudioDecoder::supported(stream.codec_id) {
            emit(
                event_tx,
                PlayerEvent::Warning {
                    message: format!(
                        "native audio codec '{}' not wired — audio muted",
                        stream.codec_id.name()
                    ),
                    generation: 1,
                },
            );
            (None, ch, rate)
        } else {
            match SymphoniaAudioDecoder::try_new(&stream.codec_params) {
                Ok(d) => (Some(d), ch, rate),
                Err(e) => {
                    emit(
                        event_tx,
                        PlayerEvent::Warning {
                            message: format!(
                                "native path: failed to open Symphonia audio decoder: {e}"
                            ),
                            generation: 1,
                        },
                    );
                    (None, ch, rate)
                }
            }
        }
    } else {
        (None, 2u16, 44_100u32)
    };

    let _rodio = OutputStream::try_default();
    let sink = _rodio
        .as_ref()
        .ok()
        .and_then(|(_, h)| Sink::try_new(h).ok());
    if let Some(ref s) = sink {
        s.set_volume(volume);
    }

    emit(
        event_tx,
        snap(
            true,
            position,
            duration,
            volume,
            generation,
            "playing-native",
        ),
    );
    emit(
        event_tx,
        PlayerEvent::Warning {
            message: format!("using native demux ({format_name})"),
            generation,
        },
    );

    let video_time_base = video_si.map(|si| {
        let tb = &ctx.streams[si].time_base;
        tb.num as f64 / tb.den.max(1) as f64
    });
    let mut base_video_pts: Option<i64> = None;

    loop {
        // ── Commands ──
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
                                event_tx,
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
                                event_tx,
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
                            let ts_ms = (capped * 1000.0) as i64;
                            let _ = ctx.seek(ts_ms);
                            pending = ctx.read_frame().ok().flatten();
                            if let Some(ref mut d) = audio_dec {
                                let _ = d.reset();
                            }
                            if let Some(ref mut d) = video_dec {
                                let _ = d.reset();
                            }
                            if let Some(ref s) = sink {
                                s.clear();
                                if playing {
                                    s.play();
                                }
                            }
                            playback_start = Some(Instant::now() - Duration::from_secs_f64(capped));
                            video_frame_index = 0;
                            base_video_pts = None;
                            force_one_frame = true;
                            emit(
                                event_tx,
                                PlayerEvent::SeekCompleted {
                                    position,
                                    generation: g,
                                },
                            );
                            emit(
                                event_tx,
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
                                event_tx,
                                snap(playing, position, duration, volume, g, "volume"),
                            );
                        }
                        other => {
                            emit(
                                event_tx,
                                PlayerEvent::Warning {
                                    message: format!(
                                        "command not implemented: gen={}",
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

        let mut packet = match pending.take() {
            Some(p) => p,
            None => match ctx.read_frame() {
                Ok(Some(p)) => p,
                Ok(None) => break,
                Err(_) => break,
            },
        };
        let si = packet.stream_index;

        // Ensure PTS is populated before send_packet (backend uses pts.or(dts)).
        if packet.pts.is_none() {
            packet.pts = packet.dts;
        }

        // ── Video ──
        if Some(si) == video_si {
            if let Some(ref mut dec) = video_dec {
                if dec.send_packet(Some(&packet)).is_ok() {
                    loop {
                        match dec.receive_frame() {
                            Ok(DecodeStatus::Frame(f)) => {
                                let w = f.width;
                                let h = f.height;
                                if w > 0 && h > 0 {
                                    let pts = f.pts.unwrap_or(0);
                                    let abs_pos = match video_time_base {
                                        Some(spt) if spt > 0.0 && spt.is_finite() => {
                                            if base_video_pts.is_none() {
                                                base_video_pts = Some(pts);
                                            }
                                            let base = base_video_pts.unwrap_or(pts);
                                            (pts - base) as f64 * spt
                                        }
                                        _ => video_frame_index as f64 * assumed_frame_dur,
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
                                                let mut rem =
                                                    Duration::from_secs_f64(delta.min(0.5));
                                                while rem > Duration::ZERO {
                                                    thread::sleep(rem.min(MAX_PACE_SLEEP));
                                                    rem = rem.saturating_sub(MAX_PACE_SLEEP);
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
                                        if let Ok(rgba) = yuv420p_frame_to_rgba(&f) {
                                            emit(
                                                event_tx,
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
                                                event_tx,
                                                PlayerEvent::PositionChanged {
                                                    position,
                                                    generation,
                                                },
                                            );
                                        }
                                    }
                                }
                                if stop {
                                    break;
                                }
                            }
                            Ok(DecodeStatus::NeedMoreInput) | Ok(DecodeStatus::EndOfStream) => {
                                break;
                            }
                            Err(_) => break,
                        }
                    }
                }
            }
        }

        // ── Audio ──
        if Some(si) == audio_si {
            if force_one_frame && !playing {
                continue;
            }
            if let Some(ref mut dec) = audio_dec {
                if dec.send_packet(Some(&packet)).is_ok() {
                    loop {
                        match dec.receive_frame() {
                            Ok(DecodeStatus::Frame(f)) => {
                                if f.channels > 0 {
                                    audio_channels = f.channels;
                                }
                                if f.sample_rate > 0 {
                                    audio_rate = f.sample_rate;
                                }
                                let samples = s16_plane_to_i16(
                                    f.data.first().map(|d| d.as_slice()).unwrap_or(&[]),
                                );
                                if !samples.is_empty() {
                                    if let Some(ref s) = sink {
                                        s.append(rodio::buffer::SamplesBuffer::new(
                                            audio_channels,
                                            audio_rate,
                                            samples,
                                        ));
                                    }
                                }
                                // Drive position from audio when no video
                                if video_si.is_none() {
                                    if let Some(ts) = f.pts.or(packet.pts).or(packet.dts) {
                                        let tb = &ctx.streams[si].time_base;
                                        let sec = ts as f64 * tb.num as f64 / tb.den.max(1) as f64;
                                        position = Duration::from_secs_f64(sec.max(0.0));
                                        emit(
                                            event_tx,
                                            PlayerEvent::PositionChanged {
                                                position,
                                                generation,
                                            },
                                        );
                                    }
                                }
                            }
                            Ok(DecodeStatus::NeedMoreInput) | Ok(DecodeStatus::EndOfStream) => {
                                break;
                            }
                            Err(_) => break,
                        }
                    }
                }
            }
        }

        if stop {
            break;
        }
    }

    if !stop {
        // Flush video decoder: send_packet(None) + drain remaining frames.
        if let Some(ref mut dec) = video_dec {
            if dec.send_packet(None).is_ok() {
                loop {
                    match dec.receive_frame() {
                        Ok(DecodeStatus::Frame(f)) => {
                            if let Ok(rgba) = yuv420p_frame_to_rgba(&f) {
                                let w = f.width;
                                let h = f.height;
                                if w > 0 && h > 0 {
                                    emit(
                                        event_tx,
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
                        Ok(DecodeStatus::NeedMoreInput) | Ok(DecodeStatus::EndOfStream) => break,
                        Err(_) => break,
                    }
                }
            }
        }
        // Soft-flush audio decoder (no presentation frames beyond sink queue).
        if let Some(ref mut dec) = audio_dec {
            let _ = dec.send_packet(None);
            loop {
                match dec.receive_frame() {
                    Ok(DecodeStatus::Frame(f)) => {
                        let samples =
                            s16_plane_to_i16(f.data.first().map(|d| d.as_slice()).unwrap_or(&[]));
                        if !samples.is_empty() {
                            if let Some(ref s) = sink {
                                s.append(rodio::buffer::SamplesBuffer::new(
                                    f.channels.max(1),
                                    f.sample_rate.max(1),
                                    samples,
                                ));
                            }
                        }
                    }
                    Ok(DecodeStatus::NeedMoreInput) | Ok(DecodeStatus::EndOfStream) => break,
                    Err(_) => break,
                }
            }
        }
        if let Some(ref s) = sink {
            s.sleep_until_end();
        }
        emit(event_tx, PlayerEvent::Ended { generation });
    } else {
        emit(
            event_tx,
            snap(false, position, duration, volume, generation, "stopped"),
        );
    }
    Ok(())
}
