//! Built-in muxer implementations for container formats.

mod wav_muxer;
mod y4m_muxer;

pub use wav_muxer::WAVMuxer;
pub use y4m_muxer::Y4mMuxer;
