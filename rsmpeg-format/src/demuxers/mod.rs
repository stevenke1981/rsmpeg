//! Built-in demuxer implementations for common container formats.
//!
//! Each demuxer parses the container header and discovers streams
//! (codec type, sample rate, channels, dimensions, etc.).

mod avi_demuxer;
mod flac_demuxer;
mod mkv_demuxer;
mod mp4_demuxer;
mod raw_demuxer;
mod wav_demuxer;

pub use avi_demuxer::AVIDemuxer;
pub use flac_demuxer::FLACDemuxer;
pub use mkv_demuxer::MKVDemuxer;
pub use mp4_demuxer::MP4Demuxer;
pub use raw_demuxer::RawVideoDemuxer;
pub use wav_demuxer::WAVDemuxer;
