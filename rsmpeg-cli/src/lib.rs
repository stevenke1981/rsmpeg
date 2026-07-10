#![forbid(unsafe_code)]

/// Parse an AVCC (AVC Decoder Configuration Record) extradata blob and
/// convert the contained SPS/PPS NAL units to Annex B format (0x00000001
/// start-code prefixed).
///
/// This is needed because MP4 stores H.264 parameter sets in the avcC box
/// (extradata), *not* in the video packet data.  OpenH264 needs to receive
/// SPS/PPS before it can decode slice NAL units.
///
/// # Format (ISO 14496-15:2014 §5.2.4.1.1)
///
/// ```text
/// Offset  Size  Field
/// 0       1     configurationVersion (0x01)
/// 1       1     AVCProfileIndication
/// 2       1     profile_compatibility
/// 3       1     AVCLevelIndication
/// 4       1     reserved(6) | lengthSizeMinusOne(2)
/// 5       1     reserved(3) | numOfSequenceParameterSets(5)
/// 6+      var   SPS entries (2-byte length + data)
///         var   numOfPictureParameterSets (1 byte)
///         var   PPS entries (2-byte length + data)
/// ```
pub fn avcc_extradata_to_annex_b(extra_data: &[u8]) -> Vec<u8> {
    if extra_data.len() < 7 {
        return Vec::new();
    }

    let mut out = Vec::new();
    let num_sps = (extra_data[5] & 0x1f) as usize;
    let mut pos = 6;

    for _ in 0..num_sps {
        if pos + 2 > extra_data.len() {
            break;
        }
        let sps_len = u16::from_be_bytes([extra_data[pos], extra_data[pos + 1]]) as usize;
        pos += 2;
        if pos + sps_len > extra_data.len() {
            break;
        }
        out.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        out.extend_from_slice(&extra_data[pos..pos + sps_len]);
        pos += sps_len;
    }

    if pos + 1 > extra_data.len() {
        return out;
    }
    let num_pps = extra_data[pos] as usize;
    pos += 1;

    for _ in 0..num_pps {
        if pos + 2 > extra_data.len() {
            break;
        }
        let pps_len = u16::from_be_bytes([extra_data[pos], extra_data[pos + 1]]) as usize;
        pos += 2;
        if pos + pps_len > extra_data.len() {
            break;
        }
        out.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        out.extend_from_slice(&extra_data[pos..pos + pps_len]);
        pos += pps_len;
    }

    out
}

/// Return the byte width used by this avcC stream for NAL length prefixes.
///
/// MP4/H.264 samples usually use four bytes, but the value is stored in avcC
/// and can legally be 1, 2, or 4 bytes. Returning `None` means the avcC blob
/// is too short or declares the reserved three-byte length size.
pub fn avcc_nal_length_size(extra_data: &[u8]) -> Option<usize> {
    let length_size = (extra_data.get(4)? & 0x03) as usize + 1;
    (length_size != 3).then_some(length_size)
}

/// Convert one AVCC packet/sample to Annex B format.
///
/// `prefix` is normally the SPS/PPS Annex B blob from
/// [`avcc_extradata_to_annex_b`] and is prepended only by callers for the first
/// packet of a stream.
pub fn avcc_packet_to_annex_b(
    packet: &[u8],
    nal_length_size: usize,
    prefix: Option<&[u8]>,
) -> Vec<u8> {
    if !(1..=4).contains(&nal_length_size) || nal_length_size == 3 {
        return Vec::new();
    }

    let extra = prefix.map_or(0, <[u8]>::len);
    let mut out = Vec::with_capacity(packet.len() + extra + 32);
    if let Some(prefix) = prefix {
        out.extend_from_slice(prefix);
    }

    let mut pos = 0;
    while pos + nal_length_size <= packet.len() {
        let mut nal_size = 0usize;
        for &byte in &packet[pos..pos + nal_length_size] {
            nal_size = (nal_size << 8) | byte as usize;
        }
        pos += nal_length_size;

        if nal_size == 0 || pos + nal_size > packet.len() {
            break;
        }

        out.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        out.extend_from_slice(&packet[pos..pos + nal_size]);
        pos += nal_size;
    }

    out
}

/// Walk top-level ISOBMFF box boundaries to locate the `moov` box.
///
/// Returns `(box_start, box_end)` where `box_start` points at the 4-byte size
/// field and `box_end` is the first byte past the box (i.e. exclusive upper
/// bound).  Searching within these bounds avoids false‑positive fourCC matches
/// from `mdat` or other payload boxes.
///
/// Returns `None` when the file is truncated, has no `moov` (fragmented MP4),
/// or contains a box larger than 4 GiB (extended-size). The caller must fall
/// back to a whole‑file scan in that case.
fn find_moov_bounds(data: &[u8]) -> Option<(usize, usize)> {
    let mut pos = 0;
    while pos + 8 <= data.len() {
        let box_size =
            u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;

        if box_size == 0 {
            // size 0 = extends to end of file (only valid for the last box)
            if &data[pos + 4..pos + 8] == b"moov" {
                return Some((pos, data.len()));
            }
            break; // cannot skip, stop scanning
        }
        if box_size < 8 {
            break; // invalid or extended-size (>4 GiB) – fall back
        }
        if pos + box_size > data.len() {
            break; // truncated file
        }
        if &data[pos + 4..pos + 8] == b"moov" {
            return Some((pos, pos + box_size));
        }
        pos += box_size;
    }
    None
}

/// Extract the raw avcC (AVC Decoder Configuration Record) from an MP4 file.
///
/// The search is restricted to the `moov` box when present, which avoids
/// false‑positive matches from binary media data in `mdat` or other payload
/// boxes. Falls back to a whole-file scan only when the file has no `moov`
/// box.
///
/// Returns the payload following the 8‑byte `avcC` box header, or `None` if
/// no valid avcC record is found.
pub fn extract_avcc_from_mp4(path: &str) -> Option<Vec<u8>> {
    let data = std::fs::read(path).ok()?;
    let target = b"avcC";

    // Restrict the byte scan to the moov payload when available to avoid
    // false-positive matches from mdat or other payload boxes.
    let (search_start, search_end) = find_moov_bounds(&data)
        .map(|(box_start, box_end)| (box_start + 8, box_end))
        .unwrap_or((8, data.len()));

    for i in search_start..search_end.saturating_sub(3) {
        if &data[i..i + 4] != target {
            continue;
        }

        let raw_size = u32::from_be_bytes([data[i - 4], data[i - 3], data[i - 2], data[i - 1]]);

        let remaining = data.len() - (i - 4);

        if raw_size == 0 {
            // size = 0 means "extends to end of file"
            return Some(data[i + 4..].to_vec());
        }

        if raw_size < 8 || raw_size as usize > remaining {
            continue; // not a valid box envelope
        }

        let payload_start = i + 4;
        let payload_end = (i - 4) + raw_size as usize;
        if payload_end > data.len() {
            continue;
        }
        return Some(data[payload_start..payload_end].to_vec());
    }
    None
}

#[cfg(test)]
mod avcc_packet_tests {
    use super::{avcc_nal_length_size, avcc_packet_to_annex_b};

    #[test]
    fn reads_avcc_nal_length_size() {
        let mut avcc = [0u8; 5];

        avcc[4] = 0;
        assert_eq!(avcc_nal_length_size(&avcc), Some(1));

        avcc[4] = 1;
        assert_eq!(avcc_nal_length_size(&avcc), Some(2));

        avcc[4] = 2;
        assert_eq!(avcc_nal_length_size(&avcc), None);

        avcc[4] = 3;
        assert_eq!(avcc_nal_length_size(&avcc), Some(4));
    }

    #[test]
    fn converts_length_prefixed_packet_to_annex_b() {
        let packet = [0, 3, 0x65, 0xaa, 0xbb, 0, 2, 0x41, 0xcc];
        let prefix = [0, 0, 0, 1, 0x67];

        assert_eq!(
            avcc_packet_to_annex_b(&packet, 2, Some(&prefix)),
            [0, 0, 0, 1, 0x67, 0, 0, 0, 1, 0x65, 0xaa, 0xbb, 0, 0, 0, 1, 0x41, 0xcc,]
        );
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build an ISOBMFF box (4‑byte big‑endian size + 4‑byte type +
    /// payload).
    fn make_box(box_type: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let size = 8u32 + payload.len() as u32;
        let mut v = Vec::with_capacity(size as usize);
        v.extend_from_slice(&size.to_be_bytes());
        v.extend_from_slice(box_type);
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn find_moov_bounds_found() {
        let payload = make_box(b"trak", &[0u8; 8]);
        let moov = make_box(b"moov", &payload);
        let ftyp = make_box(b"ftyp", &[0u8; 8]);
        let mp4 = [ftyp, moov].concat();

        let (start, end) = find_moov_bounds(&mp4).expect("moov found");
        assert_eq!(&mp4[start + 4..start + 8], b"moov");
        assert!(end > start);
        assert!(end <= mp4.len());
    }

    #[test]
    fn find_moov_bounds_not_found_truncated() {
        assert!(find_moov_bounds(&[0u8; 4]).is_none());
    }

    #[test]
    fn find_moov_bounds_skips_other_boxes() {
        let mdat = make_box(b"mdat", &[0u8; 32]);
        let free = make_box(b"free", &[0u8; 8]);
        let moov = make_box(b"moov", &[0u8; 4]);
        let mp4 = [mdat, free, moov.clone()].concat();

        let (start, end) = find_moov_bounds(&mp4).expect("moov found after mdat+free");
        assert_eq!(&mp4[start + 4..start + 8], b"moov");
        assert_eq!(&mp4[start..end], &moov[..]);
    }

    #[test]
    fn extract_avcc_skips_mdat_decoy() {
        // ── Build synthetic MP4 ──
        // ftyp        (12 bytes of filler)
        // mdat        containing decoy "avcC" bytes — *before* moov
        // moov        containing the real avcC box

        let ftyp = make_box(b"ftyp", &[0u8; 12]);

        // mdat with "avcC" embedded as a decoy at offset 10
        let mut mdat_payload = vec![0u8; 24];
        mdat_payload[10..14].copy_from_slice(b"avcC");
        let mdat = make_box(b"mdat", &mdat_payload);

        // Real avcC payload (valid‑looking avcC record)
        let avcc_payload: Vec<u8> = vec![
            0x01, // configurationVersion
            0x42, // AVCProfileIndication (High)
            0x00, // profile_compatibility
            0x1e, // AVCLevelIndication (30)
            0xff, // 0b111111 → lengthSizeMinusOne = 3 → 4 bytes
            0xe1, // 0b111 00001 → 1 SPS
            0x00, 0x0a, // SPS length = 10
            0x67, 0x42, 0x00, 0x1e, 0x8d, 0x00, 0x00, 0x03, 0x00, 0x01, // SPS data (10 bytes)
            0x01, // 1 PPS
            0x00, 0x04, // PPS length = 4
            0x68, 0xce, 0x06, 0xe2, // PPS data
        ];
        let avcc_box = make_box(b"avcC", &avcc_payload);
        let moov = make_box(b"moov", &avcc_box);

        let mp4 = [ftyp, mdat, moov].concat();

        // Write to temp file
        let tmp = std::env::temp_dir().join("test_avcc_decoy.mp4");
        std::fs::write(&tmp, &mp4).expect("write temp file");

        let result = extract_avcc_from_mp4(tmp.to_str().expect("valid temp path"));
        std::fs::remove_file(&tmp).ok();

        let extracted = result.expect("extract_avcc_from_mp4 should find avcC");

        // Must return the moov's avcC, not the mdat decoy
        assert_eq!(
            extracted, avcc_payload,
            "must return the real avcC from moov, not the decoy in mdat"
        );
    }
}
