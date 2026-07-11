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
use crate::h264_bitstream::extract_avcc_streaming;

const LATE_DROP_SEC: f64 = 0.050;
const MAX_PACE_SLEEP: Duration = Duration::from_millis(12);
const MAX_AUDIO_QUEUE_SOURCES: usize = 48;

/// Spawn the playback worker thread.
pub fn spawn_worker(
    path: PathBuf,
    volume: f32,
    prefer_native: bool,
    cmd_rx: Receiver<PlayerCommand>,
    event_tx: SyncSender<PlayerEvent>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        if let Err(e) = run_worker(path, volume, prefer_native, cmd_rx, event_tx.clone()) {
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

/// Map a Symphonia track + optional avcC blob into rsmpeg H.264 codec parameters.
#[cfg(all(
    feature = "backend-symphonia",
    feature = "backend-openh264",
    feature = "audio-rodio"
))]
fn map_h264_params(
    _track: &symphonia::core::formats::Track,
    extra: Option<Vec<u8>>,
) -> rsmpeg_codec::CodecParameters {
    use rsmpeg_codec::{CodecId, CodecParameters, H264BitstreamFormat};
    use rsmpeg_util::PixelFormat;

    let mut params = CodecParameters::new(CodecId::H264);
    params.pixel_format = Some(PixelFormat::Yuv420P);
    // Symphonia CodecParameters has no width/height; OpenH264 fills them on first frame.
    if let Some(extra) = extra {
        // Prefer avcC when present so OpenH264Decoder can convert AVCC → Annex B.
        if extra.len() >= 7 && extra[0] == 1 {
            let nal_length_size = (extra[4] & 0x03) + 1;
            params.h264_bitstream_format = H264BitstreamFormat::Avcc { nal_length_size };
        }
        params.extradata = Some(extra);
    }
    params
}

/// Map a Symphonia audio track into rsmpeg [`CodecParameters`] when the codec is known.
///
/// Supported for decode backends: AAC / MP3 / PCM. FLAC is mapped for completeness
/// but [`SymphoniaAudioDecoder`] may reject it (caller falls back or mutes).
#[cfg(all(
    feature = "backend-symphonia",
    feature = "backend-openh264",
    feature = "audio-rodio"
))]
fn map_audio_params(
    track: &symphonia::core::formats::Track,
) -> Option<rsmpeg_codec::CodecParameters> {
    use rsmpeg_codec::{CodecId, CodecParameters};
    use rsmpeg_util::SampleFormat;
    use symphonia::core::codecs::{
        CODEC_TYPE_AAC, CODEC_TYPE_FLAC, CODEC_TYPE_MP3, CODEC_TYPE_PCM_F32LE,
        CODEC_TYPE_PCM_S16LE, CODEC_TYPE_PCM_S32LE, CODEC_TYPE_PCM_U8,
    };

    let cp = &track.codec_params;
    let codec_id = if cp.codec == CODEC_TYPE_AAC {
        CodecId::Aac
    } else if cp.codec == CODEC_TYPE_MP3 {
        CodecId::Mp3
    } else if cp.codec == CODEC_TYPE_FLAC {
        CodecId::Flac
    } else if cp.codec == CODEC_TYPE_PCM_S16LE
        || cp.codec == CODEC_TYPE_PCM_S32LE
        || cp.codec == CODEC_TYPE_PCM_F32LE
        || cp.codec == CODEC_TYPE_PCM_U8
    {
        CodecId::Pcm
    } else {
        // Heuristic: sample-rate + channels without a recognized type → try AAC
        // (common for MP4 when Symphonia reports a generic audio type).
        if cp.sample_rate.is_some() && cp.channels.is_some() && cp.extra_data.is_some() {
            CodecId::Aac
        } else if cp.sample_rate.is_some() && cp.channels.is_some() {
            // Unknown PCM-like elementary stream; leave unmapped so limited path can try.
            return None;
        } else {
            return None;
        }
    };

    let mut params = CodecParameters::new(codec_id);
    params.sample_rate = cp.sample_rate;
    params.channels = cp.channels.map(|c| c.count() as u16);
    if let Some(ref extra) = cp.extra_data {
        params.extradata = Some(extra.to_vec());
    }
    if codec_id == CodecId::Pcm {
        params.sample_format = Some(if cp.codec == CODEC_TYPE_PCM_U8 {
            SampleFormat::U8
        } else if cp.codec == CODEC_TYPE_PCM_S32LE {
            SampleFormat::S32
        } else if cp.codec == CODEC_TYPE_PCM_F32LE {
            SampleFormat::F32
        } else {
            SampleFormat::S16
        });
    } else {
        params.sample_format = Some(SampleFormat::S16);
    }
    Some(params)
}

/// Build an rsmpeg [`Packet`] from a Symphonia demux packet.
#[cfg(all(
    feature = "backend-symphonia",
    feature = "backend-openh264",
    feature = "audio-rodio"
))]
fn sym_packet_to_rsmpeg(
    packet: &symphonia::core::formats::Packet,
    stream_index: usize,
    time_base: rsmpeg_util::Rational,
) -> rsmpeg_codec::Packet {
    use rsmpeg_codec::Packet;

    // `Packet::new` takes `bytes::Bytes`; convert via `Vec` so we need no direct `bytes` dep.
    let mut pkt = Packet::new(packet.data.to_vec().into(), stream_index);
    let ts = packet.ts() as i64;
    pkt.pts = Some(ts);
    pkt.dts = Some(ts);
    pkt.duration = packet.dur() as i64;
    pkt.time_base = time_base;
    pkt
}

#[cfg(all(
    feature = "backend-symphonia",
    feature = "backend-openh264",
    feature = "audio-rodio"
))]
fn run_worker(
    path: PathBuf,
    mut volume: f32,
    prefer_native: bool,
    cmd_rx: Receiver<PlayerCommand>,
    event_tx: SyncSender<PlayerEvent>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use rodio::{OutputStream, Sink};
    use rsmpeg_codec::{DecodeStatus, Decoder};
    use rsmpeg_util::Rational;
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::errors::Error;
    use symphonia::core::formats::{FormatOptions, SeekMode, SeekTo, Track};
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;
    use symphonia::core::units::Time;

    use crate::backend::OpenH264Decoder;
    use crate::backend::SymphoniaAudioDecoder;
    use crate::video_convert::yuv420p_frame_to_rgba;

    if prefer_native {
        match crate::native_pipeline::try_run_native(&path, volume, &cmd_rx, &event_tx) {
            Ok(()) => return Ok(()),
            Err(reason) => {
                emit(
                    &event_tx,
                    PlayerEvent::Warning {
                        message: format!("native pipeline unavailable ({reason}); falling back"),
                        generation: 0,
                    },
                );
            }
        }
    }

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

    let video_time_base = video_track.as_ref().and_then(|t| {
        t.codec_params
            .time_base
            .map(|tb| Rational::new(tb.numer as i32, tb.denom.max(1) as i32))
    });
    let sec_per_tick = video_time_base
        .map(|tb| tb.num as f64 / tb.den.max(1) as f64)
        .filter(|s| s.is_finite() && *s > 0.0);
    let assumed_frame_dur = 1.0 / 30.0;
    let mut playback_start: Option<Instant> = None;
    let mut base_video_pts: Option<i64> = None;
    let mut video_frame_index = 0u64;

    // ── Video decoder (OpenH264 backend) ──
    let avcc_blob = video_track
        .as_ref()
        .and_then(|t| t.codec_params.extra_data.as_ref().map(|e| e.to_vec()))
        .or(stream_avcc);
    let mut video_dec: Option<OpenH264Decoder> = if let Some(ref vt) = video_track {
        let params = map_h264_params(vt, avcc_blob);
        match OpenH264Decoder::from_params(&params) {
            Ok(d) => Some(d),
            Err(e) => {
                emit(
                    &event_tx,
                    PlayerEvent::Warning {
                        message: format!("fallback: failed to open OpenH264 decoder: {e}"),
                        generation,
                    },
                );
                None
            }
        }
    } else {
        None
    };

    // ── Audio decoder (Symphonia packet-in backend, limited raw fallback) ──
    let audio_track_id = audio_track.as_ref().map(|t| t.id);
    let audio_time_base = audio_track.as_ref().and_then(|t| {
        t.codec_params
            .time_base
            .map(|tb| Rational::new(tb.numer as i32, tb.denom.max(1) as i32))
    });
    let mut audio_channels = audio_track
        .as_ref()
        .and_then(|t| t.codec_params.channels)
        .map(|c| c.count() as u16)
        .unwrap_or(2);
    let mut audio_rate = audio_track
        .as_ref()
        .and_then(|t| t.codec_params.sample_rate)
        .unwrap_or(44_100);

    let mut audio_dec: Option<SymphoniaAudioDecoder> = None;
    // Limited path: raw Symphonia codec decoder for codecs the backend does not wire.
    let mut audio_decoder_raw: Option<Box<dyn symphonia::core::codecs::Decoder>> = None;

    if let Some(ref at) = audio_track {
        if let Some(params) = map_audio_params(at) {
            if SymphoniaAudioDecoder::supported(params.codec_id) {
                match SymphoniaAudioDecoder::try_new(&params) {
                    Ok(d) => audio_dec = Some(d),
                    Err(e) => {
                        emit(
                            &event_tx,
                            PlayerEvent::Warning {
                                message: format!(
                                    "fallback: SymphoniaAudioDecoder open failed ({e}); trying limited path"
                                ),
                                generation,
                            },
                        );
                        audio_decoder_raw = symphonia::default::get_codecs()
                            .make(&at.codec_params, &DecoderOptions::default())
                            .ok();
                    }
                }
            } else {
                // e.g. FLAC mapped but not supported by packet-in backend.
                emit(
                    &event_tx,
                    PlayerEvent::Warning {
                        message: format!(
                            "fallback audio codec '{}' not wired in backend — limited path",
                            params.codec_id.name()
                        ),
                        generation,
                    },
                );
                audio_decoder_raw = symphonia::default::get_codecs()
                    .make(&at.codec_params, &DecoderOptions::default())
                    .ok();
            }
        } else {
            // Unmapped codec: keep previous limited Symphonia decoder path.
            audio_decoder_raw = symphonia::default::get_codecs()
                .make(&at.codec_params, &DecoderOptions::default())
                .ok();
            if audio_decoder_raw.is_none() {
                emit(
                    &event_tx,
                    PlayerEvent::Warning {
                        message: "fallback: no audio decoder available — audio muted".into(),
                        generation,
                    },
                );
            }
        }
    }

    let _rodio = OutputStream::try_default();
    let sink = _rodio
        .as_ref()
        .ok()
        .and_then(|(_, h)| Sink::try_new(h).ok());
    if let Some(ref s) = sink {
        s.set_volume(volume);
    }

    let seek_track_id = video_track.as_ref().or(audio_track.as_ref()).map(|t| t.id);
    let video_stream_index = video_track.as_ref().map(|t| t.id as usize).unwrap_or(0);
    let audio_stream_index = audio_track.as_ref().map(|t| t.id as usize).unwrap_or(0);
    let default_video_tb = video_time_base.unwrap_or_else(|| Rational::new(1, 30_000));
    let default_audio_tb =
        audio_time_base.unwrap_or_else(|| Rational::new(1, audio_rate.max(1) as i32));

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
                            // Reset backends in place (do not recreate raw openh264).
                            if let Some(ref mut d) = video_dec {
                                let _ = d.reset();
                            }
                            if let Some(ref mut d) = audio_dec {
                                let _ = d.reset();
                            }
                            if let Some(ref mut d) = audio_decoder_raw {
                                d.reset();
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

        // ── Video via OpenH264Decoder ──
        if video_dec.is_some() && video_track.as_ref().is_some_and(|vt| vt.id == tid) {
            let rsmpeg_pkt = sym_packet_to_rsmpeg(&packet, video_stream_index, default_video_tb);
            if let Some(ref mut dec) = video_dec {
                if dec.send_packet(Some(&rsmpeg_pkt)).is_ok() {
                    loop {
                        match dec.receive_frame() {
                            Ok(DecodeStatus::Frame(f)) => {
                                let w = f.width;
                                let h = f.height;
                                if w > 0 && h > 0 {
                                    let pts = f.pts.unwrap_or(rsmpeg_pkt.pts.unwrap_or(0));
                                    let abs_pos = match sec_per_tick {
                                        Some(spt) => {
                                            if base_video_pts.is_none() {
                                                base_video_pts = Some(pts);
                                            }
                                            let base = base_video_pts.unwrap_or(pts);
                                            (pts - base) as f64 * spt
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

        // ── Audio via SymphoniaAudioDecoder (or limited raw path) ──
        if has_audio && audio_track_id == Some(tid) {
            if force_one_frame && !playing {
                continue;
            }
            if let Some(ref mut dec) = audio_dec {
                let rsmpeg_pkt =
                    sym_packet_to_rsmpeg(&packet, audio_stream_index, default_audio_tb);
                if dec.send_packet(Some(&rsmpeg_pkt)).is_ok() {
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
                                // Drive position from audio when no video.
                                if video_track.is_none() {
                                    if let Some(ts) = f.pts.or(rsmpeg_pkt.pts).or(rsmpeg_pkt.dts) {
                                        let sec = ts as f64 * default_audio_tb.num as f64
                                            / default_audio_tb.den.max(1) as f64;
                                        position = Duration::from_secs_f64(sec.max(0.0));
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
                            Ok(DecodeStatus::NeedMoreInput) | Ok(DecodeStatus::EndOfStream) => {
                                break;
                            }
                            Err(_) => break,
                        }
                    }
                }
            } else if let Some(ref mut raw) = audio_decoder_raw {
                // Limited path: direct Symphonia codec decoder (e.g. FLAC).
                match raw.decode(&packet) {
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
        }
        if stop {
            break;
        }
    }

    if !stop {
        // EOS: send_packet(None) + drain remaining frames (same as native_pipeline).
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
                        Ok(DecodeStatus::NeedMoreInput) | Ok(DecodeStatus::EndOfStream) => break,
                        Err(_) => break,
                    }
                }
            }
        }
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
    _prefer_native: bool,
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
