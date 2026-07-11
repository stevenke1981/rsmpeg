#![forbid(unsafe_code)]

//! Shared helpers for the rsmpeg CLI — re-exports playback utilities from
//! [`rsmpeg_player`] so tests and legacy call sites keep working.

pub use rsmpeg_player::h264_bitstream::{
    avcc_extradata_to_annex_b, avcc_nal_length_size, avcc_packet_to_annex_b,
    extract_avcc_streaming, is_annex_b, packet_for_decoder, H264BitstreamError,
    H264BitstreamFormat,
};
pub use rsmpeg_player::{
    classify_track, codec_from_fourcc, find_audio_track, find_h264_video_track,
    find_unsupported_video, DetectedVideoCodec, TrackKind,
};

/// Backward-compatible name for streaming avcC extraction.
pub fn extract_avcc_from_mp4(path: &str) -> Option<Vec<u8>> {
    extract_avcc_streaming(path)
}

pub fn avcc_extradata_to_annex_b_lossy(extra_data: &[u8]) -> Vec<u8> {
    avcc_extradata_to_annex_b(extra_data).unwrap_or_default()
}

pub fn avcc_packet_to_annex_b_lossy(
    packet: &[u8],
    nal_length_size: usize,
    prefix: Option<&[u8]>,
) -> Vec<u8> {
    avcc_packet_to_annex_b(packet, nal_length_size, prefix).unwrap_or_default()
}

pub fn avcc_nal_length_size_opt(extra_data: &[u8]) -> Option<usize> {
    avcc_nal_length_size(extra_data).ok()
}
