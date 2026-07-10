//! Shared types for GUI media player.
//!
//! These types are accessed from both the UI thread (egui) and the
//! background engine thread.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// Lock a `Mutex<PlaybackState>` safely, recovering from a poisoned mutex
/// (e.g., if the engine thread panicked while holding the lock).
/// Calling `.unwrap()` on a poisoned mutex panics, which crashes the UI.
pub fn lock_state(state: &Arc<Mutex<PlaybackState>>) -> MutexGuard<'_, PlaybackState> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Shared playback state (read/written by both engine thread and UI).
#[derive(Clone)]
pub struct PlaybackState {
    pub playing: bool,
    pub stop_requested: bool,
    pub position_sec: f64,
    pub duration_sec: f64,
    pub status: String,
    pub volume: f32,
}

impl Default for PlaybackState {
    fn default() -> Self {
        Self {
            playing: true,
            stop_requested: false,
            position_sec: 0.0,
            duration_sec: 0.0,
            status: String::new(),
            volume: 0.8,
        }
    }
}

/// Decoded video frame sent from engine thread to UI.
#[derive(Clone)]
pub struct FrameData {
    pub rgba: Vec<u8>,
    pub width: usize,
    pub height: usize,
}
