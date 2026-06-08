//! Shared types for GUI media player.
//!
//! These types are accessed from both the UI thread (egui) and the
//! background engine thread.

/// Shared playback state (read/written by both engine thread and UI).
#[derive(Clone)]
pub struct PlaybackState {
    pub playing: bool,
    pub position_sec: f64,
    pub duration_sec: f64,
    pub status: String,
    pub volume: f32,
}

impl Default for PlaybackState {
    fn default() -> Self {
        Self {
            playing: true,
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
