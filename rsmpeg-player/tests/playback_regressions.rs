//! End-to-end playback worker regression tests (native & Symphonia fallback).

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use rsmpeg_player::{Player, PlayerEvent};

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
