//! Native demux path: `rsmpeg-format` packets → OpenH264 / Symphonia decode.
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
use crate::h264_bitstream::{
    avcc_extradata_to_annex_b, avcc_nal_length_size, packet_for_decoder, H264BitstreamFormat,
};

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
    use openh264::formats::YUVSource;
    use rodio::{OutputStream, Sink};
    use symphonia::core::audio::{Channels, SampleBuffer};
    use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_AAC, CODEC_TYPE_PCM_S16LE};
    use symphonia::core::formats::Packet as SymPacket;
    use symphonia::core::units::TimeBase;

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

    // ── Video decoder ──
    let mut h264 = if video_si.is_some() {
        openh264::decoder::Decoder::with_api_config(
            openh264::OpenH264API::from_source(),
            openh264::decoder::DecoderConfig::new()
                .flush_after_decode(openh264::decoder::Flush::NoFlush),
        )
        .ok()
    } else {
        None
    };

    let (sps_pps_prefix, bitstream_format) = if let Some(si) = video_si {
        let stream = &ctx.streams[si];
        if let Some(ref avcc) = stream.codec_params.extradata {
            match (avcc_nal_length_size(avcc), avcc_extradata_to_annex_b(avcc)) {
                (Ok(n), Ok(a)) => (Some(a), H264BitstreamFormat::Avcc { nal_length_size: n }),
                _ => (None, H264BitstreamFormat::Avcc { nal_length_size: 4 }),
            }
        } else {
            match stream.codec_params.h264_bitstream_format {
                rsmpeg_codec::H264BitstreamFormat::Avcc { nal_length_size } => (
                    None,
                    H264BitstreamFormat::Avcc {
                        nal_length_size: nal_length_size as usize,
                    },
                ),
                rsmpeg_codec::H264BitstreamFormat::AnnexB => (None, H264BitstreamFormat::AnnexB),
                rsmpeg_codec::H264BitstreamFormat::Unknown => {
                    (None, H264BitstreamFormat::Avcc { nal_length_size: 4 })
                }
            }
        }
    } else {
        (None, H264BitstreamFormat::AnnexB)
    };
    let mut sps_pps_sent = false;

    // ── Audio decoder (Symphonia decode-only, no demux) ──
    let (mut audio_decoder, audio_channels, audio_rate) = if let Some(si) = audio_si {
        let stream = &ctx.streams[si];
        let rate = stream.codec_params.sample_rate.unwrap_or(48_000);
        let ch = stream.codec_params.channels.unwrap_or(2);
        let codec_ok = matches!(stream.codec_id, CodecId::Aac | CodecId::Pcm);
        if !codec_ok {
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
            let mut params = symphonia::core::codecs::CodecParameters::new();
            match stream.codec_id {
                CodecId::Aac => {
                    params.for_codec(CODEC_TYPE_AAC);
                    if let Some(ref extra) = stream.codec_params.extradata {
                        let asc = extract_aac_asc(extra).unwrap_or_else(|| extra.clone());
                        params.with_extra_data(asc.into_boxed_slice());
                    }
                }
                CodecId::Pcm => {
                    params.for_codec(CODEC_TYPE_PCM_S16LE);
                }
                _ => {}
            }
            params.with_sample_rate(rate);
            let channels = match ch {
                1 => Channels::FRONT_LEFT,
                _ => Channels::FRONT_LEFT | Channels::FRONT_RIGHT,
            };
            params.with_channels(channels);
            let den = stream.time_base.den.max(1) as u32;
            let num = stream.time_base.num.max(1) as u32;
            params.with_time_base(TimeBase::new(num, den));
            let dec = symphonia::default::get_codecs()
                .make(&params, &DecoderOptions::default())
                .ok();
            if dec.is_none() {
                emit(
                    event_tx,
                    PlayerEvent::Warning {
                        message: "native path: failed to open Symphonia audio decoder".into(),
                        generation: 1,
                    },
                );
            }
            (dec, ch, rate)
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
                            if let Some(ref mut d) = audio_decoder {
                                d.reset();
                            }
                            if video_si.is_some() {
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

        let packet = match pending.take() {
            Some(p) => p,
            None => match ctx.read_frame() {
                Ok(Some(p)) => p,
                Ok(None) => break,
                Err(_) => break,
            },
        };
        let si = packet.stream_index;

        // ── Video ──
        if Some(si) == video_si && h264.is_some() {
            let pts = packet.pts.or(packet.dts).unwrap_or(0);
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
                                    let mut rem = Duration::from_secs_f64(delta.min(0.5));
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
                            let mut rgba = vec![0u8; w * h * 4];
                            yuv.write_rgba8(&mut rgba);
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
            }
        }

        // ── Audio ──
        if Some(si) == audio_si && audio_decoder.is_some() {
            if force_one_frame && !playing {
                continue;
            }
            let ts = packet.pts.or(packet.dts).unwrap_or(0).max(0) as u64;
            let dur = packet.duration.max(0) as u64;
            let sym = SymPacket::new_from_slice(si as u32, ts, dur, &packet.data);
            match audio_decoder.as_mut().unwrap().decode(&sym) {
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
                    // Drive position from audio when no video
                    if video_si.is_none() {
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
                Err(symphonia::core::errors::Error::DecodeError(_))
                | Err(symphonia::core::errors::Error::IoError(_)) => {}
                Err(_) => {}
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

/// Pull AudioSpecificConfig from an MPEG-4 `esds` box payload (or return raw).
fn extract_aac_asc(esds: &[u8]) -> Option<Vec<u8>> {
    let mut i = 0usize;
    while i < esds.len() {
        let tag = esds[i];
        i += 1;
        if i >= esds.len() {
            return None;
        }
        // Expandable MPEG-4 length
        let mut len = 0usize;
        for _ in 0..4 {
            if i >= esds.len() {
                return None;
            }
            let b = esds[i];
            i += 1;
            len = (len << 7) | (b & 0x7f) as usize;
            if b & 0x80 == 0 {
                break;
            }
        }
        if tag == 0x05 {
            if i + len <= esds.len() && len > 0 {
                return Some(esds[i..i + len].to_vec());
            }
            return None;
        }
        // Skip this descriptor body (may contain nested descriptors — linear scan still works)
        if tag == 0x03 || tag == 0x04 {
            // Do not skip fully; nested tags follow inside — just continue from i
            continue;
        }
        i = i.saturating_add(len);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_asc_from_minimal_esds_like() {
        // Fake: tag 0x05, len 2, ASC bytes
        let data = [0x05u8, 0x02, 0x11, 0x90];
        let asc = extract_aac_asc(&data).expect("asc");
        assert_eq!(asc, vec![0x11, 0x90]);
    }
}
