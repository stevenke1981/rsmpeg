//! End-to-end playback worker regression tests (native & Symphonia fallback).

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use rsmpeg_player::{Player, PlayerError, PlayerEvent, PlayerState};

/// Single valid 48 kHz mono AAC-LC raw frame (non-silent tone, 1024 samples / ~21.33 ms).
const AAC_MONO_48K_FRAME: &[u8] = &[
    0x01, 0x36, 0x8F, 0xDE, 0xCA, 0x21, 0x96, 0x58, 0xA3, 0x14, 0x7E, 0xED, 0x38, 0xFE, 0xFA, 0xEB,
    0xA8, 0xD0, 0xD2, 0x9D, 0x63, 0xAC, 0xB6, 0x59, 0xEE, 0x14, 0x5E, 0xFF, 0x65, 0x30, 0xD7, 0x54,
    0xD9, 0x1F, 0x8C, 0xE8, 0x92, 0xCF, 0xE5, 0xB2, 0xEF, 0xE5, 0xB0, 0x61, 0x44, 0x18, 0x51, 0x1D,
    0x0B, 0xEC, 0x8B, 0x16, 0xA9, 0xFF, 0x63, 0xAA, 0x77, 0x34, 0xCE, 0x1E, 0xD6, 0xFC, 0x25, 0x85,
    0xE0, 0xE0, 0xD2, 0xDD, 0x09, 0x06, 0x96, 0xE8, 0x48, 0x34, 0xB9, 0x04, 0xE0, 0x06, 0x10, 0x11,
    0x70,
];

/// AAC-LC 48 kHz mono AudioSpecificConfig (`objectType=2`, `freqIdx=3`, `channels=1`).
const AAC_MONO_48K_ASC: &[u8] = &[0x11, 0x88];

/// Minimal valid H.264 Baseline 16×16 `avcC` box payload (SPS + PPS).
const H264_16X16_AVCC: &[u8] = &[
    0x01, 0x42, 0xC0, 0x0A, 0xFF, 0xE1, 0x00, 0x16, 0x67, 0x42, 0xC0, 0x0A, 0xD9, 0x1E, 0xC0, 0x44,
    0x00, 0x00, 0x03, 0x00, 0x04, 0x00, 0x00, 0x03, 0x00, 0x08, 0x3C, 0x48, 0x99, 0x20, 0x01, 0x00,
    0x05, 0x68, 0xCB, 0x83, 0xCB, 0x20,
];

/// Matching 4-byte AVCC length-prefixed 16×16 IDR slice NAL unit.
const H264_16X16_IDR_SAMPLE: &[u8] = &[
    0x00, 0x00, 0x00, 0x14, 0x65, 0x88, 0x84, 0x05, 0x7C, 0x46, 0x28, 0x00, 0x0E, 0xA0, 0xC7, 0x00,
    0x01, 0xB0, 0x68, 0xE0, 0x00, 0x23, 0x43, 0x80,
];

static NEXT_TMP_ID: AtomicU64 = AtomicU64::new(1);

fn unique_temp_path(stem: &str, ext: &str) -> PathBuf {
    let id = NEXT_TMP_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "rsmpeg_player_{stem}_{}_{id}.{ext}",
        std::process::id()
    ))
}

fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

fn box_bytes(tag: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + payload.len());
    out.extend_from_slice(&be32((8 + payload.len()) as u32));
    out.extend_from_slice(tag);
    out.extend_from_slice(payload);
    out
}

fn fullbox(version: u8, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + payload.len());
    out.push(version);
    out.push(((flags >> 16) & 0xFF) as u8);
    out.push(((flags >> 8) & 0xFF) as u8);
    out.push((flags & 0xFF) as u8);
    out.extend_from_slice(payload);
    out
}

fn encode_mp4_esds(asc: &[u8]) -> Vec<u8> {
    // DecoderSpecificInfo (tag 0x05)
    let mut dsi = vec![0x05, asc.len() as u8];
    dsi.extend_from_slice(asc);

    // DecoderConfigDescriptor (tag 0x04)
    let mut dcd_body = vec![
        0x40, // objectTypeIndication: MPEG-4 Audio (AAC)
        0x15, // streamType=Audio (5<<2 | 1)
        0x00, 0x00, 0x00, // bufferSizeDB
    ];
    dcd_body.extend_from_slice(&be32(64_000)); // maxBitrate
    dcd_body.extend_from_slice(&be32(64_000)); // avgBitrate
    dcd_body.extend_from_slice(&dsi);

    let mut dcd = vec![0x04, dcd_body.len() as u8];
    dcd.extend_from_slice(&dcd_body);

    // SLConfigDescriptor (tag 0x06)
    let sl = [0x06u8, 0x01, 0x02];

    // ES_Descriptor (tag 0x03)
    let mut es_body = vec![0x00, 0x01, 0x00]; // ES_ID=1, flags=0
    es_body.extend_from_slice(&dcd);
    es_body.extend_from_slice(&sl);

    let mut es = vec![0x03, es_body.len() as u8];
    es.extend_from_slice(&es_body);

    box_bytes(b"esds", &fullbox(0, 0, &es))
}

fn encode_iso639_lang(lang: &[u8; 3]) -> u16 {
    let a = u16::from(lang[0].saturating_sub(0x60)) & 0x1F;
    let b = u16::from(lang[1].saturating_sub(0x60)) & 0x1F;
    let c = u16::from(lang[2].saturating_sub(0x60)) & 0x1F;
    (a << 10) | (b << 5) | c
}

fn build_aac_trak(
    track_id: u32,
    sample_count: u32,
    chunk_offset: u32,
    frame_size: u32,
    lang: &[u8; 3],
) -> Vec<u8> {
    let duration_samples = sample_count * 1024;
    let duration_ms = (u64::from(duration_samples) * 1000 / 48_000) as u32;

    // tkhd (v0, flags=3 enabled+in_movie)
    let mut tkhd_body = vec![0u8; 80];
    tkhd_body[8..12].copy_from_slice(&be32(track_id));
    tkhd_body[16..20].copy_from_slice(&be32(duration_ms));
    tkhd_body[32..34].copy_from_slice(&0x0100u16.to_be_bytes()); // volume 1.0
    tkhd_body[36..40].copy_from_slice(&be32(0x0001_0000)); // unity matrix
    tkhd_body[52..56].copy_from_slice(&be32(0x0001_0000));
    tkhd_body[68..72].copy_from_slice(&be32(0x4000_0000));
    let tkhd = box_bytes(b"tkhd", &fullbox(0, 3, &tkhd_body));

    // mdhd (v0)
    let mut mdhd_body = vec![0u8; 20];
    mdhd_body[8..12].copy_from_slice(&be32(48_000));
    mdhd_body[12..16].copy_from_slice(&be32(duration_samples));
    mdhd_body[16..18].copy_from_slice(&encode_iso639_lang(lang).to_be_bytes());
    let mdhd = box_bytes(b"mdhd", &fullbox(0, 0, &mdhd_body));

    // hdlr
    let mut hdlr_body = vec![0u8; 21];
    hdlr_body[4..8].copy_from_slice(b"soun");
    let hdlr = box_bytes(b"hdlr", &fullbox(0, 0, &hdlr_body));

    // smhd + dinf(dref(url))
    let smhd = box_bytes(b"smhd", &fullbox(0, 0, &[0u8; 4]));
    let url_box = box_bytes(b"url ", &fullbox(0, 1, &[]));
    let mut dref_body = Vec::new();
    dref_body.extend_from_slice(&be32(1));
    dref_body.extend_from_slice(&url_box);
    let dinf = box_bytes(b"dinf", &box_bytes(b"dref", &fullbox(0, 0, &dref_body)));

    // stsd -> mp4a -> esds
    let mut mp4a_body = vec![0u8; 28];
    mp4a_body[6..8].copy_from_slice(&1u16.to_be_bytes()); // data_reference_index=1
    mp4a_body[16..18].copy_from_slice(&1u16.to_be_bytes()); // mono
    mp4a_body[18..20].copy_from_slice(&16u16.to_be_bytes()); // 16-bit
    mp4a_body[24..26].copy_from_slice(&48_000u16.to_be_bytes()); // 48 kHz
    mp4a_body.extend_from_slice(&encode_mp4_esds(AAC_MONO_48K_ASC));
    let mp4a = box_bytes(b"mp4a", &mp4a_body);

    let mut stsd_body = Vec::new();
    stsd_body.extend_from_slice(&be32(1));
    stsd_body.extend_from_slice(&mp4a);
    let stsd = box_bytes(b"stsd", &fullbox(0, 0, &stsd_body));

    let mut stts_body = Vec::new();
    stts_body.extend_from_slice(&be32(1));
    stts_body.extend_from_slice(&be32(sample_count));
    stts_body.extend_from_slice(&be32(1024));
    let stts = box_bytes(b"stts", &fullbox(0, 0, &stts_body));

    let mut stsc_body = Vec::new();
    stsc_body.extend_from_slice(&be32(1));
    stsc_body.extend_from_slice(&be32(1));
    stsc_body.extend_from_slice(&be32(sample_count));
    stsc_body.extend_from_slice(&be32(1));
    let stsc = box_bytes(b"stsc", &fullbox(0, 0, &stsc_body));

    let mut stsz_body = Vec::new();
    stsz_body.extend_from_slice(&be32(frame_size)); // uniform sample_size
    stsz_body.extend_from_slice(&be32(sample_count));
    let stsz = box_bytes(b"stsz", &fullbox(0, 0, &stsz_body));

    let mut stco_body = Vec::new();
    stco_body.extend_from_slice(&be32(1));
    stco_body.extend_from_slice(&be32(chunk_offset));
    let stco = box_bytes(b"stco", &fullbox(0, 0, &stco_body));

    let mut stbl_body = Vec::new();
    for b in [&stsd, &stts, &stsc, &stsz, &stco] {
        stbl_body.extend_from_slice(b);
    }
    let stbl = box_bytes(b"stbl", &stbl_body);

    let mut minf_body = Vec::new();
    minf_body.extend_from_slice(&smhd);
    minf_body.extend_from_slice(&dinf);
    minf_body.extend_from_slice(&stbl);
    let minf = box_bytes(b"minf", &minf_body);

    let mut mdia_body = Vec::new();
    mdia_body.extend_from_slice(&mdhd);
    mdia_body.extend_from_slice(&hdlr);
    mdia_body.extend_from_slice(&minf);
    let mdia = box_bytes(b"mdia", &mdia_body);

    let mut trak_body = Vec::new();
    trak_body.extend_from_slice(&tkhd);
    trak_body.extend_from_slice(&mdia);
    box_bytes(b"trak", &trak_body)
}

/// Build a valid self-contained dual-track AAC MP4 (`frames_per_track` × 21.33 ms per track).
fn build_dual_aac_mp4(frames_per_track: u32) -> Vec<u8> {
    let ftyp = box_bytes(b"ftyp", b"isom\0\0\x02\0isomiso2mp41");

    let frame_size = AAC_MONO_48K_FRAME.len() as u32;
    let track_bytes = (frame_size * frames_per_track) as usize;
    let mut mdat_payload = Vec::with_capacity(track_bytes * 2);
    for _ in 0..frames_per_track {
        mdat_payload.extend_from_slice(AAC_MONO_48K_FRAME);
    }
    for _ in 0..frames_per_track {
        mdat_payload.extend_from_slice(AAC_MONO_48K_FRAME);
    }
    let mdat = box_bytes(b"mdat", &mdat_payload);

    let t1_offset = (ftyp.len() + 8) as u32;
    let t2_offset = t1_offset + track_bytes as u32;

    let duration_ms = (u64::from(frames_per_track) * 1024 * 1000 / 48_000) as u32;
    let mut mvhd_body = vec![0u8; 96];
    mvhd_body[8..12].copy_from_slice(&be32(1000)); // timescale=1000
    mvhd_body[12..16].copy_from_slice(&be32(duration_ms));
    mvhd_body[16..20].copy_from_slice(&be32(0x0001_0000)); // rate 1.0
    mvhd_body[20..22].copy_from_slice(&0x0100u16.to_be_bytes()); // volume 1.0
    mvhd_body[32..36].copy_from_slice(&be32(0x0001_0000));
    mvhd_body[48..52].copy_from_slice(&be32(0x0001_0000));
    mvhd_body[64..68].copy_from_slice(&be32(0x4000_0000));
    mvhd_body[92..96].copy_from_slice(&be32(3)); // next_track_ID=3
    let mvhd = box_bytes(b"mvhd", &fullbox(0, 0, &mvhd_body));

    let trak1 = build_aac_trak(1, frames_per_track, t1_offset, frame_size, b"eng");
    let trak2 = build_aac_trak(2, frames_per_track, t2_offset, frame_size, b"zho");

    let mut moov_body = Vec::new();
    moov_body.extend_from_slice(&mvhd);
    moov_body.extend_from_slice(&trak1);
    moov_body.extend_from_slice(&trak2);
    let moov = box_bytes(b"moov", &moov_body);

    let mut file = Vec::with_capacity(ftyp.len() + mdat.len() + moov.len());
    file.extend_from_slice(&ftyp);
    file.extend_from_slice(&mdat);
    file.extend_from_slice(&moov);
    file
}

fn watch(
    player: &mut Player,
    timeout: Duration,
    mut accept: impl FnMut(PlayerEvent) -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        while let Some(ev) = player.poll_event() {
            if accept(ev) {
                return true;
            }
        }
        thread::sleep(Duration::from_millis(4));
    }
    false
}

/// P1-02 regression: seeking and switching audio tracks while the demuxer has already reached
/// EOF and `rodio::Sink` is still draining must be processed by the common command loop rather
/// than silently discarded.
#[test]
fn eof_audio_drain_handles_seek_and_track_switch_in_native_and_fallback() {
    // 14 frames ≈ 300 ms at 1.0x, ≈ 1.2 s at 0.25x so demux hits EOF well before sink drains.
    let path = unique_temp_path("p1_02_dual_aac", "mp4");
    std::fs::write(&path, build_dual_aac_mp4(14)).expect("write dual-aac fixture");

    for prefer_native in [true, false] {
        let mut player = Player::builder()
            .input(&path)
            .prefer_native_pipeline(prefer_native)
            .playback_rate(0.25)
            .volume(0.0)
            .build()
            .expect("build player");

        let mut tracks = None;
        let mut saw_audio_level = false;
        let ready = watch(&mut player, Duration::from_secs(3), |ev| {
            match ev {
                PlayerEvent::Tracks {
                    audio_tracks,
                    active_audio,
                    ..
                } => {
                    tracks = Some((audio_tracks, active_audio));
                }
                PlayerEvent::AudioLevel { peak, .. } if peak > 0.0 => {
                    saw_audio_level = true;
                }
                _ => {}
            }
            tracks.is_some() && saw_audio_level
        });
        assert!(
            ready,
            "native={prefer_native}: expected Tracks and non-zero AudioLevel"
        );

        let (audio_tracks, active_audio) = tracks.expect("tracks");
        assert_eq!(
            audio_tracks.len(),
            2,
            "native={prefer_native}: 2 AAC tracks"
        );
        let other_idx = audio_tracks
            .iter()
            .find(|t| Some(t.index) != active_audio)
            .expect("second audio track")
            .index;

        // Allow the short file's demuxer to finish reading and enter the sink drain phase.
        thread::sleep(Duration::from_millis(160));
        let mut ended_before = false;
        while let Some(ev) = player.poll_event() {
            if matches!(ev, PlayerEvent::Ended { .. }) {
                ended_before = true;
            }
        }
        if ended_before {
            // Environments without an active audio output sink drain immediately; skip tail-only assert.
            continue;
        }

        player
            .seek(Duration::from_millis(20))
            .expect("seek during drain");
        let seek_gen = player.generation();
        player
            .select_audio_track(other_idx)
            .expect("switch audio track during drain");
        let track_gen = player.generation();

        let mut seek_ack = false;
        let mut track_ack = false;
        let mut end_gen = None;
        let _ = watch(&mut player, Duration::from_secs(2), |ev| {
            match ev {
                PlayerEvent::SeekCompleted { generation, .. } if generation == seek_gen => {
                    seek_ack = true;
                }
                PlayerEvent::Tracks {
                    active_audio,
                    generation,
                    ..
                } if generation == track_gen => {
                    track_ack = active_audio == Some(other_idx);
                }
                PlayerEvent::Ended { generation } => {
                    end_gen = Some(generation);
                }
                _ => {}
            }
            seek_ack && track_ack
        });

        assert!(
            seek_ack,
            "native={prefer_native}: expected SeekCompleted(gen={seek_gen}), end_gen={end_gen:?}"
        );
        assert!(
            track_ack,
            "native={prefer_native}: expected Tracks(active={other_idx}, gen={track_gen}), end_gen={end_gen:?}"
        );

        let _ = player.shutdown();
    }

    let _ = std::fs::remove_file(&path);
}

/// Build a valid 48 kHz 16-bit stereo PCM WAV of `duration_secs` seconds.
fn build_pcm_wav(duration_secs: u32) -> Vec<u8> {
    let sample_rate: u32 = 48_000;
    let channels: u16 = 2;
    let bits_per_sample: u16 = 16;
    let block_align = channels * (bits_per_sample / 8);
    let byte_rate = sample_rate * u32::from(block_align);
    let total_frames = sample_rate * duration_secs;
    let data_bytes = total_frames * u32::from(block_align);
    let riff_size = 36 + data_bytes;

    let mut out = Vec::with_capacity((44 + data_bytes) as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_size.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits_per_sample.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_bytes.to_le_bytes());

    for i in 0..total_frames {
        let s: i16 = if (i / 55) % 2 == 0 { 4000 } else { -4000 };
        out.extend_from_slice(&s.to_le_bytes());
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// Build a valid self-contained H.264 baseline 16×16 MP4 with `frame_count` IDR keyframes
/// spaced at `frame_duration_ms` intervals (timescale = 1000).
fn build_h264_mp4(frame_count: u32, frame_duration_ms: u32) -> Vec<u8> {
    build_h264_movie(1, frame_count, frame_duration_ms, false)
}

/// Build a self-contained MP4 with `video_tracks` identical H.264 16×16 tracks (track ids
/// `1..=video_tracks`, each with `frame_count` IDR samples spaced `frame_duration_ms` apart).
/// When `with_aac` is set, a trailing 48 kHz mono AAC track (track id `video_tracks + 1`)
/// covers the same duration.
fn build_h264_movie(
    video_tracks: u32,
    frame_count: u32,
    frame_duration_ms: u32,
    with_aac: bool,
) -> Vec<u8> {
    let ftyp = box_bytes(b"ftyp", b"isom\0\0\x02\0isomiso2avc1mp41");

    let sample_size = H264_16X16_IDR_SAMPLE.len() as u32;
    let track_bytes = sample_size * frame_count;
    let duration_ms = frame_count * frame_duration_ms;
    let audio_frames = if with_aac {
        (u64::from(duration_ms) * 48_000).div_ceil(1024 * 1000) as u32
    } else {
        0
    };

    let mut mdat_payload = Vec::with_capacity((track_bytes * video_tracks) as usize);
    for _ in 0..video_tracks {
        for _ in 0..frame_count {
            mdat_payload.extend_from_slice(H264_16X16_IDR_SAMPLE);
        }
    }
    for _ in 0..audio_frames {
        mdat_payload.extend_from_slice(AAC_MONO_48K_FRAME);
    }
    let mdat = box_bytes(b"mdat", &mdat_payload);

    let first_offset = (ftyp.len() + 8) as u32;
    let track_count = video_tracks + u32::from(with_aac);

    // mvhd
    let mut mvhd_body = vec![0u8; 96];
    mvhd_body[8..12].copy_from_slice(&be32(1000));
    mvhd_body[12..16].copy_from_slice(&be32(duration_ms));
    mvhd_body[16..20].copy_from_slice(&be32(0x0001_0000));
    mvhd_body[20..22].copy_from_slice(&0x0100u16.to_be_bytes());
    mvhd_body[32..36].copy_from_slice(&be32(0x0001_0000));
    mvhd_body[48..52].copy_from_slice(&be32(0x0001_0000));
    mvhd_body[64..68].copy_from_slice(&be32(0x4000_0000));
    mvhd_body[92..96].copy_from_slice(&be32(track_count + 1));
    let mvhd = box_bytes(b"mvhd", &fullbox(0, 0, &mvhd_body));

    let mut moov_body = Vec::new();
    moov_body.extend_from_slice(&mvhd);
    for t in 0..video_tracks {
        let offset = first_offset + t * track_bytes;
        moov_body.extend_from_slice(&build_h264_trak(
            t + 1,
            frame_count,
            frame_duration_ms,
            offset,
        ));
    }
    if with_aac {
        let audio_offset = first_offset + video_tracks * track_bytes;
        moov_body.extend_from_slice(&build_aac_trak(
            video_tracks + 1,
            audio_frames,
            audio_offset,
            AAC_MONO_48K_FRAME.len() as u32,
            b"eng",
        ));
    }
    let moov = box_bytes(b"moov", &moov_body);

    let mut file = Vec::with_capacity(ftyp.len() + mdat.len() + moov.len());
    file.extend_from_slice(&ftyp);
    file.extend_from_slice(&mdat);
    file.extend_from_slice(&moov);
    file
}

/// One H.264 `trak` with track id `track_id` whose samples live at `data_offset`.
fn build_h264_trak(
    track_id: u32,
    frame_count: u32,
    frame_duration_ms: u32,
    data_offset: u32,
) -> Vec<u8> {
    let sample_size = H264_16X16_IDR_SAMPLE.len() as u32;
    let duration_ms = frame_count * frame_duration_ms;

    // tkhd
    let mut tkhd_body = vec![0u8; 80];
    tkhd_body[8..12].copy_from_slice(&be32(track_id));
    tkhd_body[16..20].copy_from_slice(&be32(duration_ms));
    tkhd_body[36..40].copy_from_slice(&be32(0x0001_0000));
    tkhd_body[52..56].copy_from_slice(&be32(0x0001_0000));
    tkhd_body[68..72].copy_from_slice(&be32(0x4000_0000));
    tkhd_body[72..76].copy_from_slice(&be32(16 << 16)); // width 16.0
    tkhd_body[76..80].copy_from_slice(&be32(16 << 16)); // height 16.0
    let tkhd = box_bytes(b"tkhd", &fullbox(0, 3, &tkhd_body));

    // mdhd (timescale = 1000)
    let mut mdhd_body = vec![0u8; 20];
    mdhd_body[8..12].copy_from_slice(&be32(1000));
    mdhd_body[12..16].copy_from_slice(&be32(duration_ms));
    let mdhd = box_bytes(b"mdhd", &fullbox(0, 0, &mdhd_body));

    // hdlr
    let mut hdlr_body = vec![0u8; 21];
    hdlr_body[4..8].copy_from_slice(b"vide");
    let hdlr = box_bytes(b"hdlr", &fullbox(0, 0, &hdlr_body));

    let vmhd = box_bytes(b"vmhd", &fullbox(0, 1, &[0u8; 8]));
    let url_box = box_bytes(b"url ", &fullbox(0, 1, &[]));
    let mut dref_body = Vec::new();
    dref_body.extend_from_slice(&be32(1));
    dref_body.extend_from_slice(&url_box);
    let dinf = box_bytes(b"dinf", &box_bytes(b"dref", &fullbox(0, 0, &dref_body)));

    // stsd -> avc1 (78-byte VisualSampleEntry header) -> avcC
    let mut avc1_body = vec![0u8; 78];
    avc1_body[6..8].copy_from_slice(&1u16.to_be_bytes());
    avc1_body[24..26].copy_from_slice(&16u16.to_be_bytes()); // width
    avc1_body[26..28].copy_from_slice(&16u16.to_be_bytes()); // height
    avc1_body[28..32].copy_from_slice(&be32(0x0048_0000));
    avc1_body[32..36].copy_from_slice(&be32(0x0048_0000));
    avc1_body[40..42].copy_from_slice(&1u16.to_be_bytes());
    avc1_body[74..76].copy_from_slice(&24u16.to_be_bytes());
    avc1_body[76..78].copy_from_slice(&0xFFFFu16.to_be_bytes());
    avc1_body.extend_from_slice(&box_bytes(b"avcC", H264_16X16_AVCC));
    let avc1 = box_bytes(b"avc1", &avc1_body);

    let mut stsd_body = Vec::new();
    stsd_body.extend_from_slice(&be32(1));
    stsd_body.extend_from_slice(&avc1);
    let stsd = box_bytes(b"stsd", &fullbox(0, 0, &stsd_body));

    let mut stts_body = Vec::new();
    stts_body.extend_from_slice(&be32(1));
    stts_body.extend_from_slice(&be32(frame_count));
    stts_body.extend_from_slice(&be32(frame_duration_ms));
    let stts = box_bytes(b"stts", &fullbox(0, 0, &stts_body));

    let mut stsc_body = Vec::new();
    stsc_body.extend_from_slice(&be32(1));
    stsc_body.extend_from_slice(&be32(1));
    stsc_body.extend_from_slice(&be32(frame_count));
    stsc_body.extend_from_slice(&be32(1));
    let stsc = box_bytes(b"stsc", &fullbox(0, 0, &stsc_body));

    let mut stsz_body = Vec::new();
    stsz_body.extend_from_slice(&be32(sample_size));
    stsz_body.extend_from_slice(&be32(frame_count));
    let stsz = box_bytes(b"stsz", &fullbox(0, 0, &stsz_body));

    let mut stco_body = Vec::new();
    stco_body.extend_from_slice(&be32(1));
    stco_body.extend_from_slice(&be32(data_offset));
    let stco = box_bytes(b"stco", &fullbox(0, 0, &stco_body));

    let mut stss_body = Vec::new();
    stss_body.extend_from_slice(&be32(frame_count));
    for i in 1..=frame_count {
        stss_body.extend_from_slice(&be32(i));
    }
    let stss = box_bytes(b"stss", &fullbox(0, 0, &stss_body));

    let mut stbl_body = Vec::new();
    for b in [&stsd, &stts, &stsc, &stsz, &stco, &stss] {
        stbl_body.extend_from_slice(b);
    }
    let stbl = box_bytes(b"stbl", &stbl_body);

    let mut minf_body = Vec::new();
    minf_body.extend_from_slice(&vmhd);
    minf_body.extend_from_slice(&dinf);
    minf_body.extend_from_slice(&stbl);
    let minf = box_bytes(b"minf", &minf_body);

    let mut mdia_body = Vec::new();
    mdia_body.extend_from_slice(&mdhd);
    mdia_body.extend_from_slice(&hdlr);
    mdia_body.extend_from_slice(&minf);
    let mdia = box_bytes(b"mdia", &mdia_body);

    let mut trak_body = Vec::new();
    trak_body.extend_from_slice(&tkhd);
    trak_body.extend_from_slice(&mdia);
    box_bytes(b"trak", &trak_body)
}

/// P1-03a regression: seeking an audio-only file while paused must NOT trigger `Ended`
/// (there is no decodable video stream to clear `force_one_frame`).
#[test]
fn paused_wav_seek_does_not_emit_ended_in_native_and_fallback() {
    let path = unique_temp_path("p1_03_paused_wav", "wav");
    std::fs::write(&path, build_pcm_wav(3)).expect("write wav fixture");

    for prefer_native in [true, false] {
        let mut player = Player::builder()
            .input(&path)
            .prefer_native_pipeline(prefer_native)
            .volume(0.0)
            .build()
            .expect("build wav player");

        assert!(
            watch(&mut player, Duration::from_secs(2), |ev| {
                matches!(ev, PlayerEvent::Tracks { .. })
            }),
            "native={prefer_native}: expected Tracks event"
        );
        player.pause().expect("pause wav");
        assert!(
            watch(&mut player, Duration::from_secs(1), |ev| {
                matches!(ev, PlayerEvent::Snapshot(s) if s.status == "paused")
            }),
            "native={prefer_native}: expected paused Snapshot"
        );

        let target = Duration::from_millis(400);
        player.seek(target).expect("seek paused wav");
        let seek_gen = player.generation();

        let mut seek_ack = false;
        let ended = watch(&mut player, Duration::from_millis(600), |ev| match ev {
            PlayerEvent::SeekCompleted {
                position,
                generation,
            } if generation == seek_gen => {
                seek_ack = position == target;
                false
            }
            PlayerEvent::Ended { generation } if generation == seek_gen => true,
            _ => false,
        });

        assert!(
            seek_ack,
            "native={prefer_native}: expected SeekCompleted(400ms)"
        );
        assert!(
            !ended,
            "native={prefer_native}: paused audio-only seek must not emit Ended"
        );

        let _ = player.shutdown();
    }

    let _ = std::fs::remove_file(&path);
}

/// P1-03b regression: opening a video with `autoplay(false)` and immediately seeking to 5.5 s
/// before any frame has been decoded must keep the 0-based media time origin so the first
/// preview frame and `Player::position()` reflect ~5.0–5.5 s rather than jumping back to 0 ns.
#[test]
fn initial_seek_with_autoplay_false_preserves_media_time_origin() {
    // 14 keyframes × 500 ms = 7.0 s video (keyframes at 0.0 s, 0.5 s, ..., 5.0 s, 5.5 s, ...).
    let path = unique_temp_path("p1_03_initial_seek", "mp4");
    std::fs::write(&path, build_h264_mp4(14, 500)).expect("write h264 fixture");

    let mut player = Player::builder()
        .input(&path)
        .prefer_native_pipeline(true)
        .autoplay(false)
        .volume(0.0)
        .build()
        .expect("build paused video player");

    let target = Duration::from_millis(5500);
    player.seek(target).expect("initial seek");
    let seek_gen = player.generation();

    let mut ack = None;
    let mut first_frame_pts = None;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && first_frame_pts.is_none() {
        match player.poll_event() {
            Some(PlayerEvent::SeekCompleted {
                position,
                generation,
            }) if generation == seek_gen => {
                ack = Some(position);
            }
            Some(PlayerEvent::VideoFrame {
                pts, generation, ..
            }) if generation == seek_gen => {
                first_frame_pts = Some(pts);
            }
            Some(_) => {}
            None => thread::sleep(Duration::from_millis(4)),
        }
    }

    assert_eq!(ack, Some(target), "expected SeekCompleted at 5.5 s");
    let frame_pts = first_frame_pts.expect("expected preview VideoFrame after paused seek");
    assert!(
        frame_pts >= Duration::from_secs(5) && frame_pts <= target,
        "preview frame pts must match media timeline (~5.0..=5.5s), got {frame_pts:?}"
    );
    assert!(
        player.position() >= Duration::from_secs(5) && player.position() <= target,
        "Player::position() must stay at ~5.0..=5.5s, got {:?}",
        player.position()
    );

    let _ = player.shutdown();
    let _ = std::fs::remove_file(&path);
}

/// P1-04 regression (end-to-end):
/// 1. Saturating the 64-slot command queue causes `select_audio_track` to return
///    `Err(CommandQueueFull)` WITHOUT incrementing `player.generation()`.
/// 2. Letting a fast-playing video saturate the 128-slot event queue without polling
///    still reliably delivers `PlayerEvent::Ended` once drained, and subsequent
///    commands return `Err(PlayerError::ShutDown)` instead of `CommandQueueFull`.
#[test]
fn queue_saturation_and_event_backpressure_preserve_generation_and_ended() {
    // 160 keyframes × 10 ms = 1.6 s at 1× (~400 ms at 4×) -> produces >320 VideoFrame + PositionChanged events!
    let path = unique_temp_path("p1_04_backpressure", "mp4");
    std::fs::write(&path, build_h264_mp4(160, 10)).expect("write high-event h264 fixture");

    // Part A: Command queue saturation must not advance generation.
    {
        let mut player = Player::builder()
            .input(&path)
            .prefer_native_pipeline(true)
            .volume(0.0)
            .build()
            .expect("build player");

        let sent = (0..256)
            .take_while(|_| player.set_volume(0.0).is_ok())
            .count();
        assert!(sent < 256, "command queue should saturate within 256 tries");
        let gen_before = player.generation();
        let res = player.select_audio_track(1);
        assert!(
            matches!(
                res,
                Err(PlayerError::CommandQueueFull) | Err(PlayerError::ShutDown)
            ),
            "expected queue error, got {res:?}"
        );
        assert_eq!(
            player.generation(),
            gen_before,
            "failed track switch must not advance generation"
        );
        let _ = player.shutdown();
    }

    // Part B: Unpolled 4× playback saturates the 128-slot streaming channel; Ended must still arrive.
    {
        let mut player = Player::builder()
            .input(&path)
            .prefer_native_pipeline(true)
            .playback_rate(4.0)
            .volume(0.0)
            .build()
            .expect("build 4x player");

        // Wait well past the ~400 ms 4× runtime without polling a single event.
        thread::sleep(Duration::from_millis(1200));

        let mut received = 0usize;
        let mut saw_ended = false;
        while let Some(ev) = player.poll_event() {
            received += 1;
            if matches!(ev, PlayerEvent::Ended { .. }) {
                saw_ended = true;
            }
        }
        assert!(
            received > 128,
            "expected >128 events (128 buffered + terminal Ended), got {received}"
        );
        assert!(
            saw_ended,
            "Ended must be delivered even after 128-slot event queue saturated"
        );
        assert_eq!(player.state(), PlayerState::Ended);

        let after_play = player.play();
        assert!(
            matches!(after_play, Err(PlayerError::ShutDown)),
            "finished worker must report ShutDown rather than CommandQueueFull, got {after_play:?}"
        );
    }

    let _ = std::fs::remove_file(&path);
}

/// P1-03 adjacent regression: opening a 3-second WAV with `autoplay(false)`, seeking to
/// 400 ms while paused, remaining paused for ~1.35 s, and then calling `play()` must resume
/// near 400 ms rather than counting the 1.35 s pause duration into playback position (~1.76 s).
#[test]
fn paused_wav_seek_then_resume_does_not_count_pause_duration() {
    let path = unique_temp_path("p1_03_paused_resume_wav", "wav");
    std::fs::write(&path, build_pcm_wav(3)).expect("write 3s wav fixture");

    for prefer_native in [true, false] {
        let mut player = Player::builder()
            .input(&path)
            .prefer_native_pipeline(prefer_native)
            .autoplay(false)
            .volume(0.0)
            .build()
            .expect("build paused wav player");

        assert!(
            watch(&mut player, Duration::from_secs(3), |ev| {
                matches!(ev, PlayerEvent::Snapshot(s) if s.status == "paused")
            }),
            "native={prefer_native}: expected initial paused snapshot"
        );

        let target = Duration::from_millis(400);
        player.seek(target).expect("seek paused wav");
        let seek_gen = player.generation();
        assert!(
            watch(&mut player, Duration::from_secs(2), |ev| {
                matches!(
                    ev,
                    PlayerEvent::SeekCompleted {
                        position,
                        generation,
                    } if generation == seek_gen && position == target
                )
            }),
            "native={prefer_native}: expected SeekCompleted(400ms)"
        );

        // Remain paused for 1.35 s: must not end and position must stay at 400 ms.
        let ended_while_paused = watch(&mut player, Duration::from_millis(1350), |ev| {
            matches!(ev, PlayerEvent::Ended { .. })
        });
        assert!(
            !ended_while_paused,
            "native={prefer_native}: must not emit Ended while paused"
        );
        assert_eq!(
            player.position(),
            target,
            "native={prefer_native}: paused position must remain 400 ms"
        );

        player.play().expect("resume paused wav");
        let mut first_pos = None;
        let mut audio_peak = false;
        let resumed = watch(&mut player, Duration::from_millis(1400), |ev| {
            match ev {
                PlayerEvent::PositionChanged {
                    position,
                    generation,
                } if generation == seek_gen => {
                    first_pos.get_or_insert(position);
                }
                PlayerEvent::AudioLevel {
                    peak, generation, ..
                } if generation == seek_gen => {
                    audio_peak |= peak > 0.0;
                }
                _ => {}
            }
            first_pos.is_some() && audio_peak
        });

        assert!(
            resumed,
            "native={prefer_native}: expected PositionChanged and non-zero AudioLevel after play"
        );
        let pos = first_pos.expect("first PositionChanged");
        assert!(
            pos >= Duration::from_millis(300) && pos < Duration::from_millis(750),
            "native={prefer_native}: resumed position jumped past pause duration: {pos:?}"
        );

        let _ = player.shutdown();
    }

    let _ = std::fs::remove_file(&path);
}

/// P1-04 adjacent regression: saturating the 128-slot streaming event channel by
/// pausing polling during playback must NOT drop subsequent control replies
/// (`Pause`, `Seek`, `SelectAudioTrack`, `SetPlaybackRate`, `SetVolume`) in either
/// the native pipeline or the Symphonia fallback worker.
#[test]
fn saturated_stream_delivers_all_control_replies_in_native_and_fallback() {
    // 450 frames × 21.33 ms ≈ 9.6 s dual-AAC MP4 -> saturates 128-slot stream channel easily.
    let path = unique_temp_path("p1_04_saturated_controls", "mp4");
    std::fs::write(&path, build_dual_aac_mp4(450)).expect("write long dual-aac fixture");

    for prefer_native in [true, false] {
        let mut player = Player::builder()
            .input(&path)
            .prefer_native_pipeline(prefer_native)
            .autoplay(true)
            .volume(0.0)
            .build()
            .expect("build dual-aac player");

        let mut next_audio = None;
        let ready = watch(&mut player, Duration::from_secs(3), |ev| {
            if let PlayerEvent::Tracks {
                audio_tracks,
                active_audio,
                ..
            } = ev
            {
                next_audio = audio_tracks
                    .iter()
                    .find(|t| Some(t.index) != active_audio)
                    .map(|t| t.index);
            }
            next_audio.is_some()
        });
        assert!(ready, "native={prefer_native}: expected Tracks event");
        let selected = next_audio.expect("second audio track");

        // Stop polling long enough for streaming AudioLevel / PositionChanged events to saturate
        // the 128-slot streaming channel.
        thread::sleep(Duration::from_millis(1600));

        player.pause().expect("pause");
        let seek_target = Duration::from_secs(2);
        player.seek(seek_target).expect("seek");
        let seek_gen = player.generation();
        player
            .select_audio_track(selected)
            .expect("select_audio_track");
        let latest_gen = player.generation();
        player.set_playback_rate(1.5).expect("set_playback_rate");
        player.set_volume(0.25).expect("set_volume");

        thread::sleep(Duration::from_millis(400));

        let mut pause_ack = false;
        let mut seek_ack = false;
        let mut track_ack = false;
        let mut rate_ack = false;
        let mut volume_ack = false;
        while let Some(ev) = player.poll_event() {
            match ev {
                PlayerEvent::SeekCompleted {
                    position,
                    generation,
                } if generation == seek_gen => {
                    seek_ack = position == seek_target;
                }
                PlayerEvent::Tracks {
                    active_audio,
                    generation,
                    ..
                } if generation == latest_gen => {
                    track_ack = active_audio == Some(selected);
                }
                PlayerEvent::Snapshot(s) => {
                    pause_ack |= s.status == "paused" && !s.playing;
                    rate_ack |= s.status == "rate" && s.generation == latest_gen;
                    volume_ack |= s.status == "volume"
                        && s.generation == latest_gen
                        && (s.volume - 0.25).abs() < 1e-4;
                }
                _ => {}
            }
        }

        assert!(
            pause_ack && seek_ack && track_ack && rate_ack && volume_ack,
            "native={prefer_native}: missing control acks under stream saturation: \
             pause={pause_ack} seek={seek_ack} track={track_ack} rate={rate_ack} volume={volume_ack}"
        );
        assert!(
            !player.is_playing(),
            "native={prefer_native}: player cache must reflect paused state"
        );
        assert!(
            (player.volume() - 0.25).abs() < 1e-4,
            "native={prefer_native}: player cache must reflect volume 0.25, got {}",
            player.volume()
        );

        let _ = player.shutdown();
    }

    let _ = std::fs::remove_file(&path);
}

/// P1-06 regression (end-to-end): playing a short dual-AAC MP4 to completion must
/// deliver `opening` / `Tracks` / `playing` strictly before `Ended`, with zero
/// events arriving after `Ended`.
#[test]
fn short_file_delivers_opening_and_playing_snapshots_strictly_before_ended() {
    // 10 frames ≈ 213 ms short AAC MP4.
    let path = unique_temp_path("p1_06_short_order", "mp4");
    std::fs::write(&path, build_dual_aac_mp4(10)).expect("write short dual-aac fixture");

    for prefer_native in [true, false] {
        let mut player = Player::builder()
            .input(&path)
            .prefer_native_pipeline(prefer_native)
            .volume(0.0)
            .build()
            .expect("build short player");

        let mut events = Vec::new();
        let mut saw_ended = false;
        let deadline = Instant::now() + Duration::from_secs(3);
        while !saw_ended && Instant::now() < deadline {
            if let Some(ev) = player.poll_event() {
                if matches!(ev, PlayerEvent::Ended { .. }) {
                    saw_ended = true;
                }
                events.push(ev);
            } else {
                thread::sleep(Duration::from_millis(2));
            }
        }
        // Brief settle then confirm nothing arrives after Ended.
        thread::sleep(Duration::from_millis(50));
        while let Some(ev) = player.poll_event() {
            events.push(ev);
        }

        assert!(saw_ended, "native={prefer_native}: expected Ended");
        let ended_idx = events
            .iter()
            .position(|e| matches!(e, PlayerEvent::Ended { .. }))
            .expect("Ended present");
        let playing_idx = events
            .iter()
            .position(|e| matches!(e, PlayerEvent::Snapshot(s) if s.playing))
            .expect("playing Snapshot present");
        let tracks_idx = events
            .iter()
            .position(|e| matches!(e, PlayerEvent::Tracks { .. }))
            .expect("Tracks present");

        assert!(
            playing_idx < ended_idx && tracks_idx < ended_idx,
            "native={prefer_native}: playing({playing_idx}) and tracks({tracks_idx}) must precede Ended({ended_idx})"
        );
        assert_eq!(
            ended_idx,
            events.len() - 1,
            "native={prefer_native}: no event may follow Ended"
        );
        assert_eq!(player.state(), PlayerState::Ended);
    }

    let _ = std::fs::remove_file(&path);
}

/// P1-05 regression (end-to-end): after saturating the 128-slot streaming channel
/// during native video playback, pausing and seeking to 7.0 s must still deliver
/// the single new preview `VideoFrame` at the seeked generation and PTS rather than
/// leaving the host with only a stale generation-1 frame.
#[test]
fn saturated_stream_delivers_paused_seek_preview_frame() {
    // 240 keyframes × 50 ms = 12.0 s @ 20 fps -> 20 frames + 20 positions/sec = 40 stream events/s;
    // at 2× speed (>80 stream events/s), sleeping 2.2 s saturates all 128 stream slots while staying well before EOF.
    let path = unique_temp_path("p1_05_saturated_preview", "mp4");
    std::fs::write(&path, build_h264_mp4(240, 50)).expect("write 20fps h264 fixture");

    let mut player = Player::builder()
        .input(&path)
        .prefer_native_pipeline(true)
        .autoplay(true)
        .playback_rate(2.0)
        .volume(0.0)
        .build()
        .expect("build native video player");

    assert!(
        watch(&mut player, Duration::from_secs(3), |ev| {
            matches!(ev, PlayerEvent::VideoFrame { .. })
        }),
        "expected initial video frame"
    );

    // Stop polling long enough to saturate the 128-slot stream channel with gen-1 frames.
    thread::sleep(Duration::from_millis(2200));

    player.pause().expect("pause");
    let target = Duration::from_secs(7);
    player.seek(target).expect("seek while paused");
    let seek_gen = player.generation();

    // Give the worker time to process Pause + Seek and decode + emit the paused preview frame.
    thread::sleep(Duration::from_millis(450));

    let (events, latest_frame) = player.poll_all();
    let saw_pause = events
        .iter()
        .any(|e| matches!(e, PlayerEvent::Snapshot(s) if s.status == "paused" && !s.playing));
    let saw_seek = events.iter().any(|e| {
        matches!(
            e,
            PlayerEvent::SeekCompleted {
                position,
                generation,
            } if *generation == seek_gen && *position == target
        )
    });
    assert!(saw_pause, "expected paused Snapshot under saturation");
    assert!(saw_seek, "expected SeekCompleted(7s) under saturation");

    match latest_frame.expect("expected paused seek preview VideoFrame under saturation") {
        PlayerEvent::VideoFrame {
            pts, generation, ..
        } => {
            assert_eq!(
                generation, seek_gen,
                "latest frame must be the fresh seek preview (gen={seek_gen}), not stale gen={generation} (pts={pts:?})"
            );
            assert!(
                pts >= Duration::from_millis(6800) && pts <= target,
                "preview frame pts must match seek target ~7.0s, got {pts:?}"
            );
        }
        other => panic!("expected VideoFrame, got {other:?}"),
    }
    assert!(!player.is_playing(), "player must remain paused");
    assert!(
        player.position() >= Duration::from_millis(6800) && player.position() <= target,
        "player position must reflect ~7.0s preview, got {:?}",
        player.position()
    );

    let _ = player.shutdown();
    let _ = std::fs::remove_file(&path);
}

fn flac_crc8(data: &[u8]) -> u8 {
    let mut crc = 0u8;
    for &b in data {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x07
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn flac_crc16(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &b in data {
        crc ^= u16::from(b) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x8005
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// Build a valid multi-frame 48 kHz mono 24-bit FLAC stream (`frame_count` × 4096 samples).
fn build_24bit_mono_flac(frame_count: u64) -> Vec<u8> {
    let blocksize: u16 = 4096;
    let sample_rate: u32 = 48_000;
    let channels: u16 = 1;
    let bps: u16 = 24;
    let total_samples = frame_count * u64::from(blocksize);

    let mut si = vec![0u8; 34];
    si[0..2].copy_from_slice(&blocksize.to_be_bytes());
    si[2..4].copy_from_slice(&blocksize.to_be_bytes());
    si[10] = (sample_rate >> 12) as u8;
    si[11] = (sample_rate >> 4) as u8;
    let ch_m1 = ((channels - 1) as u8) & 0x07;
    let bps_m1 = ((bps - 1) as u8) & 0x1F;
    si[12] = (((sample_rate & 0x0F) as u8) << 4) | (ch_m1 << 1) | (bps_m1 >> 4);
    si[13] = ((bps_m1 & 0x0F) << 4) | (((total_samples >> 32) as u8) & 0x0F);
    si[14..18].copy_from_slice(&(total_samples as u32).to_be_bytes());

    let mut out = Vec::from(&b"fLaC"[..]);
    out.push(0x80); // last metadata block, type 0 = STREAMINFO
    out.push(0x00);
    out.push(0x00);
    out.push(34);
    out.extend_from_slice(&si);

    for frame_num in 0..frame_count {
        assert!(frame_num < 0x80, "single-byte UTF-8 coded frame number");
        let mut frame = vec![
            0xFF,
            0xF8,           // sync + fixed blocksize
            (12 << 4) | 10, // bs_code=12 (4096), sr_code=10 (48000 Hz)
            6 << 1,         // ch_code=0 (mono), bps_code=6 (24-bit), reserved=0
            frame_num as u8,
        ];
        frame.push(flac_crc8(&frame));
        // Subframe header: pad=0, CONSTANT=0, wasted=0
        frame.push(0x00);
        // Alternating 24-bit signed square-wave amplitude (+0x10_0000 / -0x10_0000)
        let sample24: i32 = if frame_num % 2 == 0 {
            0x10_0000
        } else {
            -0x10_0000
        };
        frame.push(((sample24 >> 16) & 0xFF) as u8);
        frame.push(((sample24 >> 8) & 0xFF) as u8);
        frame.push((sample24 & 0xFF) as u8);
        let c16 = flac_crc16(&frame);
        frame.extend_from_slice(&c16.to_be_bytes());
        out.extend_from_slice(&frame);
    }
    out
}

/// Minimal valid 8×8 grayscale baseline JPEG (using Annex K default Huffman tables).
fn build_synthetic_jpeg_8x8() -> Vec<u8> {
    let mut out = Vec::new();
    // SOI
    out.extend_from_slice(&[0xFF, 0xD8]);
    // DQT: table 0, 8-bit, all ones
    out.extend_from_slice(&[0xFF, 0xDB, 0x00, 0x43, 0x00]);
    out.extend_from_slice(&[1u8; 64]);
    // SOF0: 8-bit, 8x8, 1 component (id=1, sampling=1x1, qt=0)
    out.extend_from_slice(&[
        0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00, 0x08, 0x00, 0x08, 0x01, 0x01, 0x11, 0x00,
    ]);
    // SOS: 1 component (id=1, dc=0 ac=0), spectral 0..63
    out.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00]);
    // Entropy scan: DC diff=0 ('00') + AC EOB ('1010') + padding '11' -> 0x2B (solid gray 128)
    out.push(0x2B);
    // EOI
    out.extend_from_slice(&[0xFF, 0xD9]);
    out
}

/// Build a valid MJPEG-in-MP4 (`jpeg` sample entry) with `frame_count` frames.
fn build_mjpeg_mp4(frame_count: u32, frame_duration_ms: u32) -> Vec<u8> {
    let jpeg = build_synthetic_jpeg_8x8();
    let ftyp = box_bytes(b"ftyp", b"isom\0\0\x02\0isomiso2mp41");

    let sample_size = jpeg.len() as u32;
    let mut mdat_payload = Vec::with_capacity((sample_size * frame_count) as usize);
    for _ in 0..frame_count {
        mdat_payload.extend_from_slice(&jpeg);
    }
    let mdat = box_bytes(b"mdat", &mdat_payload);

    let data_offset = (ftyp.len() + 8) as u32;
    let duration_ms = frame_count * frame_duration_ms;

    let mut mvhd_body = vec![0u8; 96];
    mvhd_body[8..12].copy_from_slice(&be32(1000));
    mvhd_body[12..16].copy_from_slice(&be32(duration_ms));
    mvhd_body[16..20].copy_from_slice(&be32(0x0001_0000));
    mvhd_body[20..22].copy_from_slice(&0x0100u16.to_be_bytes());
    mvhd_body[32..36].copy_from_slice(&be32(0x0001_0000));
    mvhd_body[48..52].copy_from_slice(&be32(0x0001_0000));
    mvhd_body[64..68].copy_from_slice(&be32(0x4000_0000));
    mvhd_body[92..96].copy_from_slice(&be32(2));
    let mvhd = box_bytes(b"mvhd", &fullbox(0, 0, &mvhd_body));

    let mut tkhd_body = vec![0u8; 80];
    tkhd_body[8..12].copy_from_slice(&be32(1));
    tkhd_body[16..20].copy_from_slice(&be32(duration_ms));
    tkhd_body[36..40].copy_from_slice(&be32(0x0001_0000));
    tkhd_body[52..56].copy_from_slice(&be32(0x0001_0000));
    tkhd_body[68..72].copy_from_slice(&be32(0x4000_0000));
    tkhd_body[72..76].copy_from_slice(&be32(8 << 16));
    tkhd_body[76..80].copy_from_slice(&be32(8 << 16));
    let tkhd = box_bytes(b"tkhd", &fullbox(0, 3, &tkhd_body));

    let mut mdhd_body = vec![0u8; 20];
    mdhd_body[8..12].copy_from_slice(&be32(1000));
    mdhd_body[12..16].copy_from_slice(&be32(duration_ms));
    let mdhd = box_bytes(b"mdhd", &fullbox(0, 0, &mdhd_body));

    let mut hdlr_body = vec![0u8; 21];
    hdlr_body[4..8].copy_from_slice(b"vide");
    let hdlr = box_bytes(b"hdlr", &fullbox(0, 0, &hdlr_body));

    let vmhd = box_bytes(b"vmhd", &fullbox(0, 1, &[0u8; 8]));
    let url_box = box_bytes(b"url ", &fullbox(0, 1, &[]));
    let mut dref_body = Vec::new();
    dref_body.extend_from_slice(&be32(1));
    dref_body.extend_from_slice(&url_box);
    let dinf = box_bytes(b"dinf", &box_bytes(b"dref", &fullbox(0, 0, &dref_body)));

    // stsd -> jpeg (78-byte VisualSampleEntry)
    let mut jpeg_entry_body = vec![0u8; 78];
    jpeg_entry_body[6..8].copy_from_slice(&1u16.to_be_bytes());
    jpeg_entry_body[24..26].copy_from_slice(&8u16.to_be_bytes());
    jpeg_entry_body[26..28].copy_from_slice(&8u16.to_be_bytes());
    jpeg_entry_body[28..32].copy_from_slice(&be32(0x0048_0000));
    jpeg_entry_body[32..36].copy_from_slice(&be32(0x0048_0000));
    jpeg_entry_body[40..42].copy_from_slice(&1u16.to_be_bytes());
    jpeg_entry_body[74..76].copy_from_slice(&24u16.to_be_bytes());
    jpeg_entry_body[76..78].copy_from_slice(&0xFFFFu16.to_be_bytes());
    let jpeg_entry = box_bytes(b"jpeg", &jpeg_entry_body);

    let mut stsd_body = Vec::new();
    stsd_body.extend_from_slice(&be32(1));
    stsd_body.extend_from_slice(&jpeg_entry);
    let stsd = box_bytes(b"stsd", &fullbox(0, 0, &stsd_body));

    let mut stts_body = Vec::new();
    stts_body.extend_from_slice(&be32(1));
    stts_body.extend_from_slice(&be32(frame_count));
    stts_body.extend_from_slice(&be32(frame_duration_ms));
    let stts = box_bytes(b"stts", &fullbox(0, 0, &stts_body));

    let mut stsc_body = Vec::new();
    stsc_body.extend_from_slice(&be32(1));
    stsc_body.extend_from_slice(&be32(1));
    stsc_body.extend_from_slice(&be32(frame_count));
    stsc_body.extend_from_slice(&be32(1));
    let stsc = box_bytes(b"stsc", &fullbox(0, 0, &stsc_body));

    let mut stsz_body = Vec::new();
    stsz_body.extend_from_slice(&be32(sample_size));
    stsz_body.extend_from_slice(&be32(frame_count));
    let stsz = box_bytes(b"stsz", &fullbox(0, 0, &stsz_body));

    let mut stco_body = Vec::new();
    stco_body.extend_from_slice(&be32(1));
    stco_body.extend_from_slice(&be32(data_offset));
    let stco = box_bytes(b"stco", &fullbox(0, 0, &stco_body));

    let mut stss_body = Vec::new();
    stss_body.extend_from_slice(&be32(frame_count));
    for i in 1..=frame_count {
        stss_body.extend_from_slice(&be32(i));
    }
    let stss = box_bytes(b"stss", &fullbox(0, 0, &stss_body));

    let mut stbl_body = Vec::new();
    for b in [&stsd, &stts, &stsc, &stsz, &stco, &stss] {
        stbl_body.extend_from_slice(b);
    }
    let stbl = box_bytes(b"stbl", &stbl_body);

    let mut minf_body = Vec::new();
    minf_body.extend_from_slice(&vmhd);
    minf_body.extend_from_slice(&dinf);
    minf_body.extend_from_slice(&stbl);
    let minf = box_bytes(b"minf", &minf_body);

    let mut mdia_body = Vec::new();
    mdia_body.extend_from_slice(&mdhd);
    mdia_body.extend_from_slice(&hdlr);
    mdia_body.extend_from_slice(&minf);
    let mdia = box_bytes(b"mdia", &mdia_body);

    let mut trak_body = Vec::new();
    trak_body.extend_from_slice(&tkhd);
    trak_body.extend_from_slice(&mdia);
    let trak = box_bytes(b"trak", &trak_body);

    let mut moov_body = Vec::new();
    moov_body.extend_from_slice(&mvhd);
    moov_body.extend_from_slice(&trak);
    let moov = box_bytes(b"moov", &moov_body);

    let mut file = Vec::with_capacity(ftyp.len() + mdat.len() + moov.len());
    file.extend_from_slice(&ftyp);
    file.extend_from_slice(&mdat);
    file.extend_from_slice(&moov);
    file
}

#[test]
fn native_wav_emits_pure_rust_decoder_info() {
    let path = unique_temp_path("native_wav_decoder_info", "wav");
    std::fs::write(&path, build_pcm_wav(1)).expect("write wav fixture");

    let mut player = Player::builder()
        .input(&path)
        .prefer_native_pipeline(true)
        .playback_rate(4.0)
        .volume(0.0)
        .build()
        .expect("build wav player");

    let mut info = None;
    let mut saw_audio_level = false;
    let mut saw_ended = false;
    let done = watch(&mut player, Duration::from_secs(5), |ev| {
        match ev {
            PlayerEvent::DecoderInfo {
                demuxer,
                video_decoder,
                audio_decoder,
                ..
            } => {
                info = Some((demuxer, video_decoder, audio_decoder));
            }
            PlayerEvent::AudioLevel { peak, .. } if peak > 0.0 => {
                saw_audio_level = true;
            }
            PlayerEvent::Ended { .. } => {
                saw_ended = true;
            }
            _ => {}
        }
        saw_ended
    });

    assert!(done, "expected WAV playback to finish with Ended");
    assert_eq!(
        info,
        Some((
            "rsmpeg-format (wav)".to_string(),
            None,
            Some("rsmpeg-rust (PCM)".to_string()),
        ))
    );
    assert!(saw_audio_level, "expected non-zero AudioLevel from WAV PCM");
    assert_eq!(player.state(), PlayerState::Ended);

    let _ = player.shutdown();
    let _ = std::fs::remove_file(&path);
}

#[test]
fn native_flac_decodes_seeks_and_finishes() {
    // 20 frames × 4096 samples @ 48 kHz ≈ 1.706 s
    let path = unique_temp_path("native_flac_playback", "flac");
    std::fs::write(&path, build_24bit_mono_flac(20)).expect("write flac fixture");

    let mut player = Player::builder()
        .input(&path)
        .prefer_native_pipeline(true)
        .playback_rate(1.0)
        .volume(0.0)
        .build()
        .expect("build flac player");

    let mut info = None;
    let mut saw_level = false;
    let ready = watch(&mut player, Duration::from_secs(3), |ev| {
        match ev {
            PlayerEvent::DecoderInfo {
                demuxer,
                video_decoder,
                audio_decoder,
                ..
            } => {
                info = Some((demuxer, video_decoder, audio_decoder));
            }
            PlayerEvent::AudioLevel { peak, .. } if peak > 0.0 => {
                saw_level = true;
            }
            _ => {}
        }
        info.is_some() && saw_level
    });
    assert!(
        ready,
        "expected DecoderInfo and non-zero AudioLevel for FLAC"
    );
    assert_eq!(
        info,
        Some((
            "rsmpeg-format (flac)".to_string(),
            None,
            Some("rsmpeg-rust (FLAC)".to_string()),
        ))
    );

    let seek_target = Duration::from_millis(800);
    player.seek(seek_target).expect("seek flac");
    let seek_gen = player.generation();
    player
        .set_playback_rate(4.0)
        .expect("speed up tail of flac");

    let mut saw_seek = false;
    let mut saw_ended = false;
    let finished = watch(&mut player, Duration::from_secs(5), |ev| {
        match ev {
            PlayerEvent::SeekCompleted {
                position,
                generation,
            } if generation == seek_gen => {
                saw_seek = position == seek_target;
            }
            PlayerEvent::Ended { .. } => {
                saw_ended = true;
            }
            _ => {}
        }
        saw_seek && saw_ended
    });

    assert!(saw_seek, "expected SeekCompleted for FLAC");
    assert!(
        finished && saw_ended,
        "expected FLAC playback to reach Ended"
    );
    assert_eq!(player.state(), PlayerState::Ended);

    let _ = player.shutdown();
    let _ = std::fs::remove_file(&path);
}

#[test]
fn native_mjpeg_mp4_decodes_frames_and_steps_forward() {
    // 60 frames × 50 ms = 3.0 s @ 20 fps
    let path = unique_temp_path("native_mjpeg_step", "mp4");
    std::fs::write(&path, build_mjpeg_mp4(60, 50)).expect("write mjpeg mp4 fixture");

    let mut player = Player::builder()
        .input(&path)
        .prefer_native_pipeline(true)
        .autoplay(true)
        .volume(0.0)
        .build()
        .expect("build mjpeg player");

    let mut info = None;
    let mut first_frame_ok = false;
    let ready = watch(&mut player, Duration::from_secs(3), |ev| {
        match ev {
            PlayerEvent::DecoderInfo {
                demuxer,
                video_decoder,
                audio_decoder,
                ..
            } => {
                info = Some((demuxer, video_decoder, audio_decoder));
            }
            PlayerEvent::VideoFrame {
                width,
                height,
                rgba,
                ..
            } => {
                first_frame_ok = width == 8
                    && height == 8
                    && rgba.len() == 8 * 8 * 4
                    && rgba.iter().any(|&b| b != 0);
            }
            _ => {}
        }
        info.is_some() && first_frame_ok
    });
    assert!(ready, "expected DecoderInfo and non-zero MJPEG VideoFrame");
    assert_eq!(
        info,
        Some((
            "rsmpeg-format (mp4)".to_string(),
            Some("rsmpeg-rust (MJPEG)".to_string()),
            None,
        ))
    );

    player.pause().expect("pause mjpeg");
    let mut paused_pts = Duration::ZERO;
    let mut saw_pause = false;
    let paused = watch(&mut player, Duration::from_secs(2), |ev| {
        match ev {
            PlayerEvent::VideoFrame { pts, .. } => {
                paused_pts = pts;
            }
            PlayerEvent::Snapshot(s) if s.status == "paused" && !s.playing => {
                saw_pause = true;
            }
            _ => {}
        }
        saw_pause
    });
    assert!(paused, "expected paused snapshot");
    // Drain any in-flight frames emitted just before Pause took effect.
    thread::sleep(Duration::from_millis(40));
    while let Some(ev) = player.poll_event() {
        if let PlayerEvent::VideoFrame { pts, .. } = ev {
            paused_pts = pts;
        }
    }

    player.step_forward().expect("step forward");
    let step_gen = player.generation();
    assert!(!player.is_playing());
    assert_eq!(player.state(), PlayerState::Paused);

    let mut stepped_frame = None;
    let got_step = watch(&mut player, Duration::from_secs(2), |ev| {
        if let PlayerEvent::VideoFrame {
            width,
            height,
            rgba,
            pts,
            generation,
        } = ev
        {
            if generation == step_gen {
                stepped_frame = Some((width, height, rgba, pts));
                return true;
            }
        }
        false
    });
    assert!(got_step, "expected stepped VideoFrame at gen={step_gen}");
    let (sw, sh, srgba, spts) = stepped_frame.unwrap();
    assert_eq!((sw, sh), (8, 8));
    assert!(srgba.iter().any(|&b| b != 0));
    assert!(
        spts > paused_pts,
        "step_forward must advance PTS past paused_pts ({paused_pts:?}), got {spts:?}"
    );

    // Ensure no extra frames emit while remaining paused.
    thread::sleep(Duration::from_millis(120));
    let mut extra_frames = 0usize;
    while let Some(ev) = player.poll_event() {
        if matches!(ev, PlayerEvent::VideoFrame { .. }) {
            extra_frames += 1;
        }
    }
    assert_eq!(
        extra_frames, 0,
        "step_forward must advance exactly one frame"
    );
    assert!(!player.is_playing());
    assert_eq!(player.state(), PlayerState::Paused);

    let _ = player.shutdown();
    let _ = std::fs::remove_file(&path);
}

#[test]
fn native_mjpeg_mp4_seek_precise_lands_at_target_while_paused() {
    // 20 frames × 50 ms = 1.0 s (keyframes at 0, 50, 100, ..., 950 ms)
    let path = unique_temp_path("native_mjpeg_seek_precise", "mp4");
    std::fs::write(&path, build_mjpeg_mp4(20, 50)).expect("write mjpeg mp4 fixture");

    let mut player = Player::builder()
        .input(&path)
        .prefer_native_pipeline(true)
        .autoplay(false)
        .volume(0.0)
        .build()
        .expect("build paused mjpeg player");

    assert!(
        watch(&mut player, Duration::from_secs(2), |ev| {
            matches!(ev, PlayerEvent::Snapshot(s) if s.status == "paused")
        }),
        "expected initial paused snapshot"
    );

    let target = Duration::from_millis(450);
    player.seek_precise(target).expect("seek_precise");
    let seek_gen = player.generation();

    let mut seek_ack = false;
    let mut preview_pts = None;
    let got = watch(&mut player, Duration::from_secs(2), |ev| {
        match ev {
            PlayerEvent::SeekCompleted {
                position,
                generation,
            } if generation == seek_gen => {
                seek_ack = position == target;
            }
            PlayerEvent::VideoFrame {
                pts, generation, ..
            } if generation == seek_gen => {
                preview_pts = Some(pts);
            }
            _ => {}
        }
        seek_ack && preview_pts.is_some()
    });
    assert!(got, "expected SeekCompleted and preview VideoFrame");
    let pts = preview_pts.unwrap();
    assert!(
        pts >= target,
        "precise seek preview must drop pre-target frames and land >= {target:?}, got {pts:?}"
    );
    assert!(!player.is_playing());
    assert!(
        matches!(player.state(), PlayerState::Paused | PlayerState::Ready),
        "unexpected state after paused precise seek: {:?}",
        player.state()
    );

    let _ = player.shutdown();
    let _ = std::fs::remove_file(&path);
}

/// Upper bound for any single control acknowledgement; generous for CI, but tight enough to
/// catch a worker that stops servicing commands while it waits for the next frame.
const ACK_TIMEOUT: Duration = Duration::from_secs(2);

fn is_warning_for(ev: &PlayerEvent, generation: u64, needle: &str) -> bool {
    matches!(
        ev,
        PlayerEvent::Warning { message, generation: g }
            if *g == generation && message.contains(needle)
    )
}

/// Wait for an acknowledgement of a command issued just before this call, asserting it arrives
/// within [`ACK_TIMEOUT`].
fn expect_ack_within(
    player: &mut Player,
    what: &str,
    accept: impl FnMut(PlayerEvent) -> bool,
) -> Duration {
    let started = Instant::now();
    assert!(
        watch(player, ACK_TIMEOUT, accept),
        "{what}: no acknowledgement within {ACK_TIMEOUT:?}"
    );
    started.elapsed()
}

/// Invalid track indices must surface as `Warning` events on both pipelines (never a crash or
/// hang), and the player must still shut down cleanly afterwards.
#[test]
fn invalid_track_indices_emit_warning_without_crashing_native_and_fallback() {
    // One H.264 video track plus one AAC track so both selection paths are exercised.
    let path = unique_temp_path("invalid_track_idx", "mp4");
    std::fs::write(&path, build_h264_movie(1, 14, 500, true)).expect("write h264+aac fixture");

    for prefer_native in [true, false] {
        let mut player = Player::builder()
            .input(&path)
            .prefer_native_pipeline(prefer_native)
            .autoplay(false)
            .volume(0.0)
            .build()
            .expect("build h264+aac player");

        assert!(
            watch(&mut player, Duration::from_secs(3), |ev| {
                matches!(ev, PlayerEvent::Tracks { .. })
            }),
            "native={prefer_native}: expected Tracks event"
        );

        player
            .select_audio_track(99)
            .expect("enqueue invalid audio track");
        let audio_gen = player.generation();
        assert!(
            watch(&mut player, ACK_TIMEOUT, |ev| {
                is_warning_for(&ev, audio_gen, "invalid audio track")
            }),
            "native={prefer_native}: expected invalid-audio-track Warning at gen={audio_gen}"
        );

        player
            .select_video_track(99)
            .expect("enqueue invalid video track");
        let video_gen = player.generation();
        assert!(
            watch(&mut player, ACK_TIMEOUT, |ev| {
                is_warning_for(&ev, video_gen, "invalid video track")
            }),
            "native={prefer_native}: expected invalid-video-track Warning at gen={video_gen}"
        );

        assert!(
            player.shutdown().is_ok(),
            "native={prefer_native}: shutdown after invalid track selection must succeed"
        );
    }

    let _ = std::fs::remove_file(&path);
}

/// Stepping while paused must advance the generation once per step and leave the player paused.
/// On the native pipeline each step must also emit exactly one advancing preview frame.
#[test]
fn step_forward_while_paused_emits_frame_and_stays_paused_native_and_fallback() {
    // 20 keyframes × 250 ms = 5 s of 16×16 H.264, plus an AAC track so the fallback opens the file.
    // Only the native pipeline decodes H.264 MP4 video; the fallback is held to the control
    // contract (generation, paused state, no playing snapshot) without expecting preview frames.
    let path = unique_temp_path("step_forward_paused", "mp4");
    std::fs::write(&path, build_h264_movie(1, 20, 250, true)).expect("write h264+aac fixture");

    for prefer_native in [true, false] {
        let mut player = Player::builder()
            .input(&path)
            .prefer_native_pipeline(prefer_native)
            .autoplay(false)
            .volume(0.0)
            .build()
            .expect("build paused h264 player");

        assert!(
            watch(&mut player, Duration::from_secs(3), |ev| {
                matches!(ev, PlayerEvent::Snapshot(s) if s.status == "paused" && !s.playing)
            }),
            "native={prefer_native}: expected initial paused snapshot"
        );
        let base_gen = player.generation();

        let mut prev_pts: Option<Duration> = None;
        for step in 1..=2u64 {
            player.step_forward().expect("step_forward while paused");
            let step_gen = player.generation();
            assert_eq!(step_gen, base_gen + step, "step must bump generation once");
            assert!(!player.is_playing());
            assert_eq!(player.state(), PlayerState::Paused);

            let mut frame_pts = None;
            let mut saw_playing = false;
            // Native: wait for the preview frame. Fallback: nothing to wait for, so only
            // check that no playing snapshot is produced within a short window.
            let window = if prefer_native {
                Duration::from_secs(3)
            } else {
                Duration::from_millis(300)
            };
            let got = watch(&mut player, window, |ev| match ev {
                PlayerEvent::VideoFrame {
                    pts, generation, ..
                } if generation == step_gen => {
                    frame_pts = Some(pts);
                    true
                }
                PlayerEvent::Snapshot(s) if s.playing => {
                    saw_playing = true;
                    false
                }
                _ => false,
            });
            assert!(
                !saw_playing,
                "native={prefer_native}: step {step} must not report a playing snapshot"
            );
            if prefer_native {
                assert!(
                    got,
                    "native={prefer_native}: expected preview VideoFrame for step {step} (gen={step_gen})"
                );
                let pts = frame_pts.expect("frame pts recorded");
                if let Some(prev) = prev_pts {
                    assert!(
                        pts > prev,
                        "native={prefer_native}: step {step} must advance PTS past {prev:?}, got {pts:?}"
                    );
                }
                prev_pts = Some(pts);
            }
        }

        // Settle, then confirm no further frames or playing snapshots appear on their own.
        thread::sleep(Duration::from_millis(150));
        let mut extra_frames = 0usize;
        let mut playing_snapshots = 0usize;
        while let Some(ev) = player.poll_event() {
            match ev {
                PlayerEvent::VideoFrame { .. } => extra_frames += 1,
                PlayerEvent::Snapshot(s) if s.playing => playing_snapshots += 1,
                _ => {}
            }
        }
        assert_eq!(
            extra_frames, 0,
            "native={prefer_native}: stepping must not keep emitting frames"
        );
        assert_eq!(
            playing_snapshots, 0,
            "native={prefer_native}: player must stay paused after stepping"
        );
        assert!(!player.is_playing());
        assert!(!player.snapshot().playing);

        assert!(player.shutdown().is_ok());
    }

    let _ = std::fs::remove_file(&path);
}

/// A burst of 12 rapid seeks (alternating coarse and precise) on a paused H.264+AAC file must
/// settle on the final generation: one `SeekCompleted` at the last target (plus its preview frame
/// on the native pipeline), with no `Error` and no hang.
#[test]
fn rapid_consecutive_seeks_deliver_final_generation_preview_native_and_fallback() {
    let path = unique_temp_path("rapid_seeks", "mp4");
    std::fs::write(&path, build_h264_movie(1, 14, 500, true)).expect("write h264+aac fixture");

    for prefer_native in [true, false] {
        let mut player = Player::builder()
            .input(&path)
            .prefer_native_pipeline(prefer_native)
            .autoplay(false)
            .volume(0.0)
            .build()
            .expect("build paused h264+aac player");

        assert!(
            watch(&mut player, Duration::from_secs(3), |ev| {
                matches!(ev, PlayerEvent::Snapshot(s) if s.status == "paused")
            }),
            "native={prefer_native}: expected initial paused snapshot"
        );
        let start_gen = player.generation();

        let mut last_target = Duration::ZERO;
        for i in 1..=12u64 {
            last_target = Duration::from_millis(i * 300);
            let issued = if i % 2 == 0 {
                player.seek(last_target)
            } else {
                player.seek_precise(last_target)
            };
            issued.expect("rapid seek must enqueue within capacity");
        }
        let final_gen = player.generation();
        assert_eq!(
            final_gen,
            start_gen + 12,
            "each seek must bump generation once"
        );

        let mut ack = None;
        let mut preview = false;
        let mut error = None;
        let settled = watch(&mut player, Duration::from_secs(5), |ev| {
            match ev {
                PlayerEvent::SeekCompleted {
                    position,
                    generation,
                } if generation == final_gen => ack = Some(position),
                PlayerEvent::VideoFrame { generation, .. } if generation == final_gen => {
                    preview = true;
                }
                PlayerEvent::Error {
                    message,
                    generation,
                } if generation == final_gen => error = Some(message),
                _ => {}
            }
            // Only native decodes this H.264 MP4 video, so the preview is required there alone.
            ack.is_some() && (preview || !prefer_native)
        });

        assert!(
            error.is_none(),
            "native={prefer_native}: unexpected error for final seek: {error:?}"
        );
        assert!(
            settled,
            "native={prefer_native}: expected SeekCompleted (+ preview on native) at gen={final_gen} (ack={ack:?}, preview={preview})"
        );
        assert_eq!(
            ack,
            Some(last_target),
            "native={prefer_native}: final SeekCompleted must report the last target"
        );

        assert!(player.shutdown().is_ok());
    }

    let _ = std::fs::remove_file(&path);
}

/// With a slow 2 fps stream the worker spends most of its time waiting for the next frame. Rate
/// changes, pause, play and stop issued during that wait must each be acknowledged promptly.
#[test]
fn wait_loop_responds_promptly_to_rate_change_pause_and_stop() {
    // 10 keyframes × 500 ms = 5 s at 2 fps, plus an AAC track so the fallback opens the file.
    let path = unique_temp_path("wait_loop_ack", "mp4");
    std::fs::write(&path, build_h264_movie(1, 10, 500, true)).expect("write slow h264+aac fixture");

    for prefer_native in [true, false] {
        let mut player = Player::builder()
            .input(&path)
            .prefer_native_pipeline(prefer_native)
            .autoplay(true)
            .volume(0.0)
            .build()
            .expect("build slow h264 player");

        // Native decodes the first frame; the fallback has no video decoder for this file, so
        // a playing snapshot is the only start signal it provides.
        assert!(
            watch(&mut player, Duration::from_secs(3), |ev| {
                matches!(ev, PlayerEvent::VideoFrame { .. })
                    || matches!(&ev, PlayerEvent::Snapshot(s) if s.playing)
            }),
            "native={prefer_native}: expected playback to start"
        );

        player
            .set_playback_rate(2.0)
            .expect("enqueue playback rate");
        let rate_wait = expect_ack_within(
            &mut player,
            "set_playback_rate",
            |ev| matches!(ev, PlayerEvent::Snapshot(s) if s.status == "rate"),
        );
        assert!(
            rate_wait < ACK_TIMEOUT,
            "native={prefer_native}: rate ack took {rate_wait:?}"
        );

        player.pause().expect("enqueue pause");
        let pause_wait = expect_ack_within(
            &mut player,
            "pause",
            |ev| matches!(ev, PlayerEvent::Snapshot(s) if s.status == "paused" && !s.playing),
        );
        assert!(
            pause_wait < ACK_TIMEOUT,
            "native={prefer_native}: pause ack took {pause_wait:?}"
        );
        assert!(!player.is_playing());

        player.play().expect("enqueue play");
        let play_wait = expect_ack_within(
            &mut player,
            "play",
            |ev| matches!(ev, PlayerEvent::Snapshot(s) if s.status == "playing" && s.playing),
        );
        assert!(
            play_wait < ACK_TIMEOUT,
            "native={prefer_native}: play ack took {play_wait:?}"
        );
        assert!(player.is_playing());

        player.stop().expect("enqueue stop");
        let stop_wait = expect_ack_within(
            &mut player,
            "stop",
            |ev| matches!(ev, PlayerEvent::Snapshot(s) if s.status == "stopped" && !s.playing),
        );
        assert!(
            stop_wait < ACK_TIMEOUT,
            "native={prefer_native}: stop ack took {stop_wait:?}"
        );
        assert_eq!(player.state(), PlayerState::Ready);

        assert!(player.shutdown().is_ok());
    }

    let _ = std::fs::remove_file(&path);
}

/// Switching between two H.264 video tracks on the native pipeline must report the new active
/// track in `Tracks` and deliver a preview frame stamped with the new generation.
#[test]
fn multi_video_track_switch_native_updates_active_video_and_emits_frame() {
    // Two identical H.264 tracks (ids 1 and 2), 10 frames × 200 ms each.
    let path = unique_temp_path("multi_video_switch", "mp4");
    std::fs::write(&path, build_h264_movie(2, 10, 200, false)).expect("write 2-video fixture");

    let mut player = Player::builder()
        .input(&path)
        .prefer_native_pipeline(true)
        .autoplay(false)
        .volume(0.0)
        .build()
        .expect("build 2-video player");

    let mut initial_tracks = None;
    let found = watch(&mut player, Duration::from_secs(3), |ev| {
        if let PlayerEvent::Tracks { video_tracks, .. } = ev {
            let two = video_tracks.len() == 2;
            initial_tracks = Some(video_tracks);
            return two;
        }
        false
    });
    assert!(found, "expected initial Tracks event with two video tracks");
    let tracks = initial_tracks.expect("tracks recorded");
    assert!(tracks.iter().all(|t| t.is_video));
    let second_idx = tracks[1].index;
    assert_ne!(
        tracks[0].index, second_idx,
        "video track indices must differ"
    );

    player
        .select_video_track(second_idx)
        .expect("enqueue video track switch");
    let switch_gen = player.generation();

    let mut active_ok = false;
    let mut frame_ok = false;
    let done = watch(&mut player, Duration::from_secs(3), |ev| {
        match ev {
            PlayerEvent::Tracks {
                active_video,
                generation,
                ..
            } if generation == switch_gen => {
                active_ok = active_video == Some(second_idx);
            }
            PlayerEvent::VideoFrame { generation, .. } if generation == switch_gen => {
                frame_ok = true;
            }
            _ => {}
        }
        active_ok && frame_ok
    });
    assert!(
        done,
        "expected Tracks(active_video={second_idx}) and VideoFrame at gen={switch_gen} \
         (active_ok={active_ok}, frame_ok={frame_ok})"
    );

    assert!(player.shutdown().is_ok());
    let _ = std::fs::remove_file(&path);
}

fn ebml_vint(n: u64) -> Vec<u8> {
    for len in 1..=8usize {
        if n < (1u64 << (7 * len)) - 1 {
            let marked = n | (1u64 << (7 * len));
            return (0..len).rev().map(|i| (marked >> (8 * i)) as u8).collect();
        }
    }
    unreachable!("vint length fits within 8 bytes")
}

fn ebml_id(id: u32) -> Vec<u8> {
    let b = id.to_be_bytes();
    let skip = b.iter().position(|&x| x != 0).unwrap_or(3);
    b[skip..].to_vec()
}

fn ebml_el(id: u32, body: &[u8]) -> Vec<u8> {
    let mut out = ebml_id(id);
    out.extend_from_slice(&ebml_vint(body.len() as u64));
    out.extend_from_slice(body);
    out
}

fn ebml_uint(id: u32, val: u64) -> Vec<u8> {
    let bytes = val.to_be_bytes();
    let start = bytes.iter().position(|&b| b != 0).unwrap_or(7);
    ebml_el(id, &bytes[start..])
}

fn ebml_simple_block(track_num: u8, rel_ts: i16, keyframe: bool, payload: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(4 + payload.len());
    body.push(0x80 | (track_num & 0x7F));
    body.extend_from_slice(&rel_ts.to_be_bytes());
    body.push(if keyframe { 0x80 } else { 0x00 });
    body.extend_from_slice(payload);
    ebml_el(0xA3, &body)
}

fn build_h264_pcm_mkv() -> Vec<u8> {
    // 4 clusters × 100 ms = 400 ms; 2 video frames (50 ms apart) + 100 ms 48 kHz mono S16 PCM per cluster.
    let header_body = [
        ebml_uint(0x4286, 1),
        ebml_uint(0x42F7, 1),
        ebml_uint(0x42F2, 4),
        ebml_uint(0x42F3, 8),
        ebml_el(0x4282, b"matroska"),
        ebml_uint(0x4287, 4),
        ebml_uint(0x4285, 2),
    ]
    .concat();
    let ebml_header = ebml_el(0x1A45_DFA3, &header_body);

    let mut info_body = ebml_uint(0x002A_D7B1, 1_000_000);
    info_body.extend_from_slice(&ebml_el(0x4489, &400.0f64.to_be_bytes()));
    let info = ebml_el(0x1549_A966, &info_body);

    let video_pixel_el = [ebml_uint(0xB0, 16), ebml_uint(0xBA, 16)].concat();
    let video_track = ebml_el(
        0xAE,
        &[
            ebml_uint(0xD7, 1),
            ebml_uint(0x73C5, 1),
            ebml_uint(0x83, 1),
            ebml_el(0x86, b"V_MPEG4/ISO/AVC"),
            ebml_el(0x63A2, H264_16X16_AVCC),
            ebml_el(0xE0, &video_pixel_el),
        ]
        .concat(),
    );

    let audio_settings_el = [
        ebml_el(0xB5, &48_000.0f32.to_be_bytes()),
        ebml_uint(0x9F, 1),
        ebml_uint(0x6264, 16),
    ]
    .concat();
    let audio_track = ebml_el(
        0xAE,
        &[
            ebml_uint(0xD7, 2),
            ebml_uint(0x73C5, 2),
            ebml_uint(0x83, 2),
            ebml_el(0x86, b"A_PCM/INT/LIT"),
            ebml_el(0xE1, &audio_settings_el),
        ]
        .concat(),
    );
    let tracks = ebml_el(0x1654_AE6B, &[video_track, audio_track].concat());

    // 4800 mono S16LE samples = 100 ms per chunk.
    let mut pcm_chunk = Vec::with_capacity(4800 * 2);
    for i in 0..4800u32 {
        let s: i16 = if (i / 48) % 2 == 0 { 4000 } else { -4000 };
        pcm_chunk.extend_from_slice(&s.to_le_bytes());
    }

    let mut seg_body = Vec::new();
    seg_body.extend_from_slice(&info);
    seg_body.extend_from_slice(&tracks);
    for c_idx in 0..4u64 {
        let cluster_ts = c_idx * 100;
        let cluster_body = [
            ebml_uint(0xE7, cluster_ts),
            ebml_simple_block(1, 0, true, H264_16X16_IDR_SAMPLE),
            ebml_simple_block(2, 0, true, &pcm_chunk),
            ebml_simple_block(1, 50, true, H264_16X16_IDR_SAMPLE),
        ]
        .concat();
        seg_body.extend_from_slice(&ebml_el(0x1F43_B675, &cluster_body));
    }

    let segment = ebml_el(0x1853_8067, &seg_body);
    [ebml_header, segment].concat()
}

fn riff_chunk(id: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + payload.len() + (payload.len() & 1));
    out.extend_from_slice(id);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    if payload.len() & 1 != 0 {
        out.push(0);
    }
    out
}

fn riff_list(list_type: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(4 + payload.len());
    body.extend_from_slice(list_type);
    body.extend_from_slice(payload);
    riff_chunk(b"LIST", &body)
}

fn build_mjpeg_pcm_avi() -> Vec<u8> {
    // 8 MJPEG frames @ 20 fps (400 ms) + 4 PCM S16LE chunks (100 ms each = 400 ms).
    let jpeg = build_synthetic_jpeg_8x8();

    let mut avih = [0u8; 56];
    avih[0..4].copy_from_slice(&50_000u32.to_le_bytes()); // 20 fps
    avih[12..16].copy_from_slice(&0x10u32.to_le_bytes()); // AVIF_HASINDEX
    avih[16..20].copy_from_slice(&8u32.to_le_bytes()); // total frames
    avih[24..28].copy_from_slice(&2u32.to_le_bytes()); // streams
    avih[32..36].copy_from_slice(&8u32.to_le_bytes()); // width
    avih[36..40].copy_from_slice(&8u32.to_le_bytes()); // height
    let avih_chunk = riff_chunk(b"avih", &avih);

    // Stream 0: vids / MJPG
    let mut v_strh = [0u8; 56];
    v_strh[0..4].copy_from_slice(b"vids");
    v_strh[4..8].copy_from_slice(b"MJPG");
    v_strh[20..24].copy_from_slice(&1u32.to_le_bytes()); // dwScale
    v_strh[24..28].copy_from_slice(&20u32.to_le_bytes()); // dwRate (20 fps)
    v_strh[32..36].copy_from_slice(&8u32.to_le_bytes()); // dwLength
    let mut v_strf = [0u8; 40];
    v_strf[0..4].copy_from_slice(&40u32.to_le_bytes());
    v_strf[4..8].copy_from_slice(&8i32.to_le_bytes());
    v_strf[8..12].copy_from_slice(&8i32.to_le_bytes());
    v_strf[12..14].copy_from_slice(&1u16.to_le_bytes());
    v_strf[14..16].copy_from_slice(&24u16.to_le_bytes());
    v_strf[16..20].copy_from_slice(b"MJPG");
    let v_strl = riff_list(
        b"strl",
        &[riff_chunk(b"strh", &v_strh), riff_chunk(b"strf", &v_strf)].concat(),
    );

    // Stream 1: auds / 48 kHz mono S16LE
    let total_audio_samples: u32 = 4 * 4800;
    let mut a_strh = [0u8; 56];
    a_strh[0..4].copy_from_slice(b"auds");
    a_strh[20..24].copy_from_slice(&1u32.to_le_bytes()); // dwScale
    a_strh[24..28].copy_from_slice(&48_000u32.to_le_bytes()); // dwRate
    a_strh[32..36].copy_from_slice(&total_audio_samples.to_le_bytes()); // dwLength
    a_strh[44..48].copy_from_slice(&2u32.to_le_bytes()); // dwSampleSize = 2 bytes
    let mut a_strf = [0u8; 18];
    a_strf[0..2].copy_from_slice(&1u16.to_le_bytes()); // PCM
    a_strf[2..4].copy_from_slice(&1u16.to_le_bytes()); // mono
    a_strf[4..8].copy_from_slice(&48_000u32.to_le_bytes());
    a_strf[8..12].copy_from_slice(&96_000u32.to_le_bytes());
    a_strf[12..14].copy_from_slice(&2u16.to_le_bytes()); // blockAlign
    a_strf[14..16].copy_from_slice(&16u16.to_le_bytes()); // bitsPerSample
    let a_strl = riff_list(
        b"strl",
        &[riff_chunk(b"strh", &a_strh), riff_chunk(b"strf", &a_strf)].concat(),
    );

    let hdrl = riff_list(b"hdrl", &[avih_chunk, v_strl, a_strl].concat());

    let mut pcm_chunk = Vec::with_capacity(4800 * 2);
    for i in 0..4800u32 {
        let s: i16 = if (i / 48) % 2 == 0 { 4000 } else { -4000 };
        pcm_chunk.extend_from_slice(&s.to_le_bytes());
    }

    let mut movi_body = Vec::new();
    let mut idx1_body = Vec::new();
    let mut push_entry = |ckid: &[u8; 4], payload: &[u8]| {
        // Offset relative to `"movi"` FourCC (= 4 + movi_body.len())
        let rel_offset = (4 + movi_body.len()) as u32;
        movi_body.extend_from_slice(&riff_chunk(ckid, payload));
        idx1_body.extend_from_slice(ckid);
        idx1_body.extend_from_slice(&0x10u32.to_le_bytes()); // AVIIF_KEYFRAME
        idx1_body.extend_from_slice(&rel_offset.to_le_bytes());
        idx1_body.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    };

    for _ in 0..4u32 {
        push_entry(b"00dc", &jpeg);
        push_entry(b"01wb", &pcm_chunk);
        push_entry(b"00dc", &jpeg);
    }

    let movi = riff_list(b"movi", &movi_body);
    let idx1 = riff_chunk(b"idx1", &idx1_body);

    let mut avi_payload = Vec::new();
    avi_payload.extend_from_slice(b"AVI ");
    avi_payload.extend_from_slice(&hdrl);
    avi_payload.extend_from_slice(&movi);
    avi_payload.extend_from_slice(&idx1);
    riff_chunk(b"RIFF", &avi_payload)
}

#[test]
fn native_mkv_plays_pure_rust_demux_and_decoders() {
    let path = unique_temp_path("native_mkv_e2e", "mkv");
    std::fs::write(&path, build_h264_pcm_mkv()).expect("write mkv fixture");

    let mut player = Player::builder()
        .input(&path)
        .prefer_native_pipeline(true)
        .autoplay(false)
        .volume(0.0)
        .build()
        .expect("build mkv player");

    let mut info = None;
    let paused = watch(&mut player, Duration::from_secs(3), |ev| {
        match ev {
            PlayerEvent::DecoderInfo {
                demuxer,
                video_decoder,
                audio_decoder,
                ..
            } => info = Some((demuxer, video_decoder, audio_decoder)),
            PlayerEvent::Snapshot(s) if s.status == "paused" && !s.playing => {
                return info.is_some();
            }
            _ => {}
        }
        false
    });
    assert!(paused, "expected paused snapshot and DecoderInfo for MKV");
    let (demuxer, video_decoder, audio_decoder) = info.expect("DecoderInfo for MKV");
    assert!(
        demuxer == "rsmpeg-format (mkv)" || demuxer == "rsmpeg-format (matroska)",
        "unexpected MKV demuxer name: {demuxer}"
    );
    assert_eq!(video_decoder.as_deref(), Some("OpenH264 (H.264)"));
    assert_eq!(audio_decoder.as_deref(), Some("rsmpeg-rust (PCM)"));

    let target = Duration::from_millis(200);
    player.seek(target).expect("seek mkv");
    let seek_gen = player.generation();

    let mut seek_ok = false;
    let mut preview_ok = false;
    assert!(
        watch(&mut player, Duration::from_secs(3), |ev| {
            match ev {
                PlayerEvent::SeekCompleted {
                    position,
                    generation,
                } if generation == seek_gen => seek_ok = position == target,
                PlayerEvent::VideoFrame {
                    width,
                    height,
                    generation,
                    ..
                } if generation == seek_gen => preview_ok = width == 16 && height == 16,
                _ => {}
            }
            seek_ok && preview_ok
        }),
        "expected SeekCompleted and 16x16 preview frame for MKV seek"
    );

    player.set_playback_rate(2.0).expect("set rate");
    player.play().expect("resume mkv");
    let mut saw_level = false;
    let mut saw_ended = false;
    assert!(
        watch(&mut player, Duration::from_secs(4), |ev| {
            match ev {
                PlayerEvent::AudioLevel { peak, .. } if peak > 0.0 => saw_level = true,
                PlayerEvent::Ended { .. } => saw_ended = true,
                _ => {}
            }
            saw_level && saw_ended
        }),
        "expected non-zero AudioLevel and Ended for MKV playback (level={saw_level}, ended={saw_ended})"
    );

    let _ = player.shutdown();
    let _ = std::fs::remove_file(&path);
}

#[test]
fn native_avi_plays_pure_rust_mjpeg_and_pcm() {
    let path = unique_temp_path("native_avi_e2e", "avi");
    std::fs::write(&path, build_mjpeg_pcm_avi()).expect("write avi fixture");

    let mut player = Player::builder()
        .input(&path)
        .prefer_native_pipeline(true)
        .autoplay(false)
        .volume(0.0)
        .build()
        .expect("build avi player");

    let mut info = None;
    let paused = watch(&mut player, Duration::from_secs(3), |ev| {
        match ev {
            PlayerEvent::DecoderInfo {
                demuxer,
                video_decoder,
                audio_decoder,
                ..
            } => info = Some((demuxer, video_decoder, audio_decoder)),
            PlayerEvent::Snapshot(s) if s.status == "paused" && !s.playing => {
                return info.is_some();
            }
            _ => {}
        }
        false
    });
    assert!(paused, "expected paused snapshot and DecoderInfo for AVI");
    assert_eq!(
        info,
        Some((
            "rsmpeg-format (avi)".to_string(),
            Some("rsmpeg-rust (MJPEG)".to_string()),
            Some("rsmpeg-rust (PCM)".to_string()),
        ))
    );

    let target = Duration::from_millis(200);
    player.seek(target).expect("seek avi");
    let seek_gen = player.generation();

    let mut seek_ok = false;
    let mut preview_ok = false;
    assert!(
        watch(&mut player, Duration::from_secs(3), |ev| {
            match ev {
                PlayerEvent::SeekCompleted {
                    position,
                    generation,
                } if generation == seek_gen => seek_ok = position == target,
                PlayerEvent::VideoFrame {
                    width,
                    height,
                    rgba,
                    generation,
                    ..
                } if generation == seek_gen => {
                    preview_ok = width == 8 && height == 8 && rgba.len() == 8 * 8 * 4;
                }
                _ => {}
            }
            seek_ok && preview_ok
        }),
        "expected SeekCompleted and 8x8 MJPEG preview frame for AVI seek"
    );

    player.set_playback_rate(2.0).expect("set rate");
    player.play().expect("resume avi");
    let mut saw_level = false;
    let mut saw_ended = false;
    assert!(
        watch(&mut player, Duration::from_secs(4), |ev| {
            match ev {
                PlayerEvent::AudioLevel { peak, .. } if peak > 0.0 => saw_level = true,
                PlayerEvent::Ended { .. } => saw_ended = true,
                _ => {}
            }
            saw_level && saw_ended
        }),
        "expected non-zero AudioLevel and Ended for AVI playback (level={saw_level}, ended={saw_ended})"
    );

    let _ = player.shutdown();
    let _ = std::fs::remove_file(&path);
}
