#![forbid(unsafe_code)]

//! Shared helpers for the rsmpeg CLI binary (AVCC conversion, codec detection).

pub mod codec_detect;
pub mod h264_bitstream;

// Re-export primary H.264 helpers for callers and tests.
pub use h264_bitstream::{
    avcc_extradata_to_annex_b, avcc_nal_length_size, avcc_packet_to_annex_b,
    extract_avcc_streaming, is_annex_b, packet_for_decoder, H264BitstreamError,
    H264BitstreamFormat,
};

/// Backward-compatible name for streaming avcC extraction.
///
/// **Never** reads the whole file into memory.
pub fn extract_avcc_from_mp4(path: &str) -> Option<Vec<u8>> {
    extract_avcc_streaming(path)
}

/// Fallible → Vec wrappers used by older call sites that discard errors.
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
