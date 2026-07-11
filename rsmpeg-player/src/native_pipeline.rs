//! Native demux path: `rsmpeg-format` packets → backend OpenH264 / Symphonia decode.
//!
//! Used when `prefer_native_pipeline` is true and the container yields a real
//! sample index (currently non-fragmented MP4, and WAV PCM).

use std::sync::mpsc::{Receiver, SyncSender, TryRecvError};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use rsmpeg_codec::CodecId;
use rsmpeg_format::FormatContext;
use rsmpeg_util::MediaType;

use crate::clock::AudioPlaybackClock;
use crate::command::PlayerCommand;
use crate::event::{PlayerEvent, PlayerSnapshot};
use crate::frame_pool::FramePool;
use crate::video_scheduler::{ScheduleAction, VideoScheduler};

const MAX_PACE_SLEEP: Duration = Duration::from_millis(12);
const MAX_AUDIO_QUEUE_SOURCES: usize = 48;

/// Process-wide reusable scratch buffer for YUV→RGBA conversion.
///
/// A single large pool is shared across every native-pipeline frame emission
/// so we allocate a fresh `Vec<u8>` only on the first frame (and if the
/// resolution changes). The verified conversion from
/// [`crate::video_convert::yuv420p_frame_to_rgba_cached`] is copied into the
/// pooled scratch buffer; the event keeps a clone and the scratch is recycled
/// for the next frame. Pixel content is therefore byte-identical to the
/// non-pooled path.
static RGBA_POOL: OnceLock<FramePool> = OnceLock::new();

fn rgba_pool() -> &'static FramePool {
    RGBA_POOL.get_or_init(|| FramePool::new(64 * 1024 * 1024))
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

    use crate::audio_convert::frame_to_s16_device;
    use crate::backend::symphonia_audio::SymphoniaAudioDecoder;
    use crate::backend::OpenH264Decoder;
    use crate::video_convert::yuv420p_frame_to_rgba_cached;

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
    // `rodio::Sink` has no rendered-sample counter. Keep an output-timeline
    // clock instead of treating queued samples as already played.
    let mut audio_clock = AudioPlaybackClock::new();
    let mut force_one_frame = false;
    let mut was_playing = true;
    let mut playback_start: Option<Instant> = None;
    let mut playback_rate = 1.0f64;
    let mut video_frame_index = 0u64;
    let assumed_frame_dur = 1.0 / 30.0;
    // ~50 ms late-drop threshold (VideoScheduler::new default).
    let mut video_scheduler = VideoScheduler::new();

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
                                        - Duration::from_secs_f64(
                                            position.as_secs_f64() / playback_rate,
                                        ),
                                );
                                if let Some(ref s) = sink {
                                    s.play();
                                }
                            }
                            playing = true;
                            was_playing = true;
                            audio_clock.resume();
                            emit(
                                event_tx,
                                snap(true, position, duration, volume, g, "playing"),
                            );
                        }
                        PlayerCommand::Pause { .. } => {
                            playing = false;
                            was_playing = false;
                            audio_clock.pause();
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
                            audio_clock.seek(position);
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
                            playback_start = Some(
                                Instant::now() - Duration::from_secs_f64(capped / playback_rate),
                            );
                            video_frame_index = 0;
                            base_video_pts = None;
                            // A paused seek may request one video preview, but
                            // audio-only playback must stay paused at the
                            // target instead of consuming its newly sought
                            // packets before Play resumes.
                            force_one_frame = video_si.is_some();
                            video_scheduler.reset_stats();
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
                        PlayerCommand::SetPlaybackRate {
                            rate,
                            generation: g,
                        } if rate.is_finite() && (0.25..=4.0).contains(&rate) => {
                            playback_rate = rate;
                            audio_clock.set_rate(rate);
                            playback_start = Some(
                                Instant::now()
                                    - Duration::from_secs_f64(position.as_secs_f64() / rate),
                            );
                            if let Some(ref s) = sink {
                                s.set_speed(rate as f32);
                            }
                            emit(
                                event_tx,
                                snap(playing, position, duration, volume, g, "rate"),
                            );
                        }
                        PlayerCommand::SetPlaybackRate { rate, generation } => {
                            emit(
                                event_tx,
                                PlayerEvent::Warning {
                                    message: format!("unsupported playback rate: {rate}"),
                                    generation,
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

        // A queued rodio source must never advance playback position. The
        // output clock advances only from the first submitted source after a
        // clear and is frozen by Pause/Seek until output resumes.
        if video_si.is_none() && audio_clock.is_output_active() {
            let audio_position = audio_clock.now();
            if audio_position != position {
                position = audio_position;
                emit(
                    event_tx,
                    PlayerEvent::PositionChanged {
                        position,
                        generation,
                    },
                );
            }
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
                                    let frame_pts = Duration::from_secs_f64(abs_pos.max(0.0));
                                    // Seek preview: always display without wait/drop.
                                    let mut present = true;
                                    if !force_one_frame {
                                        if playback_start.is_none() {
                                            playback_start = Some(Instant::now());
                                        }
                                        let now = playback_start
                                            .map(|t0| t0.elapsed().mul_f64(playback_rate))
                                            .unwrap_or(Duration::ZERO);
                                        match video_scheduler.schedule(frame_pts, now) {
                                            ScheduleAction::DropLate => {
                                                present = false;
                                            }
                                            ScheduleAction::Wait { duration } => {
                                                // Cap wait so a bad PTS cannot freeze the loop.
                                                let mut rem =
                                                    duration.min(Duration::from_millis(500));
                                                while rem > Duration::ZERO {
                                                    thread::sleep(rem.min(MAX_PACE_SLEEP));
                                                    rem = rem.saturating_sub(MAX_PACE_SLEEP);
                                                }
                                                if present {
                                                    video_scheduler.mark_displayed();
                                                }
                                            }
                                            ScheduleAction::Display => {
                                                present = true;
                                            }
                                        }
                                    }
                                    video_frame_index += 1;
                                    position = frame_pts;
                                    if present || force_one_frame {
                                        if let Ok(converted) = yuv420p_frame_to_rgba_cached(&f) {
                                            // Reuse a pooled scratch buffer instead of allocating
                                            // a fresh Vec per frame; pixels remain byte-identical.
                                            let needed_len = converted.len();
                                            let mut scratch = rgba_pool().get(needed_len);
                                            scratch.extend_from_slice(&converted);
                                            emit(
                                                event_tx,
                                                PlayerEvent::VideoFrame {
                                                    width: w,
                                                    height: h,
                                                    rgba: scratch.clone(),
                                                    pts: position,
                                                    generation,
                                                },
                                            );
                                            rgba_pool().recycle(scratch);
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
                                let device_ch = audio_channels.max(1);
                                let device_rate = audio_rate.max(1);
                                let samples = frame_to_s16_device(&f, device_rate, device_ch)
                                    .unwrap_or_default();
                                if !samples.is_empty() {
                                    if let Some(ref s) = sink {
                                        s.append(rodio::buffer::SamplesBuffer::new(
                                            device_ch,
                                            device_rate,
                                            samples,
                                        ));

                                        // Do not count queued samples as played. `rodio`
                                        // cannot report its rendered sample cursor, so
                                        // anchor once at the first source PTS and use a
                                        // monotonic timeline until Pause or Seek.
                                        if video_si.is_none() && !audio_clock.is_output_active() {
                                            let frame_position = f
                                                .pts
                                                .map(|pts| {
                                                    let seconds = pts as f64 * f.time_base.to_f64();
                                                    Duration::from_secs_f64(seconds.max(0.0))
                                                })
                                                .unwrap_or(position);
                                            audio_clock.start_output_at(frame_position);
                                            position = audio_clock.now();
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
                            if let Ok(converted) = yuv420p_frame_to_rgba_cached(&f) {
                                let w = f.width;
                                let h = f.height;
                                if w > 0 && h > 0 {
                                    let needed_len = converted.len();
                                    let mut scratch = rgba_pool().get(needed_len);
                                    scratch.extend_from_slice(&converted);
                                    emit(
                                        event_tx,
                                        PlayerEvent::VideoFrame {
                                            width: w,
                                            height: h,
                                            rgba: scratch.clone(),
                                            pts: position,
                                            generation,
                                        },
                                    );
                                    rgba_pool().recycle(scratch);
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
                        let ch = f.channels.max(1);
                        let rate = f.sample_rate.max(1);
                        let samples = frame_to_s16_device(&f, rate, ch).unwrap_or_default();
                        if !samples.is_empty() {
                            if let Some(ref s) = sink {
                                s.append(rodio::buffer::SamplesBuffer::new(ch, rate, samples));
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
