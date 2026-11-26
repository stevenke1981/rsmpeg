//! H.264 bitstream format helpers (AVCC vs Annex B).
//!
//! MP4 stores length-prefixed NAL units (AVCC).  MPEG-TS / raw Elementary
//! Streams use start-code prefixed Annex B.  Converting Annex B a second time
//! corrupts the stream; converting AVCC with the wrong NAL length size fails
//! silently.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// How H.264 access units are framed in the container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum H264BitstreamFormat {
    /// Length-prefixed NAL units (ISO BMFF / MP4 `avcC`).
    Avcc { nal_length_size: usize },
    /// Start-code prefixed NAL units (`0x000001` / `0x00000001`).
    AnnexB,
}

impl H264BitstreamFormat {
    pub fn avcc(nal_length_size: usize) -> Option<Self> {
        matches!(nal_length_size, 1 | 2 | 4).then_some(Self::Avcc { nal_length_size })
    }
}

/// Errors while converting or parsing H.264 bitstream envelopes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum H264BitstreamError {
    TruncatedAvcc,
    InvalidNalLengthSize(usize),
    TruncatedPacket {
        pos: usize,
        need: usize,
        have: usize,
    },
    ZeroNalSize {
        pos: usize,
    },
    EmptyResult,
}

impl std::fmt::Display for H264BitstreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TruncatedAvcc => write!(f, "truncated avcC extradata"),
            Self::InvalidNalLengthSize(n) => write!(f, "invalid NAL length size {n}"),
            Self::TruncatedPacket { pos, need, have } => write!(
                f,
                "truncated AVCC packet at {pos}: need {need} bytes, have {have}"
            ),
            Self::ZeroNalSize { pos } => write!(f, "zero-size NAL unit at offset {pos}"),
            Self::EmptyResult => write!(f, "conversion produced empty Annex B buffer"),
        }
    }
}

impl std::error::Error for H264BitstreamError {}

/// Detect Annex B start codes at the head of a packet.
pub fn is_annex_b(packet: &[u8]) -> bool {
    if packet.len() >= 4 && packet[..4] == [0, 0, 0, 1] {
        return true;
    }
    if packet.len() >= 3 && packet[..3] == [0, 0, 1] {
        return true;
    }
    false
}

/// Read NAL length size from avcC (`lengthSizeMinusOne + 1`).
pub fn avcc_nal_length_size(extra_data: &[u8]) -> Result<usize, H264BitstreamError> {
    let length_size = (extra_data
        .get(4)
        .copied()
        .ok_or(H264BitstreamError::TruncatedAvcc)?
        & 0x03) as usize
        + 1;
    if length_size == 3 || !(1..=4).contains(&length_size) {
        return Err(H264BitstreamError::InvalidNalLengthSize(length_size));
    }
    Ok(length_size)
}

/// Convert avcC extradata SPS/PPS to Annex B.
pub fn avcc_extradata_to_annex_b(extra_data: &[u8]) -> Result<Vec<u8>, H264BitstreamError> {
    if extra_data.len() < 7 {
        return Err(H264BitstreamError::TruncatedAvcc);
    }

    let mut out = Vec::new();
    let num_sps = (extra_data[5] & 0x1f) as usize;
    let mut pos = 6;

    for _ in 0..num_sps {
        if pos + 2 > extra_data.len() {
            return Err(H264BitstreamError::TruncatedAvcc);
        }
        let sps_len = u16::from_be_bytes([extra_data[pos], extra_data[pos + 1]]) as usize;
        pos += 2;
        if pos + sps_len > extra_data.len() {
            return Err(H264BitstreamError::TruncatedAvcc);
        }
        out.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        out.extend_from_slice(&extra_data[pos..pos + sps_len]);
        pos += sps_len;
    }

    if pos >= extra_data.len() {
        return Err(H264BitstreamError::TruncatedAvcc);
    }
    let num_pps = extra_data[pos] as usize;
    pos += 1;

    for _ in 0..num_pps {
        if pos + 2 > extra_data.len() {
            return Err(H264BitstreamError::TruncatedAvcc);
        }
        let pps_len = u16::from_be_bytes([extra_data[pos], extra_data[pos + 1]]) as usize;
        pos += 2;
        if pos + pps_len > extra_data.len() {
            return Err(H264BitstreamError::TruncatedAvcc);
        }
        out.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        out.extend_from_slice(&extra_data[pos..pos + pps_len]);
        pos += pps_len;
    }

    if out.is_empty() {
        return Err(H264BitstreamError::EmptyResult);
    }
    Ok(out)
}

/// Convert one AVCC sample to Annex B.  Returns an error on truncated NAL sizes
/// instead of silently returning an empty / partial buffer.
pub fn avcc_packet_to_annex_b(
    packet: &[u8],
    nal_length_size: usize,
    prefix: Option<&[u8]>,
) -> Result<Vec<u8>, H264BitstreamError> {
    if !(1..=4).contains(&nal_length_size) || nal_length_size == 3 {
        return Err(H264BitstreamError::InvalidNalLengthSize(nal_length_size));
    }

    // Already Annex B — do not re-wrap.
    if is_annex_b(packet) {
        let mut out = Vec::with_capacity(packet.len() + prefix.map_or(0, <[u8]>::len));
        if let Some(p) = prefix {
            out.extend_from_slice(p);
        }
        out.extend_from_slice(packet);
        return Ok(out);
    }

    let extra = prefix.map_or(0, <[u8]>::len);
    let mut out = Vec::with_capacity(packet.len() + extra + 32);
    if let Some(prefix) = prefix {
        out.extend_from_slice(prefix);
    }

    let mut pos = 0;
    let mut nal_count = 0usize;
    while pos + nal_length_size <= packet.len() {
        let mut nal_size = 0usize;
        for &byte in &packet[pos..pos + nal_length_size] {
            nal_size = (nal_size << 8) | byte as usize;
        }
        let size_pos = pos;
        pos += nal_length_size;

        if nal_size == 0 {
            return Err(H264BitstreamError::ZeroNalSize { pos: size_pos });
        }
        if pos + nal_size > packet.len() {
            return Err(H264BitstreamError::TruncatedPacket {
                pos: size_pos,
                need: nal_size,
                have: packet.len().saturating_sub(pos),
            });
        }

        out.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        out.extend_from_slice(&packet[pos..pos + nal_size]);
        pos += nal_size;
        nal_count += 1;
    }

    if nal_count == 0 && prefix.is_none() {
        return Err(H264BitstreamError::EmptyResult);
    }
    Ok(out)
}

/// Prepare a packet for OpenH264 according to detected format.
pub fn packet_for_decoder(
    packet: &[u8],
    format: H264BitstreamFormat,
    sps_pps_prefix: Option<&[u8]>,
    sps_pps_sent: bool,
) -> Result<Vec<u8>, H264BitstreamError> {
    let prefix = if !sps_pps_sent { sps_pps_prefix } else { None };
    match format {
        H264BitstreamFormat::AnnexB => {
            let mut out = Vec::with_capacity(packet.len() + prefix.map_or(0, <[u8]>::len));
            if let Some(p) = prefix {
                out.extend_from_slice(p);
            }
            out.extend_from_slice(packet);
            Ok(out)
        }
        H264BitstreamFormat::Avcc { nal_length_size } => {
            avcc_packet_to_annex_b(packet, nal_length_size, prefix)
        }
    }
}

// ---------------------------------------------------------------------------
// Streaming avcC extraction (no whole-file read)
// ---------------------------------------------------------------------------

/// Walk ISOBMFF box headers with seek, extract `avcC` payload from `moov`.
///
/// Never loads the entire file into memory — only box headers and the avcC
/// payload itself are read.  Suitable for multi-GB MP4 files.
pub fn extract_avcc_streaming(path: impl AsRef<Path>) -> Option<Vec<u8>> {
    let mut file = File::open(path).ok()?;
    let file_len = file.seek(SeekFrom::End(0)).ok()?;
    file.seek(SeekFrom::Start(0)).ok()?;

    // Find moov at top level
    let mut pos = 0u64;
    while pos + 8 <= file_len {
        file.seek(SeekFrom::Start(pos)).ok()?;
        let mut hdr = [0u8; 8];
        file.read_exact(&mut hdr).ok()?;
        let mut size = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as u64;
        let btype = &hdr[4..8];

        let header_size = if size == 1 {
            // extended 64-bit size
            let mut ext = [0u8; 8];
            file.read_exact(&mut ext).ok()?;
            size = u64::from_be_bytes(ext);
            16u64
        } else if size == 0 {
            size = file_len - pos;
            8u64
        } else {
            8u64
        };

        if size < header_size {
            break;
        }

        if btype == b"moov" {
            let moov_end = pos + size;
            return scan_box_tree_for_avcc(&mut file, pos + header_size, moov_end);
        }

        pos = pos.saturating_add(size);
    }
    None
}

fn scan_box_tree_for_avcc(file: &mut File, mut pos: u64, end: u64) -> Option<Vec<u8>> {
    const MAX_DEPTH_BYTES: u64 = 64 * 1024 * 1024; // safety: don't scan >64MB of moov
    let scan_end = end.min(pos.saturating_add(MAX_DEPTH_BYTES));

    while pos + 8 <= scan_end {
        file.seek(SeekFrom::Start(pos)).ok()?;
        let mut hdr = [0u8; 8];
        file.read_exact(&mut hdr).ok()?;
        let mut size = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as u64;
        let btype = &hdr[4..8];

        let header_size = if size == 1 {
            let mut ext = [0u8; 8];
            file.read_exact(&mut ext).ok()?;
            size = u64::from_be_bytes(ext);
            16u64
        } else if size == 0 {
            size = scan_end - pos;
            8u64
        } else {
            8u64
        };

        if size < header_size || pos + size > end {
            break;
        }

        if btype == b"avcC" {
            let payload_len = (size - header_size) as usize;
            if payload_len == 0 || payload_len > 1024 * 1024 {
                return None;
            }
            let mut payload = vec![0u8; payload_len];
            file.read_exact(&mut payload).ok()?;
            return Some(payload);
        }

        // Containers that may nest avcC (stsd skips 8-byte FullBox + entry_count header;
        // avc1/avc3 skip 78-byte VisualSampleEntry fixed fields).
        let child_offset = match btype {
            b"stsd" => Some(8u64),
            b"avc1" | b"avc3" => Some(78u64),
            b"trak" | b"mdia" | b"minf" | b"stbl" | b"mp4v" => Some(0u64),
            _ => None,
        };
        if let Some(extra_hdr) = child_offset {
            if let Some(child_start) = pos
                .checked_add(header_size)
                .and_then(|p| p.checked_add(extra_hdr))
            {
                if child_start < pos + size {
                    if let Some(v) = scan_box_tree_for_avcc(file, child_start, pos + size) {
                        return Some(v);
                    }
                }
            }
        }

        pos = pos.saturating_add(size);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nal_length_sizes() {
        let mut avcc = [0u8; 5];
        avcc[4] = 0;
        assert_eq!(avcc_nal_length_size(&avcc).unwrap(), 1);
        avcc[4] = 1;
        assert_eq!(avcc_nal_length_size(&avcc).unwrap(), 2);
        avcc[4] = 3;
        assert_eq!(avcc_nal_length_size(&avcc).unwrap(), 4);
        avcc[4] = 2;
        assert!(avcc_nal_length_size(&avcc).is_err());
    }

    #[test]
    fn converts_1_2_4_byte_prefixes() {
        // 1-byte length
        let p1 = [3u8, 0x65, 0xaa, 0xbb];
        let out = avcc_packet_to_annex_b(&p1, 1, None).unwrap();
        assert_eq!(out, [0, 0, 0, 1, 0x65, 0xaa, 0xbb]);

        // 2-byte length
        let p2 = [0, 3, 0x65, 0xaa, 0xbb];
        let out = avcc_packet_to_annex_b(&p2, 2, None).unwrap();
        assert_eq!(out, [0, 0, 0, 1, 0x65, 0xaa, 0xbb]);

        // 4-byte length
        let p4 = [0, 0, 0, 2, 0x41, 0xcc];
        let out = avcc_packet_to_annex_b(&p4, 4, None).unwrap();
        assert_eq!(out, [0, 0, 0, 1, 0x41, 0xcc]);
    }

    #[test]
    fn already_annex_b_not_rewrapped() {
        let pkt = [0u8, 0, 0, 1, 0x65, 0x11];
        let out = avcc_packet_to_annex_b(&pkt, 4, None).unwrap();
        assert_eq!(out, pkt);
        assert!(is_annex_b(&pkt));
    }

    #[test]
    fn truncated_avcc_packet_errors() {
        let pkt = [0, 0, 0, 10, 0x65]; // claims 10 bytes, only 1 present
        let err = avcc_packet_to_annex_b(&pkt, 4, None).unwrap_err();
        assert!(matches!(err, H264BitstreamError::TruncatedPacket { .. }));
    }

    #[test]
    fn zero_nal_size_errors() {
        let pkt = [0, 0, 0, 0];
        let err = avcc_packet_to_annex_b(&pkt, 4, None).unwrap_err();
        assert!(matches!(err, H264BitstreamError::ZeroNalSize { .. }));
    }

    #[test]
    fn streaming_extract_finds_avcc_not_mdat_decoy() {
        fn make_box(box_type: &[u8; 4], payload: &[u8]) -> Vec<u8> {
            let size = 8u32 + payload.len() as u32;
            let mut v = Vec::with_capacity(size as usize);
            v.extend_from_slice(&size.to_be_bytes());
            v.extend_from_slice(box_type);
            v.extend_from_slice(payload);
            v
        }

        let ftyp = make_box(b"ftyp", &[0u8; 12]);
        let mut mdat_payload = vec![0u8; 24];
        mdat_payload[10..14].copy_from_slice(b"avcC");
        let mdat = make_box(b"mdat", &mdat_payload);

        let avcc_payload: Vec<u8> = vec![
            0x01, 0x42, 0x00, 0x1e, 0xff, 0xe1, 0x00, 0x0a, 0x67, 0x42, 0x00, 0x1e, 0x8d, 0x00,
            0x00, 0x03, 0x00, 0x01, 0x01, 0x00, 0x04, 0x68, 0xce, 0x06, 0xe2,
        ];
        // nest: moov / trak / mdia / minf / stbl / stsd-like / avc1 / avcC
        // Simplified: moov directly contains avcC for unit test of walker
        let avcc_box = make_box(b"avcC", &avcc_payload);
        let moov = make_box(b"moov", &avcc_box);
        let mp4 = [ftyp, mdat, moov].concat();

        let tmp = std::env::temp_dir().join("test_avcc_stream.mp4");
        std::fs::write(&tmp, &mp4).unwrap();
        let extracted = extract_avcc_streaming(&tmp);
        std::fs::remove_file(&tmp).ok();

        assert_eq!(extracted.as_deref(), Some(avcc_payload.as_slice()));
    }

    #[test]
    fn invalid_nal_length_sizes_are_rejected() {
        // 0, 3 and 5 are all invalid prefix widths.
        for n in [0usize, 3, 5, 8] {
            let err = avcc_packet_to_annex_b(&[0, 0, 0, 1, 0x65], n, None).unwrap_err();
            assert!(
                matches!(err, H264BitstreamError::InvalidNalLengthSize(_)),
                "n={n} gave {err:?}"
            );
            assert!(H264BitstreamFormat::avcc(n).is_none(), "n={n} accepted");
        }
        for n in [1usize, 2, 4] {
            assert!(H264BitstreamFormat::avcc(n).is_some(), "n={n} rejected");
        }
    }

    #[test]
    fn avcc_truncated_extradata_errors() {
        // Fewer than 7 bytes cannot hold the fixed avcC header.
        for len in 0..7usize {
            let data = vec![0u8; len];
            assert!(
                matches!(
                    avcc_extradata_to_annex_b(&data),
                    Err(H264BitstreamError::TruncatedAvcc)
                ),
                "len={len} should be truncated"
            );
        }
    }

    #[test]
    fn avcc_extradata_to_annex_b_roundtrips_sps_and_pps() {
        // 1 SPS (3 bytes) + 1 PPS (2 bytes).
        let mut avcc = vec![0x01, 0x42, 0x00, 0x1e, 0xff, 0xe1];
        avcc.extend_from_slice(&3u16.to_be_bytes());
        avcc.extend_from_slice(&[0x67, 0x42, 0x00]);
        avcc.push(1); // num_pps
        avcc.extend_from_slice(&2u16.to_be_bytes());
        avcc.extend_from_slice(&[0x68, 0xce]);

        let out = avcc_extradata_to_annex_b(&avcc).unwrap();
        assert_eq!(
            out,
            vec![0, 0, 0, 1, 0x67, 0x42, 0x00, 0, 0, 0, 1, 0x68, 0xce]
        );
    }

    #[test]
    fn avcc_multi_nal_packet_converts_each_unit() {
        // Two NAL units in one AVCC sample.
        let pkt = [
            0, 0, 0, 2, 0x41, 0x9a, // 2-byte NAL
            0, 0, 0, 1, 0x65, // 1-byte NAL
        ];
        let out = avcc_packet_to_annex_b(&pkt, 4, None).unwrap();
        assert_eq!(out, vec![0, 0, 0, 1, 0x41, 0x9a, 0, 0, 0, 1, 0x65]);
    }

    #[test]
    fn prefix_is_only_prepended_once() {
        let prefix = [0u8, 0, 0, 1, 0x67];
        let pkt = [0, 0, 0, 1, 0x65];

        // sps_pps_sent = false → prefix added.
        let first = packet_for_decoder(
            &pkt,
            H264BitstreamFormat::Avcc { nal_length_size: 4 },
            Some(&prefix),
            false,
        )
        .unwrap();
        assert_eq!(first, vec![0, 0, 0, 1, 0x67, 0, 0, 0, 1, 0x65]);

        // sps_pps_sent = true → prefix omitted.
        let later = packet_for_decoder(
            &pkt,
            H264BitstreamFormat::Avcc { nal_length_size: 4 },
            Some(&prefix),
            true,
        )
        .unwrap();
        assert_eq!(later, vec![0, 0, 0, 1, 0x65]);
    }

    #[test]
    fn annex_b_format_passes_through_with_optional_prefix() {
        let pkt = [0u8, 0, 0, 1, 0x65];
        let out = packet_for_decoder(&pkt, H264BitstreamFormat::AnnexB, None, false).unwrap();
        assert_eq!(out, pkt);
    }

    #[test]
    fn is_annex_b_recognises_both_start_code_lengths() {
        assert!(is_annex_b(&[0, 0, 0, 1]));
        assert!(is_annex_b(&[0, 0, 1]));
        assert!(!is_annex_b(&[0, 0]));
        assert!(!is_annex_b(&[]));
        assert!(!is_annex_b(&[0, 0, 0, 2]));
    }

    #[test]
    fn empty_packet_without_prefix_errors() {
        assert!(matches!(
            avcc_packet_to_annex_b(&[], 4, None),
            Err(H264BitstreamError::EmptyResult)
        ));
        // With a prefix the output is the prefix alone, which is valid.
        assert_eq!(
            avcc_packet_to_annex_b(&[], 4, Some(&[1, 2])).unwrap(),
            vec![1, 2]
        );
    }

    #[test]
    fn streaming_extract_returns_none_for_missing_file() {
        assert!(extract_avcc_streaming("definitely-missing-file-xyz.mp4").is_none());
    }

    #[test]
    fn streaming_extract_returns_none_without_moov() {
        let mut v = Vec::new();
        let payload = [0u8; 4];
        v.extend_from_slice(&(8u32 + payload.len() as u32).to_be_bytes());
        v.extend_from_slice(b"mdat");
        v.extend_from_slice(&payload);

        let tmp = std::env::temp_dir().join("test_avcc_no_moov.mp4");
        std::fs::write(&tmp, &v).unwrap();
        let got = extract_avcc_streaming(&tmp);
        std::fs::remove_file(&tmp).ok();
        assert!(got.is_none());
    }

    #[test]
    fn streaming_extract_finds_avcc_in_nested_stsd_avc1_hierarchy() {
        fn make_box(box_type: &[u8; 4], payload: &[u8]) -> Vec<u8> {
            let size = 8u32 + payload.len() as u32;
            let mut v = Vec::with_capacity(size as usize);
            v.extend_from_slice(&size.to_be_bytes());
            v.extend_from_slice(box_type);
            v.extend_from_slice(payload);
            v
        }

        let ftyp = make_box(b"ftyp", &[0u8; 12]);
        let mut mdat_payload = vec![0u8; 24];
        mdat_payload[10..14].copy_from_slice(b"avcC");
        let mdat = make_box(b"mdat", &mdat_payload);

        // Preceding audio track (`stsd` + `mp4a`) must be skipped cleanly before reaching video `trak`.
        let mp4a = make_box(b"mp4a", &[0u8; 28]);
        let mut audio_stsd_body = vec![0u8; 8];
        audio_stsd_body[7] = 1;
        audio_stsd_body.extend_from_slice(&mp4a);
        let audio_stsd = make_box(b"stsd", &audio_stsd_body);
        let audio_trak = make_box(
            b"trak",
            &make_box(b"mdia", &make_box(b"minf", &make_box(b"stbl", &audio_stsd))),
        );

        let avcc_payload: Vec<u8> = vec![
            0x01, 0x42, 0x00, 0x1e, 0xff, 0xe1, 0x00, 0x0a, 0x67, 0x42, 0x00, 0x1e, 0x8d, 0x00,
            0x00, 0x03, 0x00, 0x01, 0x01, 0x00, 0x04, 0x68, 0xce, 0x06, 0xe2,
        ];
        let pasp_box = make_box(b"pasp", &[0, 0, 0, 1, 0, 0, 0, 1]);
        let avcc_box = make_box(b"avcC", &avcc_payload);

        // VisualSampleEntry fixed header is 78 bytes before child boxes (`pasp`, `avcC`).
        let mut avc1_body = vec![0u8; 78];
        avc1_body[6..8].copy_from_slice(&1u16.to_be_bytes());
        avc1_body.extend_from_slice(&pasp_box);
        avc1_body.extend_from_slice(&avcc_box);
        let avc1 = make_box(b"avc1", &avc1_body);

        // stsd FullBox header is 8 bytes (version/flags + entry_count) before sample entries.
        let mut stsd_body = vec![0u8; 8];
        stsd_body[7] = 1;
        stsd_body.extend_from_slice(&avc1);
        let stsd = make_box(b"stsd", &stsd_body);

        let video_trak = make_box(
            b"trak",
            &make_box(b"mdia", &make_box(b"minf", &make_box(b"stbl", &stsd))),
        );
        let moov = make_box(b"moov", &[audio_trak, video_trak].concat());
        let mp4 = [ftyp, mdat, moov].concat();

        let tmp = std::env::temp_dir().join("test_avcc_nested_stsd_avc1.mp4");
        std::fs::write(&tmp, &mp4).unwrap();
        let extracted = extract_avcc_streaming(&tmp);
        std::fs::remove_file(&tmp).ok();

        assert_eq!(extracted.as_deref(), Some(avcc_payload.as_slice()));
    }
}
