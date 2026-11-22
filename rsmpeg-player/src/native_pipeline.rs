//! Native demux path: `rsmpeg-format` packets → backend OpenH264 / Symphonia decode.
//!
//! Used when `prefer_native_pipeline` is true and the container yields a real
//! sample index (currently non-fragmented MP4, and WAV PCM).

use std::sync::mpsc::{Receiver, SyncSender, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use rsmpeg_codec::CodecId;
use rsmpeg_format::FormatContext;
use rsmpeg_util::MediaType;

use crate::clock::{MasterClock, PlaybackClock};
use crate::command::PlayerCommand;
use crate::event::{compute_pcm_level, PlayerEvent, PlayerSnapshot, PlayerTrackInfo};
use crate::player::{EventSender, MAX_PLAYBACK_RATE, MIN_PLAYBACK_RATE};
use crate::video_scheduler::{ScheduleAction, VideoScheduler};

const MAX_PACE_SLEEP: Duration = Duration::from_millis(12);
const MAX_AUDIO_QUEUE_SOURCES: usize = 48;
const AUDIO_LEVEL_INTERVAL: Duration = Duration::from_millis(40);
const AUDIO_ONLY_LEAD_LIMIT: Duration = Duration::from_millis(250);

fn emit(tx: &EventSender, ev: PlayerEvent) {
    tx.send(ev);
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

/// Build selectable video and audio track lists from a parsed [`FormatContext`].
pub(crate) fn collect_native_tracks(
    ctx: &FormatContext,
) -> (Vec<PlayerTrackInfo>, Vec<PlayerTrackInfo>) {
    let mut video_tracks = Vec::new();
    let mut audio_tracks = Vec::new();
    for s in &ctx.streams {
        let language = s.metadata.get("language").map(ToOwned::to_owned);
        match s.media_type {
            MediaType::Video => {
                video_tracks.push(PlayerTrackInfo {
                    index: s.index,
                    is_video: true,
                    codec: s.codec_id.name().to_string(),
                    width: s.codec_params.width.filter(|&w| w > 0),
                    height: s.codec_params.height.filter(|&h| h > 0),
                    sample_rate: None,
                    channels: None,
                    language,
                    supported: s.codec_id == CodecId::H264,
                });
            }
            MediaType::Audio => {
                let supported = matches!(s.codec_id, CodecId::Aac | CodecId::Pcm | CodecId::Mp3);
                audio_tracks.push(PlayerTrackInfo {
                    index: s.index,
                    is_video: false,
                    codec: s.codec_id.name().to_string(),
                    width: None,
                    height: None,
                    sample_rate: s.codec_params.sample_rate.filter(|&sr| sr > 0),
                    channels: s.codec_params.channels.filter(|&ch| ch > 0),
                    language,
                    supported,
                });
            }
            _ => {}
        }
    }
    (video_tracks, audio_tracks)
}

#[cfg(all(
    feature = "backend-symphonia",
    feature = "backend-openh264",
    feature = "audio-rodio"
))]
fn open_native_video_decoder(
    ctx: &FormatContext,
    stream_idx: usize,
    generation: u64,
    event_tx: &EventSender,
) -> Option<crate::backend::OpenH264Decoder> {
    let stream = ctx.streams.get(stream_idx)?;
    if stream.codec_id != CodecId::H264 {
        emit(
            event_tx,
            PlayerEvent::Warning {
                message: format!(
                    "Unsupported video codec '{}' — skipping video",
                    stream.codec_id.name()
                ),
                generation,
            },
        );
        return None;
    }
    match crate::backend::OpenH264Decoder::from_params(&stream.codec_params) {
        Ok(d) => Some(d),
        Err(e) => {
            emit(
                event_tx,
                PlayerEvent::Warning {
                    message: format!("native path: failed to open OpenH264 decoder: {e}"),
                    generation,
                },
            );
            None
        }
    }
}

#[cfg(all(
    feature = "backend-symphonia",
    feature = "backend-openh264",
    feature = "audio-rodio"
))]
fn open_native_audio_decoder(
    ctx: &FormatContext,
    stream_idx: usize,
    generation: u64,
    event_tx: &EventSender,
) -> (
    Option<crate::backend::symphonia_audio::SymphoniaAudioDecoder>,
    u16,
    u32,
) {
    use crate::backend::symphonia_audio::SymphoniaAudioDecoder;

    let Some(stream) = ctx.streams.get(stream_idx) else {
        return (None, 2, 44_100);
    };
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
                generation,
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
                        generation,
                    },
                );
                (None, ch, rate)
            }
        }
    }
}

/// Try native demux. Returns `Ok(())` if the session finished on this path.
/// Returns `Err` when native demux is unavailable so the caller can fall back.
pub fn try_run_native(
    path: &std::path::Path,
    volume: f32,
    cmd_rx: &Receiver<PlayerCommand>,
    event_tx: &SyncSender<PlayerEvent>,
) -> Result<(), String> {
    let sender = EventSender::single(event_tx.clone());
    try_run_native_with_sender(path, volume, cmd_rx, &sender)
}

pub(crate) fn try_run_native_with_sender(
    path: &std::path::Path,
    volume: f32,
    cmd_rx: &Receiver<PlayerCommand>,
    event_tx: &EventSender,
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
    event_tx: &EventSender,
) -> Result<(), String> {
    use rodio::{OutputStream, Sink};
    use rsmpeg_codec::{DecodeStatus, Decoder};

    use crate::audio_convert::frame_to_s16_device;
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

    let mut video_si = ctx
        .streams
        .iter()
        .position(|s| s.media_type == MediaType::Video && s.codec_id == CodecId::H264);
    let mut audio_si = ctx.streams.iter().position(|s| {
        s.media_type == MediaType::Audio
            && matches!(
                s.codec_id,
                CodecId::Aac | CodecId::Pcm | CodecId::Mp3 | CodecId::Alac
            )
    });

    if video_si.is_none() && audio_si.is_none() {
        return Err("native demux: no H.264 video or supported audio stream".into());
    }

    let (video_tracks, audio_tracks) = collect_native_tracks(&ctx);

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
    let mut playback_rate = 1.0f64;
    let mut audio_master = MasterClock::new();
    audio_master.set_audio_master(true);
    let mut appended_samples: u64 = 0;
    let mut force_one_frame = false;
    let mut was_playing = true;
    let mut playback_clock = PlaybackClock::new();
    let mut clock_started = false;
    let mut last_level_emit: Option<Instant> = None;
    let mut video_frame_index = 0u64;
    let assumed_frame_dur = 1.0 / 30.0;
    // ~50 ms late-drop threshold (VideoScheduler::new default).
    let mut video_scheduler = VideoScheduler::new();

    // ── Video decoder (OpenH264 backend) ──
    let mut video_dec: Option<OpenH264Decoder> =
        video_si.and_then(|si| open_native_video_decoder(&ctx, si, generation, event_tx));

    // ── Audio decoder (Symphonia packet-in backend) ──
    let (mut audio_dec, mut audio_channels, mut audio_rate) = match audio_si {
        Some(si) => open_native_audio_decoder(&ctx, si, generation, event_tx),
        None => (None, 2u16, 44_100u32),
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
        PlayerEvent::Tracks {
            video_tracks: video_tracks.clone(),
            audio_tracks: audio_tracks.clone(),
            active_video: video_si,
            active_audio: audio_si,
            generation,
        },
    );
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

    let mut video_time_base = video_si.map(|si| {
        let tb = &ctx.streams[si].time_base;
        tb.num as f64 / tb.den.max(1) as f64
    });
    // Establish a stable 0-based media time origin before any seek can run
    // (MP4Demuxer builds 0-based DTS/PTS and `ctx.seek(ts_ms)` maps directly
    // against that origin).
    let mut base_video_pts: Option<i64> = video_si.map(|_| 0i64);
    let mut deferred_cmd: Option<PlayerCommand> = None;
    let mut eof_draining = false;

    loop {
        // ── Commands ──
        loop {
            let next_cmd = match deferred_cmd.take() {
                Some(cmd) => Ok(cmd),
                None => cmd_rx.try_recv(),
            };
            match next_cmd {
                Ok(cmd) => {
                    let g = cmd.generation();
                    match cmd {
                        PlayerCommand::Play { .. } => {
                            if !was_playing {
                                if !clock_started {
                                    playback_clock.seek(position);
                                    playback_clock.start();
                                    clock_started = true;
                                } else {
                                    playback_clock.resume();
                                }
                                audio_master.resume();
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
                            playback_clock.pause();
                            audio_master.pause();
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
                                s.set_speed(playback_rate as f32);
                                if playing {
                                    s.play();
                                } else {
                                    s.pause();
                                }
                            }
                            playback_clock.seek(position);
                            if !playing {
                                playback_clock.pause();
                            }
                            clock_started = true;
                            audio_master.seek_to(position);
                            if !playing {
                                audio_master.pause();
                            }
                            appended_samples =
                                MasterClock::duration_to_audio_samples(position, audio_rate);
                            video_frame_index = 0;
                            force_one_frame = video_dec.is_some();
                            eof_draining = false;
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
                        } => {
                            playback_rate = if rate.is_finite() {
                                rate.clamp(MIN_PLAYBACK_RATE, MAX_PLAYBACK_RATE)
                            } else {
                                1.0
                            };
                            playback_clock.set_rate(playback_rate);
                            if let Some(ref s) = sink {
                                s.set_speed(playback_rate as f32);
                            }
                            emit(
                                event_tx,
                                snap(playing, position, duration, volume, g, "rate"),
                            );
                        }
                        PlayerCommand::SelectAudioTrack {
                            index,
                            generation: g,
                        } => {
                            generation = g;
                            let valid = ctx
                                .streams
                                .get(index)
                                .is_some_and(|s| s.media_type == MediaType::Audio);
                            if !valid {
                                emit(
                                    event_tx,
                                    PlayerEvent::Warning {
                                        message: format!("invalid audio track index {index}"),
                                        generation: g,
                                    },
                                );
                            } else {
                                audio_si = Some(index);
                                let (dec, ch, rate) =
                                    open_native_audio_decoder(&ctx, index, g, event_tx);
                                audio_dec = dec;
                                audio_channels = ch;
                                audio_rate = rate;
                                if eof_draining {
                                    let ts_ms = (position.as_secs_f64().max(0.0) * 1000.0) as i64;
                                    let _ = ctx.seek(ts_ms);
                                    pending = ctx.read_frame().ok().flatten();
                                    eof_draining = false;
                                }
                                audio_master.set_audio_sample_rate(audio_rate);
                                appended_samples =
                                    MasterClock::duration_to_audio_samples(position, audio_rate);
                                if let Some(ref s) = sink {
                                    s.clear();
                                    s.set_volume(volume);
                                    s.set_speed(playback_rate as f32);
                                    if playing {
                                        s.play();
                                    } else {
                                        s.pause();
                                    }
                                }
                                emit(
                                    event_tx,
                                    PlayerEvent::Tracks {
                                        video_tracks: video_tracks.clone(),
                                        audio_tracks: audio_tracks.clone(),
                                        active_video: video_si,
                                        active_audio: audio_si,
                                        generation: g,
                                    },
                                );
                                emit(
                                    event_tx,
                                    snap(playing, position, duration, volume, g, "audio-track"),
                                );
                            }
                        }
                        PlayerCommand::SelectVideoTrack {
                            index,
                            generation: g,
                        } => {
                            generation = g;
                            let valid = ctx
                                .streams
                                .get(index)
                                .is_some_and(|s| s.media_type == MediaType::Video);
                            if !valid {
                                emit(
                                    event_tx,
                                    PlayerEvent::Warning {
                                        message: format!("invalid video track index {index}"),
                                        generation: g,
                                    },
                                );
                            } else {
                                video_si = Some(index);
                                video_dec = open_native_video_decoder(&ctx, index, g, event_tx);
                                video_time_base = Some({
                                    let tb = &ctx.streams[index].time_base;
                                    tb.num as f64 / tb.den.max(1) as f64
                                });
                                base_video_pts = Some(0);
                                let ts_ms = (position.as_secs_f64().max(0.0) * 1000.0) as i64;
                                let _ = ctx.seek(ts_ms);
                                pending = ctx.read_frame().ok().flatten();
                                if let Some(ref mut d) = audio_dec {
                                    let _ = d.reset();
                                }
                                if let Some(ref s) = sink {
                                    s.clear();
                                    s.set_volume(volume);
                                    s.set_speed(playback_rate as f32);
                                    if playing {
                                        s.play();
                                    } else {
                                        s.pause();
                                    }
                                }
                                playback_clock.seek(position);
                                if !playing {
                                    playback_clock.pause();
                                }
                                clock_started = true;
                                video_scheduler.reset_stats();
                                force_one_frame = video_dec.is_some();
                                eof_draining = false;
                                emit(
                                    event_tx,
                                    PlayerEvent::Tracks {
                                        video_tracks: video_tracks.clone(),
                                        audio_tracks: audio_tracks.clone(),
                                        active_video: video_si,
                                        active_audio: audio_si,
                                        generation: g,
                                    },
                                );
                                emit(
                                    event_tx,
                                    snap(playing, position, duration, volume, g, "video-track"),
                                );
                            }
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

        if eof_draining {
            let sink_empty = sink.as_ref().map_or(true, |s| s.empty());
            if sink_empty {
                break;
            }
            if video_si.is_none() && playing && clock_started {
                let now = playback_clock.now();
                position = if duration > Duration::ZERO {
                    now.min(duration)
                } else {
                    now
                };
                emit(
                    event_tx,
                    PlayerEvent::PositionChanged {
                        position,
                        generation,
                    },
                );
            }
            thread::sleep(Duration::from_millis(12));
            continue;
        }

        if video_si.is_none() && playing && clock_started {
            let queued_pos =
                PlaybackClock::duration_from_audio_samples(appended_samples, audio_rate);
            let now = playback_clock.now();
            if duration > Duration::ZERO {
                position = now.min(duration);
            } else {
                position = now;
            }
            if queued_pos > now.saturating_add(AUDIO_ONLY_LEAD_LIMIT) {
                emit(
                    event_tx,
                    PlayerEvent::PositionChanged {
                        position,
                        generation,
                    },
                );
                thread::sleep(Duration::from_millis(8));
                continue;
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
                Ok(None) | Err(_) => {
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
                                                emit(
                                                    event_tx,
                                                    PlayerEvent::VideoFrame {
                                                        width: w,
                                                        height: h,
                                                        rgba: Arc::new(converted),
                                                        pts: position,
                                                        generation,
                                                    },
                                                );
                                            }
                                        }
                                    }
                                    Ok(DecodeStatus::NeedMoreInput)
                                    | Ok(DecodeStatus::EndOfStream) => break,
                                    Err(_) => break,
                                }
                            }
                        }
                    }
                    // Soft-flush audio decoder (append tail to sink, then drain inside main loop).
                    if let Some(ref mut dec) = audio_dec {
                        let _ = dec.send_packet(None);
                        loop {
                            match dec.receive_frame() {
                                Ok(DecodeStatus::Frame(f)) => {
                                    let ch = f.channels.max(1);
                                    let rate = f.sample_rate.max(1);
                                    let samples =
                                        frame_to_s16_device(&f, rate, ch).unwrap_or_default();
                                    if !samples.is_empty() {
                                        if let Some(ref s) = sink {
                                            s.append(rodio::buffer::SamplesBuffer::new(
                                                ch, rate, samples,
                                            ));
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
                    force_one_frame = false;
                    eof_draining = true;
                    continue;
                }
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
                                            let base = *base_video_pts.get_or_insert(0);
                                            pts.saturating_sub(base) as f64 * spt
                                        }
                                        _ => video_frame_index as f64 * assumed_frame_dur,
                                    };
                                    let frame_pts = Duration::from_secs_f64(abs_pos.max(0.0));
                                    // Seek preview: always display without wait/drop.
                                    let mut present = true;
                                    if !force_one_frame {
                                        if !clock_started {
                                            playback_clock.start();
                                            clock_started = true;
                                        }
                                        let now = playback_clock.now();
                                        match video_scheduler.schedule(frame_pts, now) {
                                            ScheduleAction::DropLate => {
                                                present = false;
                                            }
                                            ScheduleAction::Wait { duration } => {
                                                // Scale wait time by playback_rate; cap so bad PTS cannot freeze.
                                                let scaled_wait = duration
                                                    .div_f64(playback_rate.max(MIN_PLAYBACK_RATE));
                                                let mut rem =
                                                    scaled_wait.min(Duration::from_millis(500));
                                                while rem > Duration::ZERO {
                                                    thread::sleep(rem.min(MAX_PACE_SLEEP));
                                                    rem = rem.saturating_sub(MAX_PACE_SLEEP);
                                                    match cmd_rx.try_recv() {
                                                        Ok(
                                                            PlayerCommand::Stop { .. }
                                                            | PlayerCommand::Shutdown { .. },
                                                        )
                                                        | Err(TryRecvError::Disconnected) => {
                                                            stop = true;
                                                            present = false;
                                                            break;
                                                        }
                                                        Ok(other) => {
                                                            deferred_cmd = Some(other);
                                                            present = false;
                                                            break;
                                                        }
                                                        Err(TryRecvError::Empty) => {}
                                                    }
                                                    if !playing && !force_one_frame {
                                                        present = false;
                                                        break;
                                                    }
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
                                            // Move the freshly converted plane straight into the
                                            // event: wrapping it in an `Arc` makes later clones
                                            // (e.g. duplicate-frame repeats) a refcount bump
                                            // instead of a full-frame copy.
                                            emit(
                                                event_tx,
                                                PlayerEvent::VideoFrame {
                                                    width: w,
                                                    height: h,
                                                    rgba: Arc::new(converted),
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
                                if stop || deferred_cmd.is_some() {
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
                                let sample_count = samples.len();
                                if !samples.is_empty() {
                                    let should_emit_level = last_level_emit
                                        .map_or(true, |t| t.elapsed() >= AUDIO_LEVEL_INTERVAL);
                                    if should_emit_level {
                                        let (rms, peak) = compute_pcm_level(&samples);
                                        emit(
                                            event_tx,
                                            PlayerEvent::AudioLevel {
                                                rms,
                                                peak,
                                                generation,
                                            },
                                        );
                                        last_level_emit = Some(Instant::now());
                                    }
                                    if let Some(ref s) = sink {
                                        s.append(rodio::buffer::SamplesBuffer::new(
                                            device_ch,
                                            device_rate,
                                            samples,
                                        ));
                                    }
                                }
                                // Drive position from audio when no video.
                                if video_si.is_none() {
                                    if !clock_started {
                                        playback_clock.seek(position);
                                        playback_clock.start();
                                        clock_started = true;
                                    }
                                    if audio_master.audio_sample_rate() == 0 {
                                        audio_master.set_audio_sample_rate(device_rate);
                                    }
                                    // samples is interleaved i16; frames = len / channels.
                                    appended_samples += sample_count as u64 / device_ch as u64;
                                    audio_master.set_audio_samples_played(appended_samples);
                                    let now = playback_clock.now();
                                    position = if duration > Duration::ZERO {
                                        now.min(duration)
                                    } else {
                                        now
                                    };
                                    emit(
                                        event_tx,
                                        PlayerEvent::PositionChanged {
                                            position,
                                            generation,
                                        },
                                    );
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
        emit(event_tx, PlayerEvent::Ended { generation });
    } else {
        emit(
            event_tx,
            snap(false, position, duration, volume, generation, "stopped"),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsmpeg_format::Stream;
    use rsmpeg_util::Dict;

    #[test]
    fn collect_native_tracks_splits_video_and_audio_with_metadata() {
        let mut ctx = FormatContext {
            input: None,
            output: None,
            streams: Vec::new(),
            io: None,
            metadata: Dict::new(),
            duration: 0,
            bit_rate: 0,
            filename: None,
            format_name: Some("mp4".into()),
        };

        let mut v0 = Stream::new(0, CodecId::H264);
        v0.media_type = MediaType::Video;
        v0.codec_params.width = Some(1280);
        v0.codec_params.height = Some(720);
        ctx.add_stream(v0);

        let mut v1 = Stream::new(1, CodecId::Hevc);
        v1.media_type = MediaType::Video;
        v1.codec_params.width = Some(1920);
        v1.codec_params.height = Some(1080);
        ctx.add_stream(v1);

        let mut a0 = Stream::new(2, CodecId::Aac);
        a0.media_type = MediaType::Audio;
        a0.codec_params.sample_rate = Some(48_000);
        a0.codec_params.channels = Some(2);
        a0.metadata.insert("language", "eng");
        ctx.add_stream(a0);

        let mut a1 = Stream::new(3, CodecId::Aac);
        a1.media_type = MediaType::Audio;
        a1.codec_params.sample_rate = Some(44_100);
        a1.codec_params.channels = Some(2);
        a1.metadata.insert("language", "zho");
        ctx.add_stream(a1);

        let (v, a) = collect_native_tracks(&ctx);
        assert_eq!(v.len(), 2);
        assert_eq!(a.len(), 2);
        assert!(v[0].supported);
        assert_eq!(v[0].width, Some(1280));
        assert!(!v[1].supported);
        assert_eq!(a[0].language.as_deref(), Some("eng"));
        assert_eq!(a[1].language.as_deref(), Some("zho"));
        assert!(a[0].supported);
    }
}
