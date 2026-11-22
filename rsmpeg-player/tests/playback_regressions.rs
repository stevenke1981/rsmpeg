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
    let ftyp = box_bytes(b"ftyp", b"isom\0\0\x02\0isomiso2avc1mp41");

    let sample_size = H264_16X16_IDR_SAMPLE.len() as u32;
    let mut mdat_payload = Vec::with_capacity((sample_size * frame_count) as usize);
    for _ in 0..frame_count {
        mdat_payload.extend_from_slice(H264_16X16_IDR_SAMPLE);
    }
    let mdat = box_bytes(b"mdat", &mdat_payload);

    let data_offset = (ftyp.len() + 8) as u32;
    let duration_ms = frame_count * frame_duration_ms;

    // mvhd
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

    // tkhd
    let mut tkhd_body = vec![0u8; 80];
    tkhd_body[8..12].copy_from_slice(&be32(1));
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
