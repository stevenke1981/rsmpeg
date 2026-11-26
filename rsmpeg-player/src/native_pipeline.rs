//! Native demux path: `rsmpeg-format` packets → rsmpeg or backend decoders.
//!
//! Used when `prefer_native_pipeline` is true (always, without the
//! `backend-symphonia` fallback) and the container yields a real sample index.
//!
//! It builds under every feature combination. MJPEG, raw video, PCM and FLAC
//! are always decoded; H.264 needs `backend-openh264`; AAC, MP3, Vorbis and
//! ALAC need `backend-symphonia`; and sound output needs `audio-rodio`.

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

fn emit_preview(tx: &EventSender, ev: PlayerEvent) {
    tx.send_preview(ev);
}

fn emit_native_video_frame(
    event_tx: &EventSender,
    frame: &rsmpeg_codec::Frame,
    pts: Duration,
    generation: u64,
    is_preview: bool,
) -> bool {
    let w = frame.width;
    let h = frame.height;
    if w == 0 || h == 0 {
        return false;
    }
    let Ok(converted) = crate::video_convert::frame_to_rgba_cached(frame) else {
        return false;
    };
    let ev = PlayerEvent::VideoFrame {
        width: w,
        height: h,
        rgba: Arc::new(converted),
        pts,
        generation,
    };
    if is_preview {
        emit_preview(event_tx, ev);
    } else {
        emit(event_tx, ev);
    }
    true
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

/// Audio output queue: a `rodio::Sink` with the `audio-rodio` feature.
///
/// Without that feature [`AudioSink::try_open_sink`] always returns `None`, so
/// playback runs silently and the other methods are never reached.
#[cfg_attr(not(feature = "audio-rodio"), allow(dead_code))]
pub(crate) struct AudioSink {
    #[cfg(feature = "audio-rodio")]
    inner: rodio::Sink,
}

#[cfg_attr(
    not(feature = "audio-rodio"),
    allow(dead_code, unused_variables, clippy::unused_self)
)]
impl AudioSink {
    /// Open a sink on the default output device at `volume`, if there is one.
    pub(crate) fn try_open_sink(volume: f32) -> Option<Self> {
        #[cfg(feature = "audio-rodio")]
        {
            crate::audio_output::try_open_sink(volume).map(|inner| Self { inner })
        }
        #[cfg(not(feature = "audio-rodio"))]
        {
            None
        }
    }

    pub(crate) fn play(&self) {
        #[cfg(feature = "audio-rodio")]
        self.inner.play();
    }

    pub(crate) fn pause(&self) {
        #[cfg(feature = "audio-rodio")]
        self.inner.pause();
    }

    /// Discard everything queued.
    pub(crate) fn clear(&self) {
        #[cfg(feature = "audio-rodio")]
        self.inner.clear();
    }

    pub(crate) fn set_volume(&self, volume: f32) {
        #[cfg(feature = "audio-rodio")]
        self.inner.set_volume(volume);
    }

    pub(crate) fn set_speed(&self, speed: f32) {
        #[cfg(feature = "audio-rodio")]
        self.inner.set_speed(speed);
    }

    /// Number of queued buffers.
    pub(crate) fn len(&self) -> usize {
        #[cfg(feature = "audio-rodio")]
        {
            self.inner.len()
        }
        #[cfg(not(feature = "audio-rodio"))]
        {
            0
        }
    }

    /// Whether everything queued has been played.
    pub(crate) fn empty(&self) -> bool {
        #[cfg(feature = "audio-rodio")]
        {
            self.inner.empty()
        }
        #[cfg(not(feature = "audio-rodio"))]
        {
            true
        }
    }

    /// Queue interleaved signed 16-bit PCM.
    pub(crate) fn append_i16(&self, channels: u16, rate: u32, samples: Vec<i16>) {
        #[cfg(feature = "audio-rodio")]
        self.inner
            .append(rodio::buffer::SamplesBuffer::new(channels, rate, samples));
    }
}

/// Video codecs the native path can open a decoder for.
fn is_native_video_codec(codec_id: CodecId) -> bool {
    matches!(codec_id, CodecId::Mjpeg | CodecId::RawVideo)
        || (cfg!(feature = "backend-openh264") && codec_id == CodecId::H264)
}

/// Audio codecs the native path can open a decoder for.
fn is_native_audio_codec(codec_id: CodecId) -> bool {
    matches!(codec_id, CodecId::Flac | CodecId::Pcm)
        || (cfg!(feature = "backend-symphonia")
            && matches!(
                codec_id,
                CodecId::Aac | CodecId::Mp3 | CodecId::Vorbis | CodecId::Alac
            ))
}

/// Trim interleaved audio that plays before a precise-seek `target`.
///
/// `frame_start` is the media time of `samples[0]`. Leading sample frames that
/// precede `target` (to the nearest sample) are removed in place.
///
/// Returns `true` when output remains, meaning playback has reached `target`.
/// Returns `false` when nothing remains: the whole frame ends at or before
/// `target` (and `samples` is cleared), or `samples` was already empty. With a
/// zero `sample_rate` or `channels` the timeline is unknown and `samples` is
/// left untouched.
pub(crate) fn trim_audio_samples_before_target(
    frame_start: Duration,
    sample_rate: u32,
    channels: u16,
    target: Duration,
    samples: &mut Vec<i16>,
) -> bool {
    if target <= frame_start || sample_rate == 0 || channels == 0 {
        return !samples.is_empty();
    }
    let channels = usize::from(channels);
    let frames = samples.len() / channels;
    let frame_len = Duration::from_secs_f64(frames as f64 / f64::from(sample_rate));
    let frame_end = frame_start.saturating_add(frame_len);
    // Round to the nearest sample: timestamps carry nanosecond truncation.
    let skip = if frame_end <= target {
        frames
    } else {
        let lead_nanos = (target - frame_start).as_nanos();
        let rounded = (lead_nanos * u128::from(sample_rate) + 500_000_000) / 1_000_000_000;
        usize::try_from(rounded).map_or(frames, |n| n.min(frames))
    };
    if skip >= frames {
        samples.clear();
        return false;
    }
    samples.drain(..skip * channels);
    true
}

/// Media time of a timestamp expressed in `time_base` units, or `None` when it
/// cannot be determined. Negative timestamps clamp to zero.
fn timestamp_to_duration(ts: Option<i64>, time_base: rsmpeg_util::Rational) -> Option<Duration> {
    if time_base.num <= 0 || time_base.den <= 0 {
        return None;
    }
    let secs = ts? as f64 * f64::from(time_base.num) / f64::from(time_base.den);
    Duration::try_from_secs_f64(secs.max(0.0)).ok()
}

/// Apply the pending precise-seek target to one decoded audio frame, clearing
/// `drop_until` once the target has been reached. A frame without a usable
/// timestamp cannot be placed on the timeline, so it ends trimming.
fn apply_audio_seek_trim(
    drop_until: &mut Option<Duration>,
    frame_start: Option<Duration>,
    sample_rate: u32,
    channels: u16,
    samples: &mut Vec<i16>,
) {
    let Some(target) = *drop_until else {
        return;
    };
    let reached = match frame_start {
        Some(start) => {
            trim_audio_samples_before_target(start, sample_rate, channels, target, samples)
        }
        None => true,
    };
    if reached {
        *drop_until = None;
    }
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
                let supported = is_native_video_codec(s.codec_id);
                video_tracks.push(PlayerTrackInfo {
                    index: s.index,
                    is_video: true,
                    codec: s.codec_id.name().to_string(),
                    width: s.codec_params.width.filter(|&w| w > 0),
                    height: s.codec_params.height.filter(|&h| h > 0),
                    sample_rate: None,
                    channels: None,
                    language,
                    supported,
                });
            }
            MediaType::Audio => {
                let supported = is_native_audio_codec(s.codec_id);
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

type NativeVideoDecoderBundle = (Option<Box<dyn rsmpeg_codec::Decoder>>, Option<String>);

type NativeAudioDecoderBundle = (
    Option<Box<dyn rsmpeg_codec::Decoder>>,
    Option<String>,
    u16,
    u32,
);

fn open_native_video_decoder(
    ctx: &FormatContext,
    stream_idx: usize,
    generation: u64,
    event_tx: &EventSender,
) -> NativeVideoDecoderBundle {
    let Some(stream) = ctx.streams.get(stream_idx) else {
        return (None, None);
    };
    match stream.codec_id {
        CodecId::Mjpeg => match rsmpeg_codec::MjpegDecoder::from_params(&stream.codec_params) {
            Ok(d) => (Some(Box::new(d)), Some("rsmpeg-rust (MJPEG)".into())),
            Err(e) => {
                emit(
                    event_tx,
                    PlayerEvent::Warning {
                        message: format!("native path: failed to open MJPEG decoder: {e}"),
                        generation,
                    },
                );
                (None, None)
            }
        },
        CodecId::RawVideo => {
            let mut d = rsmpeg_codec::RawVideoDecoder::new();
            d.set_parameters(stream.codec_params.clone());
            (Some(Box::new(d)), Some("rsmpeg-rust (RawVideo)".into()))
        }
        #[cfg(feature = "backend-openh264")]
        CodecId::H264 => match crate::backend::OpenH264Decoder::from_params(&stream.codec_params) {
            Ok(d) => (Some(Box::new(d)), Some("OpenH264 (H.264)".into())),
            Err(e) => {
                emit(
                    event_tx,
                    PlayerEvent::Warning {
                        message: format!("native path: failed to open OpenH264 decoder: {e}"),
                        generation,
                    },
                );
                (None, None)
            }
        },
        other => {
            emit(
                event_tx,
                PlayerEvent::Warning {
                    message: format!(
                        "Unsupported video codec '{}' — skipping video",
                        other.name()
                    ),
                    generation,
                },
            );
            (None, None)
        }
    }
}

fn open_native_audio_decoder(
    ctx: &FormatContext,
    stream_idx: usize,
    generation: u64,
    event_tx: &EventSender,
) -> NativeAudioDecoderBundle {
    let Some(stream) = ctx.streams.get(stream_idx) else {
        return (None, None, 2, 44_100);
    };
    let rate = stream.codec_params.sample_rate.unwrap_or(48_000);
    let ch = stream.codec_params.channels.unwrap_or(2);
    match stream.codec_id {
        CodecId::Flac => match rsmpeg_codec::FlacDecoder::from_params(&stream.codec_params) {
            Ok(d) => (
                Some(Box::new(d)),
                Some("rsmpeg-rust (FLAC)".into()),
                ch,
                rate,
            ),
            Err(e) => {
                emit(
                    event_tx,
                    PlayerEvent::Warning {
                        message: format!("native path: failed to open FLAC decoder: {e}"),
                        generation,
                    },
                );
                (None, None, ch, rate)
            }
        },
        CodecId::Pcm => match rsmpeg_codec::PCMAudioDecoder::from_params(&stream.codec_params) {
            Ok(d) => (
                Some(Box::new(d)),
                Some("rsmpeg-rust (PCM)".into()),
                ch,
                rate,
            ),
            Err(e) => {
                emit(
                    event_tx,
                    PlayerEvent::Warning {
                        message: format!("native path: failed to open PCM decoder: {e}"),
                        generation,
                    },
                );
                (None, None, ch, rate)
            }
        },
        #[cfg(feature = "backend-symphonia")]
        CodecId::Aac | CodecId::Mp3 | CodecId::Vorbis | CodecId::Alac => {
            match crate::backend::SymphoniaAudioDecoder::try_new(&stream.codec_params) {
                Ok(d) => (
                    Some(Box::new(d)),
                    Some(format!(
                        "Symphonia ({})",
                        stream.codec_id.name().to_ascii_uppercase()
                    )),
                    ch,
                    rate,
                ),
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
                    (None, None, ch, rate)
                }
            }
        }
        other => {
            emit(
                event_tx,
                PlayerEvent::Warning {
                    message: format!(
                        "native audio codec '{}' not wired — audio muted",
                        other.name()
                    ),
                    generation,
                },
            );
            (None, None, ch, rate)
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
    run_native_inner(path, volume, cmd_rx, event_tx)
}

fn run_native_inner(
    path: &std::path::Path,
    mut volume: f32,
    cmd_rx: &Receiver<PlayerCommand>,
    event_tx: &EventSender,
) -> Result<(), String> {
    use rsmpeg_codec::DecodeStatus;

    use crate::audio_convert::frame_to_s16_device;

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
    let native_ok = matches!(
        format_name.as_str(),
        "mp4"
            | "mov"
            | "m4a"
            | "m4v"
            | "wav"
            | "flac"
            | "matroska"
            | "mkv"
            | "webm"
            | "mka"
            | "avi"
            | "y4m"
    );
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
        .position(|s| s.media_type == MediaType::Video && is_native_video_codec(s.codec_id));
    let mut audio_si = ctx
        .streams
        .iter()
        .position(|s| s.media_type == MediaType::Audio && is_native_audio_codec(s.codec_id));

    if video_si.is_none() && audio_si.is_none() {
        return Err("native demux: no supported video or audio stream".into());
    }

    let (video_tracks, audio_tracks) = collect_native_tracks(&ctx);

    if let Some(u) = ctx.streams.iter().find(|s| {
        s.media_type == MediaType::Video
            && !is_native_video_codec(s.codec_id)
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
    let mut drop_until: Option<Duration> = None;
    let mut last_dropped_frame: Option<(rsmpeg_codec::Frame, Duration)> = None;
    // Precise-seek target still to be reached by the audio stream.
    let mut audio_drop_until: Option<Duration> = None;

    // ── Video decoder ──
    let (mut video_dec, mut video_dec_name) = match video_si {
        Some(si) => open_native_video_decoder(&ctx, si, generation, event_tx),
        None => (None, None),
    };

    // ── Audio decoder ──
    let (mut audio_dec, mut audio_dec_name, mut audio_channels, mut audio_rate) = match audio_si {
        Some(si) => open_native_audio_decoder(&ctx, si, generation, event_tx),
        None => (None, None, 2u16, 44_100u32),
    };

    let mut sink = audio_si.and_then(|_| AudioSink::try_open_sink(volume));

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
        PlayerEvent::DecoderInfo {
            demuxer: format!("rsmpeg-format ({format_name})"),
            video_decoder: video_dec_name.clone(),
            audio_decoder: audio_dec_name.clone(),
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
                            mode,
                            generation: g,
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
                            drop_until =
                                (mode == crate::command::SeekMode::Precise).then_some(position);
                            audio_drop_until = drop_until;
                            last_dropped_frame = None;
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
                        PlayerCommand::StepForward { generation: g } => {
                            generation = g;
                            playing = false;
                            was_playing = false;
                            playback_clock.pause();
                            audio_master.pause();
                            if let Some(ref s) = sink {
                                s.pause();
                            }
                            force_one_frame = video_dec.is_some();
                            emit(
                                event_tx,
                                snap(false, position, duration, volume, g, "paused"),
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
                                audio_drop_until = None;
                                let (dec, dec_name, ch, rate) =
                                    open_native_audio_decoder(&ctx, index, g, event_tx);
                                audio_dec = dec;
                                audio_dec_name = dec_name;
                                audio_channels = ch;
                                audio_rate = rate;
                                if audio_dec.is_some() && sink.is_none() {
                                    sink = AudioSink::try_open_sink(volume);
                                }
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
                                    PlayerEvent::DecoderInfo {
                                        demuxer: format!("rsmpeg-format ({format_name})"),
                                        video_decoder: video_dec_name.clone(),
                                        audio_decoder: audio_dec_name.clone(),
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
                                let (dec, dec_name) =
                                    open_native_video_decoder(&ctx, index, g, event_tx);
                                video_dec = dec;
                                video_dec_name = dec_name;
                                drop_until = None;
                                audio_drop_until = None;
                                last_dropped_frame = None;
                                video_time_base = Some({
                                    let tb = &ctx.streams[index].time_base;
                                    tb.num as f64 / tb.den.max(1) as f64
                                });
                                base_video_pts = Some(0);
                                appended_samples =
                                    MasterClock::duration_to_audio_samples(position, audio_rate);
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
                                    PlayerEvent::DecoderInfo {
                                        demuxer: format!("rsmpeg-format ({format_name})"),
                                        video_decoder: video_dec_name.clone(),
                                        audio_decoder: audio_dec_name.clone(),
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
            if video_dec.is_none() && playing && clock_started {
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

        if video_dec.is_none() && playing && clock_started {
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
                    let mut pending_flushed_preview: Option<(rsmpeg_codec::Frame, Duration)> = None;
                    if let Some(ref mut dec) = video_dec {
                        if dec.send_packet(None).is_ok() {
                            loop {
                                match dec.receive_frame() {
                                    Ok(DecodeStatus::Frame(f)) => {
                                        let w = f.width;
                                        let h = f.height;
                                        if w > 0 && h > 0 {
                                            let frame_pts = match (f.pts, video_time_base) {
                                                (Some(pts), Some(spt))
                                                    if spt > 0.0 && spt.is_finite() =>
                                                {
                                                    let base = *base_video_pts.get_or_insert(0);
                                                    Duration::from_secs_f64(
                                                        (pts.saturating_sub(base) as f64 * spt)
                                                            .max(0.0),
                                                    )
                                                }
                                                _ => position,
                                            };
                                            if let Some(t) = drop_until {
                                                if video_scheduler.drop_before_seek(frame_pts, t) {
                                                    if force_one_frame {
                                                        pending_flushed_preview =
                                                            Some((f, frame_pts));
                                                    }
                                                    continue;
                                                }
                                                drop_until = None;
                                                last_dropped_frame = None;
                                            }
                                            if emit_native_video_frame(
                                                event_tx,
                                                &f,
                                                frame_pts,
                                                generation,
                                                force_one_frame,
                                            ) {
                                                position = frame_pts;
                                                if force_one_frame {
                                                    if !playing {
                                                        playback_clock.seek(position);
                                                    }
                                                    force_one_frame = false;
                                                    emit(
                                                        event_tx,
                                                        PlayerEvent::PositionChanged {
                                                            position,
                                                            generation,
                                                        },
                                                    );
                                                }
                                                pending_flushed_preview = None;
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
                    if force_one_frame {
                        if let Some((f, frame_pts)) =
                            pending_flushed_preview.or_else(|| last_dropped_frame.take())
                        {
                            if emit_native_video_frame(event_tx, &f, frame_pts, generation, true) {
                                position = frame_pts;
                                if !playing {
                                    playback_clock.seek(position);
                                }
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
                    drop_until = None;
                    last_dropped_frame = None;
                    // Soft-flush audio decoder (append tail to sink, then drain inside main loop).
                    if let Some(ref mut dec) = audio_dec {
                        let _ = dec.send_packet(None);
                        let audio_time_base = audio_si
                            .and_then(|si| ctx.streams.get(si))
                            .map(|s| s.time_base);
                        loop {
                            match dec.receive_frame() {
                                Ok(DecodeStatus::Frame(f)) => {
                                    let ch = f.channels.max(1);
                                    let rate = f.sample_rate.max(1);
                                    let mut samples =
                                        frame_to_s16_device(&f, rate, ch).unwrap_or_default();
                                    apply_audio_seek_trim(
                                        &mut audio_drop_until,
                                        audio_time_base
                                            .and_then(|tb| timestamp_to_duration(f.pts, tb)),
                                        rate,
                                        ch,
                                        &mut samples,
                                    );
                                    if !samples.is_empty() {
                                        if let Some(ref s) = sink {
                                            s.append_i16(ch, rate, samples);
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
                    audio_drop_until = None;
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
                                    if let Some(t) = drop_until {
                                        if video_scheduler.drop_before_seek(frame_pts, t) {
                                            video_frame_index += 1;
                                            if force_one_frame {
                                                last_dropped_frame = Some((f, frame_pts));
                                            }
                                            continue;
                                        }
                                        drop_until = None;
                                        last_dropped_frame = None;
                                    }
                                    // Seek / step preview: always display without wait/drop.
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
                                    if (present || force_one_frame)
                                        && emit_native_video_frame(
                                            event_tx,
                                            &f,
                                            frame_pts,
                                            generation,
                                            force_one_frame,
                                        )
                                    {
                                        position = frame_pts;
                                        if force_one_frame && !playing {
                                            playback_clock.seek(position);
                                        }
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
                    let audio_time_base = ctx.streams.get(si).map(|s| s.time_base);
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
                                let mut samples = frame_to_s16_device(&f, device_rate, device_ch)
                                    .unwrap_or_default();
                                let frame_start = audio_time_base.and_then(|tb| {
                                    timestamp_to_duration(f.pts.or(packet.pts).or(packet.dts), tb)
                                });
                                apply_audio_seek_trim(
                                    &mut audio_drop_until,
                                    frame_start,
                                    device_rate,
                                    device_ch,
                                    &mut samples,
                                );
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
                                        s.append_i16(device_ch, device_rate, samples);
                                    }
                                }
                                // Drive position from audio when no video is decoded.
                                if video_dec.is_none() {
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
        assert_eq!(v[0].supported, cfg!(feature = "backend-openh264"));
        assert_eq!(v[0].width, Some(1280));
        assert!(!v[1].supported);
        assert_eq!(a[0].language.as_deref(), Some("eng"));
        assert_eq!(a[1].language.as_deref(), Some("zho"));
        assert_eq!(a[0].supported, cfg!(feature = "backend-symphonia"));
    }

    #[test]
    fn collect_native_tracks_marks_pure_rust_codecs_supported() {
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

        for (idx, codec) in [CodecId::Mjpeg, CodecId::RawVideo].into_iter().enumerate() {
            let mut s = Stream::new(idx, codec);
            s.media_type = MediaType::Video;
            ctx.add_stream(s);
        }
        for (idx, codec) in [CodecId::Flac, CodecId::Pcm, CodecId::Opus]
            .into_iter()
            .enumerate()
        {
            let mut s = Stream::new(idx + 2, codec);
            s.media_type = MediaType::Audio;
            ctx.add_stream(s);
        }

        let (v, a) = collect_native_tracks(&ctx);
        assert!(v[0].supported, "Mjpeg should be supported");
        assert!(v[1].supported, "RawVideo should be supported");
        assert!(a[0].supported, "Flac should be supported");
        assert!(a[1].supported, "Pcm should be supported");
        assert!(!a[2].supported, "Opus should remain unsupported");
    }

    #[test]
    fn collect_native_tracks_covers_every_wired_audio_codec() {
        let mut ctx = FormatContext {
            input: None,
            output: None,
            streams: Vec::new(),
            io: None,
            metadata: Dict::new(),
            duration: 0,
            bit_rate: 0,
            filename: None,
            format_name: Some("matroska".into()),
        };
        let symphonia = cfg!(feature = "backend-symphonia");
        let codecs = [
            (CodecId::Flac, true),
            (CodecId::Pcm, true),
            (CodecId::Aac, symphonia),
            (CodecId::Mp3, symphonia),
            (CodecId::Vorbis, symphonia),
            (CodecId::Alac, symphonia),
            (CodecId::Opus, false),
            (CodecId::Unknown, false),
        ];
        for (idx, (codec, _)) in codecs.into_iter().enumerate() {
            let mut s = Stream::new(idx, codec);
            s.media_type = MediaType::Audio;
            ctx.add_stream(s);
        }
        // Non-A/V streams are not selectable.
        let mut data = Stream::new(codecs.len(), CodecId::Srt);
        data.media_type = MediaType::Subtitle;
        ctx.add_stream(data);

        let (v, a) = collect_native_tracks(&ctx);
        assert!(v.is_empty());
        assert_eq!(a.len(), codecs.len());
        for (idx, ((codec, supported), track)) in codecs.into_iter().zip(&a).enumerate() {
            assert_eq!(track.index, idx);
            assert_eq!(track.codec, codec.name());
            assert_eq!(track.supported, supported, "{codec:?}");
            assert_eq!(track.sample_rate, None);
            assert_eq!(track.channels, None);
        }
    }

    #[test]
    fn video_codec_support_follows_the_compiled_backends() {
        assert!(is_native_video_codec(CodecId::Mjpeg));
        assert!(is_native_video_codec(CodecId::RawVideo));
        assert_eq!(
            is_native_video_codec(CodecId::H264),
            cfg!(feature = "backend-openh264")
        );
        assert!(!is_native_video_codec(CodecId::Hevc));
        assert!(!is_native_video_codec(CodecId::Unknown));
    }

    #[cfg(not(feature = "audio-rodio"))]
    #[test]
    fn no_sink_opens_without_rodio() {
        assert!(AudioSink::try_open_sink(1.0).is_none());
    }

    /// Stereo ramp whose value identifies the sample frame it came from.
    fn stereo_ramp(frames: i16) -> Vec<i16> {
        (0..frames).flat_map(|f| [f, -f]).collect()
    }

    /// Trim a 10 ms (480 frames @ 48 kHz) stereo frame starting at 100 ms.
    fn trim_10ms_at_100ms(target: Duration) -> (bool, Vec<i16>) {
        let mut samples = stereo_ramp(480);
        let kept = trim_audio_samples_before_target(
            Duration::from_millis(100),
            48_000,
            2,
            target,
            &mut samples,
        );
        (kept, samples)
    }

    #[test]
    fn trim_keeps_frames_at_or_after_the_target() {
        for target_ms in [0, 50, 100] {
            let (kept, samples) = trim_10ms_at_100ms(Duration::from_millis(target_ms));
            assert!(kept);
            assert_eq!(samples, stereo_ramp(480), "target {target_ms} ms");
        }
    }

    #[test]
    fn trim_drops_frames_ending_at_or_before_the_target() {
        for target_ms in [110, 111, 5_000] {
            let (kept, samples) = trim_10ms_at_100ms(Duration::from_millis(target_ms));
            assert!(!kept, "target {target_ms} ms");
            assert!(samples.is_empty());
        }
    }

    #[test]
    fn trim_cuts_a_straddling_frame_on_a_channel_boundary() {
        // 2.5 ms into the frame = 120 sample frames.
        let (kept, samples) = trim_10ms_at_100ms(Duration::from_micros(102_500));
        assert!(kept);
        assert_eq!(samples.len(), (480 - 120) * 2);
        assert_eq!(&samples[..4], &[120, -120, 121, -121]);
        assert_eq!(&samples[samples.len() - 2..], &[479, -479]);
    }

    #[test]
    fn trim_rounds_truncated_timestamps_to_the_nearest_sample() {
        // 1024 samples @ 48 kHz = 21.333… ms, truncated to whole nanoseconds.
        let start = Duration::from_nanos(21_333_333);
        let target = Duration::from_millis(25); // 176.000016 samples later
        let mut samples: Vec<i16> = (0..1024).collect();
        assert!(trim_audio_samples_before_target(
            start,
            48_000,
            1,
            target,
            &mut samples
        ));
        assert_eq!(samples[0], 176);
        assert_eq!(samples.len(), 1024 - 176);
    }

    #[test]
    fn trim_drops_a_frame_with_less_than_half_a_sample_left() {
        let almost_end = Duration::from_nanos(110_000_000 - 1_000);
        let (kept, samples) = trim_10ms_at_100ms(almost_end);
        assert!(!kept);
        assert!(samples.is_empty());
    }

    #[test]
    fn trim_handles_empty_input_and_unknown_layouts() {
        let (zero, later) = (Duration::ZERO, Duration::from_secs(1));
        let mut empty = Vec::new();
        for target in [zero, later] {
            assert!(!trim_audio_samples_before_target(
                zero, 48_000, 2, target, &mut empty
            ));
        }
        for (rate, channels) in [(0, 2), (48_000, 0)] {
            let mut samples = vec![1, 2, 3, 4];
            assert!(trim_audio_samples_before_target(
                zero,
                rate,
                channels,
                later,
                &mut samples
            ));
            assert_eq!(samples, vec![1, 2, 3, 4]);
        }
        // An extreme start time must not overflow the frame end.
        let mut samples = vec![1, 2];
        assert!(!trim_audio_samples_before_target(
            Duration::MAX - Duration::from_nanos(1),
            48_000,
            2,
            Duration::MAX,
            &mut samples
        ));
    }

    #[test]
    fn audio_seek_trim_stays_armed_until_the_target_is_reached() {
        let target = Duration::from_millis(15);
        let mut drop_until = Some(target);
        let frame = |ms| Some(Duration::from_millis(ms));

        // Entirely before the target: dropped, still armed.
        let mut samples = vec![7i16; 10];
        apply_audio_seek_trim(&mut drop_until, frame(0), 1_000, 1, &mut samples);
        assert!(samples.is_empty());
        assert_eq!(drop_until, Some(target));

        // Straddling: trimmed, disarmed.
        let mut samples = vec![7i16; 10];
        apply_audio_seek_trim(&mut drop_until, frame(10), 1_000, 1, &mut samples);
        assert_eq!(samples.len(), 5);
        assert_eq!(drop_until, None);

        // Disarmed: even an early frame passes through.
        let mut samples = vec![7i16; 10];
        apply_audio_seek_trim(&mut drop_until, frame(0), 1_000, 1, &mut samples);
        assert_eq!(samples.len(), 10);

        // No timestamp: nothing to compare against, so trimming ends.
        let mut drop_until = Some(target);
        apply_audio_seek_trim(&mut drop_until, None, 1_000, 1, &mut samples);
        assert_eq!(samples.len(), 10);
        assert_eq!(drop_until, None);
    }

    #[test]
    fn timestamps_convert_through_the_stream_time_base() {
        use rsmpeg_util::Rational;

        assert_eq!(
            timestamp_to_duration(Some(24_000), Rational::new(1, 48_000)),
            Some(Duration::from_millis(500))
        );
        assert_eq!(
            timestamp_to_duration(Some(-5), Rational::new(1, 1_000)),
            Some(Duration::ZERO)
        );
        assert_eq!(timestamp_to_duration(None, Rational::new(1, 1_000)), None);
        assert_eq!(timestamp_to_duration(Some(1), Rational::new(1, 0)), None);
        assert_eq!(timestamp_to_duration(Some(1), Rational::new(0, 1)), None);
    }
}
