//! Commands sent from UI / CLI into the player control plane.

use std::time::Duration;

/// Seek precision request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekMode {
    /// Fast, keyframe-aligned.
    Coarse,
    /// Decode forward from keyframe to the exact target.
    Precise,
}

/// Control-plane command.  Every command carries a generation so stale
/// results from older seeks can be discarded.
#[derive(Debug, Clone)]
pub enum PlayerCommand {
    Play {
        generation: u64,
    },
    Pause {
        generation: u64,
    },
    Stop {
        generation: u64,
    },
    Seek {
        position: Duration,
        mode: SeekMode,
        generation: u64,
    },
    SetVolume {
        volume: f32,
        generation: u64,
    },
    SelectAudioTrack {
        index: usize,
        generation: u64,
    },
    SelectVideoTrack {
        index: usize,
        generation: u64,
    },
    SetPlaybackRate {
        rate: f64,
        generation: u64,
    },
    Shutdown {
        generation: u64,
    },
}

impl PlayerCommand {
    pub fn generation(&self) -> u64 {
        match self {
            Self::Play { generation }
            | Self::Pause { generation }
            | Self::Stop { generation }
            | Self::Seek { generation, .. }
            | Self::SetVolume { generation, .. }
            | Self::SelectAudioTrack { generation, .. }
            | Self::SelectVideoTrack { generation, .. }
            | Self::SetPlaybackRate { generation, .. }
            | Self::Shutdown { generation } => *generation,
        }
    }
}
