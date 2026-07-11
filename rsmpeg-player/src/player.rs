//! High-level [`Player`] handle — CLI and GUI entry point.
//!
//! Hosts only send commands and poll events.  Demux/decode never run on the
//! calling thread.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::command::{PlayerCommand, SeekMode};
use crate::demux_worker;
use crate::event::{PlayerEvent, PlayerSnapshot};

const CMD_CAPACITY: usize = 64;
const EVT_CAPACITY: usize = 128;

/// Player lifecycle / readiness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerState {
    Idle,
    Opening,
    Ready,
    Playing,
    Paused,
    Seeking,
    Ended,
    Error,
}

/// Errors from the player control plane.
#[derive(Debug, thiserror::Error)]
pub enum PlayerError {
    #[error("no input path configured")]
    NoInput,
    #[error("command queue full")]
    CommandQueueFull,
    #[error("playback rate must be finite and between 0.25 and 4.0")]
    InvalidPlaybackRate,
    #[error("player already shut down")]
    ShutDown,
    #[error("{0}")]
    Message(String),
}

/// Builder for [`Player`].
#[derive(Debug, Default)]
pub struct PlayerBuilder {
    input: Option<PathBuf>,
    prefer_native_pipeline: bool,
    volume: f32,
    /// Start playback immediately when the worker opens the file.
    autoplay: bool,
}

impl PlayerBuilder {
    pub fn new() -> Self {
        Self {
            input: None,
            prefer_native_pipeline: true,
            volume: 0.8,
            autoplay: true,
        }
    }

    pub fn input(mut self, path: impl Into<PathBuf>) -> Self {
        self.input = Some(path.into());
        self
    }

    pub fn prefer_native_pipeline(mut self, yes: bool) -> Self {
        self.prefer_native_pipeline = yes;
        self
    }

    pub fn volume(mut self, v: f32) -> Self {
        self.volume = v.clamp(0.0, 1.0);
        self
    }

    pub fn autoplay(mut self, yes: bool) -> Self {
        self.autoplay = yes;
        self
    }

    pub fn build(self) -> Result<Player, PlayerError> {
        let path = self.input.ok_or(PlayerError::NoInput)?;
        Ok(Player::open(
            path,
            self.prefer_native_pipeline,
            self.volume,
            self.autoplay,
        ))
    }
}

/// Unified player handle.
pub struct Player {
    path: PathBuf,
    #[allow(dead_code)]
    prefer_native: bool,
    state: PlayerState,
    generation: AtomicU64,
    volume: f32,
    playback_rate: f64,
    position: Duration,
    duration: Duration,
    playing: bool,
    cmd_tx: mpsc::SyncSender<PlayerCommand>,
    event_rx: mpsc::Receiver<PlayerEvent>,
    handle: Option<thread::JoinHandle<()>>,
    shut_down: bool,
    last_error: Option<String>,
}

impl Player {
    fn open(path: PathBuf, prefer_native: bool, volume: f32, autoplay: bool) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::sync_channel(CMD_CAPACITY);
        let (event_tx, event_rx) = mpsc::sync_channel(EVT_CAPACITY);
        let handle =
            demux_worker::spawn_worker(path.clone(), volume, prefer_native, cmd_rx, event_tx);

        let player = Self {
            path,
            prefer_native,
            state: PlayerState::Opening,
            generation: AtomicU64::new(1),
            volume,
            playback_rate: 1.0,
            position: Duration::ZERO,
            duration: Duration::ZERO,
            playing: autoplay,
            cmd_tx,
            event_rx,
            handle: Some(handle),
            shut_down: false,
            last_error: None,
        };

        if !autoplay {
            let g = player.generation();
            let _ = player.send_command(PlayerCommand::Pause { generation: g });
        }
        player
    }

    pub fn builder() -> PlayerBuilder {
        PlayerBuilder::new()
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub fn state(&self) -> PlayerState {
        self.state
    }

    pub fn position(&self) -> Duration {
        self.position
    }

    pub fn duration(&self) -> Duration {
        self.duration
    }

    pub fn volume(&self) -> f32 {
        self.volume
    }

    pub fn playback_rate(&self) -> f64 {
        self.playback_rate
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    fn next_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Enqueue a command (non-blocking).
    pub fn send_command(&self, cmd: PlayerCommand) -> Result<(), PlayerError> {
        if self.shut_down {
            return Err(PlayerError::ShutDown);
        }
        self.cmd_tx
            .try_send(cmd)
            .map_err(|_| PlayerError::CommandQueueFull)
    }

    pub fn play(&mut self) -> Result<(), PlayerError> {
        let g = self.generation();
        self.send_command(PlayerCommand::Play { generation: g })?;
        self.playing = true;
        self.state = PlayerState::Playing;
        Ok(())
    }

    pub fn pause(&mut self) -> Result<(), PlayerError> {
        let g = self.generation();
        self.send_command(PlayerCommand::Pause { generation: g })?;
        self.playing = false;
        self.state = PlayerState::Paused;
        Ok(())
    }

    pub fn stop(&mut self) -> Result<(), PlayerError> {
        let g = self.generation();
        self.send_command(PlayerCommand::Stop { generation: g })?;
        self.playing = false;
        self.state = PlayerState::Ready;
        self.position = Duration::ZERO;
        Ok(())
    }

    pub fn seek(&mut self, position: Duration) -> Result<(), PlayerError> {
        let g = self.next_generation();
        self.send_command(PlayerCommand::Seek {
            position,
            mode: SeekMode::Coarse,
            generation: g,
        })?;
        self.position = position;
        self.state = PlayerState::Seeking;
        Ok(())
    }

    pub fn set_volume(&mut self, volume: f32) -> Result<(), PlayerError> {
        let g = self.generation();
        let volume = volume.clamp(0.0, 1.0);
        self.send_command(PlayerCommand::SetVolume {
            volume,
            generation: g,
        })?;
        self.volume = volume;
        Ok(())
    }

    /// Change playback speed for both audio and video. Supported rates are
    /// deliberately bounded to keep the video scheduler and audio backend in
    /// a range they can service reliably.
    pub fn set_playback_rate(&mut self, rate: f64) -> Result<(), PlayerError> {
        if !rate.is_finite() || !(0.25..=4.0).contains(&rate) {
            return Err(PlayerError::InvalidPlaybackRate);
        }
        let g = self.generation();
        self.send_command(PlayerCommand::SetPlaybackRate {
            rate,
            generation: g,
        })?;
        self.playback_rate = rate;
        Ok(())
    }

    pub fn shutdown(&mut self) -> Result<(), PlayerError> {
        if self.shut_down {
            return Ok(());
        }
        let g = self.generation();
        let _ = self.send_command(PlayerCommand::Shutdown { generation: g });
        self.shut_down = true;
        self.playing = false;
        self.state = PlayerState::Idle;
        // Non-blocking: do not join on the UI thread (todos.md).
        if let Some(h) = self.handle.take() {
            // Detach — worker exits on Shutdown.
            drop(h);
        }
        Ok(())
    }

    /// Poll one outbound event (non-blocking) and update local cache.
    pub fn poll_event(&mut self) -> Option<PlayerEvent> {
        match self.event_rx.try_recv() {
            Ok(ev) => {
                self.apply_event(&ev);
                Some(ev)
            }
            Err(_) => None,
        }
    }

    /// Drain all pending events; returns the latest video frame if any.
    pub fn poll_all(&mut self) -> (Vec<PlayerEvent>, Option<PlayerEvent>) {
        let mut events = Vec::new();
        let mut latest_frame = None;
        while let Some(ev) = self.poll_event() {
            if matches!(ev, PlayerEvent::VideoFrame { .. }) {
                latest_frame = Some(ev);
            } else {
                events.push(ev);
            }
        }
        (events, latest_frame)
    }

    fn apply_event(&mut self, ev: &PlayerEvent) {
        match ev {
            PlayerEvent::Snapshot(s) => {
                self.playing = s.playing;
                self.position = s.position;
                self.duration = s.duration;
                self.volume = s.volume;
                self.state = if s.playing {
                    PlayerState::Playing
                } else if s.status == "paused" {
                    PlayerState::Paused
                } else if s.status == "opening" {
                    PlayerState::Opening
                } else {
                    PlayerState::Ready
                };
            }
            PlayerEvent::PositionChanged { position, .. } => {
                self.position = *position;
            }
            PlayerEvent::SeekCompleted { position, .. } => {
                self.position = *position;
                self.state = if self.playing {
                    PlayerState::Playing
                } else {
                    PlayerState::Paused
                };
            }
            PlayerEvent::Ended { .. } => {
                self.playing = false;
                self.state = PlayerState::Ended;
            }
            PlayerEvent::Error { message, .. } => {
                self.playing = false;
                self.state = PlayerState::Error;
                self.last_error = Some(message.clone());
            }
            PlayerEvent::VideoFrame { pts, .. } => {
                self.position = *pts;
                if self.state == PlayerState::Opening {
                    self.state = PlayerState::Playing;
                }
            }
            PlayerEvent::Warning { .. } => {}
        }
    }

    /// Convenience snapshot for UI binding.
    pub fn snapshot(&self) -> PlayerSnapshot {
        PlayerSnapshot {
            playing: self.playing,
            position: self.position,
            duration: self.duration,
            volume: self.volume,
            generation: self.generation(),
            status: format!("{:?}", self.state),
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_requires_input() {
        match Player::builder().build() {
            Err(PlayerError::NoInput) => {}
            other => panic!("expected NoInput, got {:?}", other.map(|_| "Player")),
        }
    }

    #[test]
    fn open_missing_file_reports_error_event() {
        let mut p = Player::builder()
            .input("definitely-missing-file-xyz.mp4")
            .build()
            .unwrap();
        // Worker should fail open and emit Error.
        let mut saw_error = false;
        for _ in 0..50 {
            thread::sleep(Duration::from_millis(20));
            while let Some(ev) = p.poll_event() {
                if matches!(ev, PlayerEvent::Error { .. }) {
                    saw_error = true;
                }
            }
            if saw_error {
                break;
            }
        }
        assert!(saw_error, "expected Error event for missing file");
    }

    #[test]
    fn seek_bumps_generation() {
        let mut p = Player::builder()
            .input("definitely-missing-file-xyz.mp4")
            .autoplay(false)
            .build()
            .unwrap();
        let g0 = p.generation();
        let _ = p.seek(Duration::from_secs(10));
        assert!(p.generation() > g0);
    }

    #[test]
    fn twenty_pause_resume_commands() {
        let mut p = Player::builder()
            .input("definitely-missing-file-xyz.mp4")
            .autoplay(false)
            .build()
            .unwrap();
        for _ in 0..20 {
            let _ = p.play();
            let _ = p.pause();
        }
        assert_eq!(p.state(), PlayerState::Paused);
    }

    #[test]
    fn rejects_invalid_playback_rate_before_enqueueing() {
        let mut p = Player::builder()
            .input("definitely-missing-file-xyz.mp4")
            .autoplay(false)
            .build()
            .unwrap();
        assert!(matches!(
            p.set_playback_rate(0.0),
            Err(PlayerError::InvalidPlaybackRate)
        ));
        assert_eq!(p.playback_rate(), 1.0);
    }

    #[test]
    fn playback_rate_enqueues_validated_command() {
        let (cmd_tx, cmd_rx) = mpsc::sync_channel(1);
        let (_event_tx, event_rx) = mpsc::sync_channel(1);
        let mut p = Player {
            path: PathBuf::from("test.mp4"),
            prefer_native: true,
            state: PlayerState::Ready,
            generation: AtomicU64::new(7),
            volume: 0.8,
            playback_rate: 1.0,
            position: Duration::ZERO,
            duration: Duration::ZERO,
            playing: false,
            cmd_tx,
            event_rx,
            handle: None,
            shut_down: false,
            last_error: None,
        };

        p.set_playback_rate(1.5).unwrap();
        assert_eq!(p.playback_rate(), 1.5);
        assert!(matches!(
            cmd_rx.try_recv(),
            Ok(PlayerCommand::SetPlaybackRate {
                rate,
                generation: 7
            }) if rate == 1.5
        ));
    }
}
