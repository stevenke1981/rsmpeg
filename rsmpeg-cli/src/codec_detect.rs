//! Explicit codec detection for Symphonia tracks.
//!
//! Symphonia does not decode video, so H.264 tracks often appear as
//! `CODEC_TYPE_NULL`.  We must **not** treat every unknown track as H.264 —
//! only streams whose container tags / extradata prove the codec, and never
//! feed unsupported codecs into OpenH264.

use symphonia::core::codecs::CODEC_TYPE_NULL;
use symphonia::core::formats::Track;

/// Video codecs we can name.  Only [`DetectedVideoCodec::H264`] may use OpenH264.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetectedVideoCodec {
    H264,
    Hevc,
    Vp9,
    Vp8,
    Av1,
    Mpeg2,
    Mpeg4,
    Mjpeg,
    /// Container says "video" but the codec is unknown or unsupported.
    Unsupported {
        tag: String,
    },
}

impl DetectedVideoCodec {
    pub fn name(&self) -> &str {
        match self {
            Self::H264 => "h264",
            Self::Hevc => "hevc",
            Self::Vp9 => "vp9",
            Self::Vp8 => "vp8",
            Self::Av1 => "av1",
            Self::Mpeg2 => "mpeg2",
            Self::Mpeg4 => "mpeg4",
            Self::Mjpeg => "mjpeg",
            Self::Unsupported { tag } => tag.as_str(),
        }
    }

    pub fn is_h264(&self) -> bool {
        matches!(self, Self::H264)
    }

    pub fn supported_by_openh264(&self) -> bool {
        self.is_h264()
    }
}

/// Map a 4-byte container codec tag (e.g. `avc1`, `hvc1`) to a video codec.
pub fn codec_from_fourcc(tag: &[u8]) -> Option<DetectedVideoCodec> {
    if tag.len() < 4 {
        return None;
    }
    let fourcc = [
        tag[0].to_ascii_lowercase(),
        tag[1].to_ascii_lowercase(),
        tag[2].to_ascii_lowercase(),
        tag[3].to_ascii_lowercase(),
    ];
    match &fourcc {
        b"avc1" | b"avc3" | b"avc2" | b"avc4" | b"h264" | b"x264" => Some(DetectedVideoCodec::H264),
        b"hvc1" | b"hev1" | b"hevc" | b"h265" => Some(DetectedVideoCodec::Hevc),
        b"vp09" | b"vp9 " | b"vp90" => Some(DetectedVideoCodec::Vp9),
        b"vp08" | b"vp8 " | b"vp80" => Some(DetectedVideoCodec::Vp8),
        b"av01" | b"av1 " => Some(DetectedVideoCodec::Av1),
        b"mp2v" | b"m2v1" | b"mpeg" => Some(DetectedVideoCodec::Mpeg2),
        b"mp4v" | b"xvid" | b"divx" => Some(DetectedVideoCodec::Mpeg4),
        b"jpeg" | b"mjpg" | b"mjpa" | b"mjpb" => Some(DetectedVideoCodec::Mjpeg),
        // Subtitle / data — not video
        b"tx3g" | b"stpp" | b"wvtt" | b"sbtt" | b"c608" | b"c708" | b"mp4s" => None,
        other => {
            let tag = String::from_utf8_lossy(other).into_owned();
            // Printable fourcc → unsupported video candidate
            if other.iter().all(|b| b.is_ascii_graphic()) {
                Some(DetectedVideoCodec::Unsupported { tag })
            } else {
                None
            }
        }
    }
}

/// True when extradata looks like an AVC Decoder Configuration Record (avcC).
pub fn looks_like_avcc(extra: &[u8]) -> bool {
    if extra.len() < 7 {
        return false;
    }
    // configurationVersion must be 1
    if extra[0] != 1 {
        return false;
    }
    let length_size = (extra[4] & 0x03) + 1;
    length_size == 1 || length_size == 2 || length_size == 4
}

/// True when extradata looks like an HEVC Decoder Configuration Record (hvcC).
pub fn looks_like_hvcc(extra: &[u8]) -> bool {
    // hvcC: configurationVersion == 1, longer header than avcC
    extra.len() >= 23 && extra[0] == 1
}

/// Classify a Symphonia track as audio, H.264 video, unsupported video, or ignore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackKind {
    Audio,
    Video(DetectedVideoCodec),
    /// Subtitle / attachment / data — never treat as video.
    Ignore,
}

/// Detect track kind without assuming `CODEC_TYPE_NULL` ⇒ H.264.
pub fn classify_track(track: &Track) -> TrackKind {
    let cp = &track.codec_params;

    // Definite audio
    if cp.sample_rate.is_some() && cp.codec != CODEC_TYPE_NULL {
        return TrackKind::Audio;
    }
    if cp.sample_rate.is_some() && cp.channels.is_some() {
        return TrackKind::Audio;
    }

    // Extradata-based video detection (most reliable for MP4/H.264)
    if let Some(extra) = cp.extra_data.as_ref() {
        if looks_like_avcc(extra) {
            return TrackKind::Video(DetectedVideoCodec::H264);
        }
        if looks_like_hvcc(extra) {
            return TrackKind::Video(DetectedVideoCodec::Hevc);
        }
    }

    // Null codec, no audio params — do NOT assume H.264 (could be HEVC/subtitle/data).
    // Without extradata we leave as Ignore so callers can fall back to a
    // container-level streaming avcC probe.
    if cp.codec == CODEC_TYPE_NULL && cp.sample_rate.is_none() {
        return TrackKind::Ignore;
    }

    TrackKind::Ignore
}

/// Find the first playable audio track.
pub fn find_audio_track(tracks: &[Track]) -> Option<&Track> {
    tracks
        .iter()
        .find(|t| matches!(classify_track(t), TrackKind::Audio))
}

/// Find the first H.264 video track (safe for OpenH264).
pub fn find_h264_video_track(tracks: &[Track]) -> Option<&Track> {
    tracks.iter().find(|t| {
        matches!(
            classify_track(t),
            TrackKind::Video(DetectedVideoCodec::H264)
        )
    })
}

/// Report the first video track that is present but not H.264 (for user messaging).
pub fn find_unsupported_video(tracks: &[Track]) -> Option<DetectedVideoCodec> {
    tracks.iter().find_map(|t| match classify_track(t) {
        TrackKind::Video(c) if !c.is_h264() => Some(c),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fourcc_maps_h264_variants() {
        assert_eq!(codec_from_fourcc(b"avc1"), Some(DetectedVideoCodec::H264));
        assert_eq!(codec_from_fourcc(b"AVC1"), Some(DetectedVideoCodec::H264));
        assert_eq!(codec_from_fourcc(b"hvc1"), Some(DetectedVideoCodec::Hevc));
        assert_eq!(codec_from_fourcc(b"av01"), Some(DetectedVideoCodec::Av1));
        assert_eq!(codec_from_fourcc(b"vp09"), Some(DetectedVideoCodec::Vp9));
    }

    #[test]
    fn fourcc_rejects_subtitle_tags() {
        assert_eq!(codec_from_fourcc(b"tx3g"), None);
        assert_eq!(codec_from_fourcc(b"wvtt"), None);
    }

    #[test]
    fn avcc_detection() {
        let mut avcc = vec![0u8; 7];
        avcc[0] = 1;
        avcc[4] = 0xff; // lengthSizeMinusOne = 3 → 4 bytes
        assert!(looks_like_avcc(&avcc));

        avcc[0] = 2;
        assert!(!looks_like_avcc(&avcc));
    }

    #[test]
    fn hevc_not_treated_as_h264() {
        assert!(!DetectedVideoCodec::Hevc.supported_by_openh264());
        assert!(DetectedVideoCodec::H264.supported_by_openh264());
    }
}
