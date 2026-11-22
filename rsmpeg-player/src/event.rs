//! Events emitted by the player toward UI / CLI consumers.

use std::sync::Arc;
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

/// Player → host events.
///
/// Video frames carry their pixel buffer as an [`Arc`] so repeating a frame
/// (`SyncAction::Duplicate`) or fanning it out to several consumers costs a
/// reference-count bump instead of a full-frame `memcpy`.
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
        rgba: Arc<Vec<u8>>,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(rgba: Vec<u8>, generation: u64) -> PlayerEvent {
        PlayerEvent::VideoFrame {
            width: 2,
            height: 2,
            rgba: Arc::new(rgba),
            pts: Duration::from_millis(40),
            generation,
        }
    }

    #[test]
    fn generation_is_exposed_for_every_variant() {
        let snapshot = PlayerEvent::Snapshot(PlayerSnapshot {
            playing: true,
            position: Duration::ZERO,
            duration: Duration::from_secs(1),
            volume: 0.5,
            generation: 7,
            status: "playing".into(),
        });
        let variants = [
            snapshot,
            PlayerEvent::PositionChanged {
                position: Duration::ZERO,
                generation: 7,
            },
            frame(vec![0; 16], 7),
            PlayerEvent::SeekCompleted {
                position: Duration::ZERO,
                generation: 7,
            },
            PlayerEvent::Ended { generation: 7 },
            PlayerEvent::Error {
                message: "boom".into(),
                generation: 7,
            },
            PlayerEvent::Warning {
                message: "meh".into(),
                generation: 7,
            },
        ];
        for ev in &variants {
            assert_eq!(ev.generation(), 7, "{ev:?}");
        }
    }

    #[test]
    fn cloning_a_video_event_shares_the_pixel_buffer() {
        let ev = frame(vec![1u8, 2, 3, 4], 1);
        let dup = ev.clone();
        let (a, b) = match (&ev, &dup) {
            (PlayerEvent::VideoFrame { rgba: a, .. }, PlayerEvent::VideoFrame { rgba: b, .. }) => {
                (a, b)
            }
            _ => unreachable!("constructed as VideoFrame"),
        };
        // The clone must share the allocation, not copy it — this is what makes
        // `SyncAction::Duplicate` in the playback loop cheap.
        assert!(Arc::ptr_eq(a, b));
        assert_eq!(a.as_slice(), &[1u8, 2, 3, 4]);
        // Strong count is 2 while both events are alive.
        assert_eq!(Arc::strong_count(a), 2);
    }

    #[test]
    fn dropping_the_clone_keeps_the_original_buffer_alive() {
        let ev = frame(vec![9u8; 32], 3);
        {
            let _dup = ev.clone();
        }
        match &ev {
            PlayerEvent::VideoFrame { rgba, .. } => {
                assert_eq!(rgba.len(), 32);
                assert_eq!(Arc::strong_count(rgba), 1);
            }
            _ => panic!("expected VideoFrame"),
        }
    }
}
