/// Format detection confidence score.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProbeScore {
    NoMatch = 0,
    Possible = 25,
    Likely = 50,
    VeryLikely = 75,
    Certain = 100,
}

/// Result of format probing.
#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub format_name: &'static str,
    pub description: &'static str,
    pub score: ProbeScore,
    pub extension: &'static str,
}

/// Detect container format from initial bytes.
pub fn probe_format(buf: &[u8]) -> Vec<ProbeResult> {
    let mut results = Vec::new();

    // MP4/ISOBMFF: ftyp box
    if buf.len() >= 8 && &buf[4..8] == b"ftyp" {
        results.push(ProbeResult {
            format_name: "mp4",
            description: "MP4/ISOBMFF",
            score: ProbeScore::Certain,
            extension: "mp4",
        });
    }

    // MKV/WebM: EBML header
    if buf.len() >= 4 && buf[0..4] == [0x1A, 0x45, 0xDF, 0xA3] {
        let is_webm = buf.len() > 20 && buf[..20].windows(4).any(|w| w == b"webm");
        if is_webm {
            results.push(ProbeResult {
                format_name: "webm",
                description: "WebM",
                score: ProbeScore::Certain,
                extension: "webm",
            });
        } else {
            results.push(ProbeResult {
                format_name: "mkv",
                description: "Matroska",
                score: ProbeScore::Certain,
                extension: "mkv",
            });
        }
    }

    // AVI: RIFF header
    if buf.len() >= 12 && &buf[0..4] == b"RIFF" && &buf[8..12] == b"AVI " {
        results.push(ProbeResult {
            format_name: "avi",
            description: "AVI (Audio Video Interleave)",
            score: ProbeScore::Certain,
            extension: "avi",
        });
    }

    // MPEG-TS: sync byte 0x47
    if buf.len() >= 192 {
        let sync_count = buf[..192].iter().filter(|&&b| b == 0x47).count();
        if sync_count > 5 {
            results.push(ProbeResult {
                format_name: "mpegts",
                description: "MPEG-TS (Transport Stream)",
                score: ProbeScore::VeryLikely,
                extension: "ts",
            });
        }
    }

    // OGG: capture pattern
    if buf.len() >= 4 && &buf[0..4] == b"OggS" {
        results.push(ProbeResult {
            format_name: "ogg",
            description: "OGG",
            score: ProbeScore::Certain,
            extension: "ogg",
        });
    }

    // FLAC: fLaC marker
    if buf.len() >= 4 && &buf[0..4] == b"fLaC" {
        results.push(ProbeResult {
            format_name: "flac",
            description: "FLAC (Free Lossless Audio Codec)",
            score: ProbeScore::Certain,
            extension: "flac",
        });
    }

    // WAV: RIFF WAVE
    if buf.len() >= 12 && &buf[0..4] == b"RIFF" && &buf[8..12] == b"WAVE" {
        results.push(ProbeResult {
            format_name: "wav",
            description: "WAV (Waveform Audio)",
            score: ProbeScore::Certain,
            extension: "wav",
        });
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_probe_mp4() {
        let mut buf = vec![0u8; 16];
        buf[4..8].copy_from_slice(b"ftyp");
        buf[8..12].copy_from_slice(b"isom");
        let results = probe_format(&buf);
        assert!(results.iter().any(|r| r.format_name == "mp4"));
    }

    #[test]
    fn test_probe_mkv() {
        let buf = vec![0x1A, 0x45, 0xDF, 0xA3];
        let results = probe_format(&buf);
        assert!(results.iter().any(|r| r.format_name == "mkv"));
    }

    #[test]
    fn test_probe_avi() {
        let mut buf = vec![0u8; 12];
        buf[0..4].copy_from_slice(b"RIFF");
        buf[8..12].copy_from_slice(b"AVI ");
        let results = probe_format(&buf);
        assert!(results.iter().any(|r| r.format_name == "avi"));
    }

    #[test]
    fn test_probe_flac() {
        let buf = b"fLaC\x00\x00\x00\x22\x12\x00\x12\x00";
        let results = probe_format(buf);
        assert!(results.iter().any(|r| r.format_name == "flac"));
    }

    #[test]
    fn test_probe_unknown() {
        let buf = vec![0x00, 0x01, 0x02, 0x03];
        let results = probe_format(&buf);
        assert!(results.is_empty());
    }
}
