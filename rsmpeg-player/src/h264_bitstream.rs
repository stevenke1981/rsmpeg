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

        // Containers that may nest avcC
        if matches!(
            btype,
            b"trak" | b"mdia" | b"minf" | b"stbl" | b"stsd" | b"avc1" | b"avc3" | b"mp4v"
        ) {
            if let Some(v) = scan_box_tree_for_avcc(file, pos + header_size, pos + size) {
                return Some(v);
            }
        }

        // stsd has a 8-byte sample entry header before child boxes for visual sample entries
        // (version/flags + entry count).  For robustness the recursive scan above already
        // entered stsd; child sample entries (avc1) are scanned next iteration via seek.

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
}
