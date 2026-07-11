//! Events emitted by the player toward UI / CLI consumers.

use std::time::Duration;

/// Lightweight state snapshot for UI binding (replaces large shared Mutex state).
#[derive(Debug, Clone)]
pub struct PlayerSnapshot {
    pub playing: bool,
    pub position: Duration,
    pub duration: Duration,
    pub volume: f32,
    pub generation: u64,
    pub status: String,
}

/// Player → host events.  Frame payloads stay small / reference-counted later.
#[derive(Debug, Clone)]
pub enum PlayerEvent {
    Snapshot(PlayerSnapshot),
    PositionChanged {
        position: Duration,
        generation: u64,
    },
    /// RGBA preview / display frame (Phase 2 placeholder).
    VideoFrame {
        width: usize,
        height: usize,
        rgba: Vec<u8>,
        pts: Duration,
        generation: u64,
    },
    SeekCompleted {
        position: Duration,
        generation: u64,
    },
    Ended {
        generation: u64,
    },
    Error {
        message: String,
        generation: u64,
    },
    /// Unsupported codec reported without treating it as fatal when audio remains.
    Warning {
        message: String,
        generation: u64,
    },
}

impl PlayerEvent {
    pub fn generation(&self) -> u64 {
        match self {
            Self::Snapshot(s) => s.generation,
            Self::PositionChanged { generation, .. }
            | Self::VideoFrame { generation, .. }
            | Self::SeekCompleted { generation, .. }
            | Self::Ended { generation }
            | Self::Error { generation, .. }
            | Self::Warning { generation, .. } => *generation,
        }
    }
}
