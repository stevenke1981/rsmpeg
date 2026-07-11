//! Unified rsmpeg playback core.
//!
//! CLI and GUI share this crate.  Hosts only send [`PlayerCommand`]s and poll
//! [`PlayerEvent`]s — demux / decode / audio never run on the UI thread.

#![forbid(unsafe_code)]

pub mod clock;
pub mod codec_detect;
pub mod command;
pub mod demux_worker;
pub mod event;
pub mod h264_bitstream;
pub mod native_pipeline;
pub mod player;
pub mod queue;

pub use clock::{MasterClock, PlaybackClock};
pub use codec_detect::{
    classify_track, codec_from_fourcc, find_audio_track, find_h264_video_track,
    find_unsupported_video, DetectedVideoCodec, TrackKind,
};
pub use command::{PlayerCommand, SeekMode};
pub use event::{PlayerEvent, PlayerSnapshot};
pub use h264_bitstream::{
    avcc_extradata_to_annex_b, avcc_nal_length_size, avcc_packet_to_annex_b,
    extract_avcc_streaming, is_annex_b, packet_for_decoder, H264BitstreamError,
    H264BitstreamFormat,
};
pub use player::{Player, PlayerBuilder, PlayerError, PlayerState};
pub use queue::BoundedQueue;
