//! High-level [`Player`] handle — CLI and GUI entry point.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use crate::clock::MasterClock;
use crate::command::{PlayerCommand, SeekMode};
use crate::event::{PlayerEvent, PlayerSnapshot};
use crate::queue::BoundedQueue;

const CMD_CAPACITY: usize = 64;
const EVT_CAPACITY: usize = 64;

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
}

impl PlayerBuilder {
    pub fn new() -> Self {
        Self {
            input: None,
            prefer_native_pipeline: true,
            volume: 0.8,
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

    pub fn build(self) -> Result<Player, PlayerError> {
        let path = self.input.ok_or(PlayerError::NoInput)?;
        Ok(Player::new(path, self.prefer_native_pipeline, self.volume))
    }
}

/// Unified player handle.
///
/// Phase 2 scaffold: command/event channels and generation tracking work now.
/// Demux/decode workers will drain these queues in later phases; until then
/// the control plane updates a local snapshot so hosts can integrate safely.
pub struct Player {
    path: PathBuf,
    prefer_native: bool,
    state: PlayerState,
    generation: AtomicU64,
    volume: f32,
    position: Duration,
    duration: Duration,
    playing: bool,
    clock: MasterClock,
    cmd_tx: mpsc::SyncSender<PlayerCommand>,
    cmd_rx: mpsc::Receiver<PlayerCommand>,
    /// Outbound events for the host (bounded).
    events: BoundedQueue<PlayerEvent>,
    shut_down: bool,
}

impl Player {
    fn new(path: PathBuf, prefer_native: bool, volume: f32) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::sync_channel(CMD_CAPACITY);
        let mut player = Self {
            path,
            prefer_native,
            state: PlayerState::Ready,
            generation: AtomicU64::new(1),
            volume,
            position: Duration::ZERO,
            duration: Duration::ZERO,
            playing: false,
            clock: MasterClock::new(),
            cmd_tx,
            cmd_rx,
            events: BoundedQueue::new(EVT_CAPACITY),
            shut_down: false,
        };
        player.push_snapshot();
        let _ = player.prefer_native; // reserved for backend selection
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

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    fn next_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Enqueue a command (non-blocking).  Fails if the bounded queue is full.
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
        self.drain_commands();
        Ok(())
    }

    pub fn pause(&mut self) -> Result<(), PlayerError> {
        let g = self.generation();
        self.send_command(PlayerCommand::Pause { generation: g })?;
        self.drain_commands();
        Ok(())
    }

    pub fn stop(&mut self) -> Result<(), PlayerError> {
        let g = self.generation();
        self.send_command(PlayerCommand::Stop { generation: g })?;
        self.drain_commands();
        Ok(())
    }

    pub fn seek(&mut self, position: Duration) -> Result<(), PlayerError> {
        let g = self.next_generation();
        self.send_command(PlayerCommand::Seek {
            position,
            mode: SeekMode::Coarse,
            generation: g,
        })?;
        self.drain_commands();
        Ok(())
    }

    pub fn set_volume(&mut self, volume: f32) -> Result<(), PlayerError> {
        let g = self.generation();
        self.send_command(PlayerCommand::SetVolume {
            volume: volume.clamp(0.0, 1.0),
            generation: g,
        })?;
        self.drain_commands();
        Ok(())
    }

    pub fn shutdown(&mut self) -> Result<(), PlayerError> {
        let g = self.generation();
        let _ = self.send_command(PlayerCommand::Shutdown { generation: g });
        self.drain_commands();
        self.shut_down = true;
        Ok(())
    }

    /// Poll one outbound event (non-blocking).
    pub fn poll_event(&mut self) -> Option<PlayerEvent> {
        self.drain_commands();
        self.events.pop()
    }

    /// Process pending control commands on the calling thread.
    ///
    /// Later phases move this into a dedicated control worker.
    pub fn drain_commands(&mut self) {
        while let Ok(cmd) = self.cmd_rx.try_recv() {
            // Discard commands from a superseded generation when applicable.
            match cmd {
                PlayerCommand::Play { generation } => {
                    self.playing = true;
                    self.state = PlayerState::Playing;
                    // resume() if previously paused, else start()
                    if self.clock.clock().is_paused() {
                        self.clock.clock_mut().resume();
                    }
                    self.clock.clock_mut().start();
                    self.push_event(PlayerEvent::Snapshot(self.snapshot(generation)));
                }
                PlayerCommand::Pause { generation } => {
                    self.playing = false;
                    self.state = PlayerState::Paused;
                    self.clock.clock_mut().pause();
                    self.push_event(PlayerEvent::Snapshot(self.snapshot(generation)));
                }
                PlayerCommand::Stop { generation } => {
                    self.playing = false;
                    self.state = PlayerState::Ready;
                    self.position = Duration::ZERO;
                    self.clock.clock_mut().seek(Duration::ZERO);
                    self.clock.clock_mut().pause();
                    self.push_event(PlayerEvent::Snapshot(self.snapshot(generation)));
                }
                PlayerCommand::Seek {
                    position,
                    generation,
                    ..
                } => {
                    self.state = PlayerState::Seeking;
                    self.position = position;
                    self.clock.clock_mut().seek(position);
                    self.push_event(PlayerEvent::SeekCompleted {
                        position,
                        generation,
                    });
                    self.state = if self.playing {
                        PlayerState::Playing
                    } else {
                        PlayerState::Paused
                    };
                    self.push_event(PlayerEvent::Snapshot(self.snapshot(generation)));
                }
                PlayerCommand::SetVolume { volume, generation } => {
                    self.volume = volume.clamp(0.0, 1.0);
                    self.push_event(PlayerEvent::Snapshot(self.snapshot(generation)));
                }
                PlayerCommand::SelectAudioTrack { generation, .. }
                | PlayerCommand::SelectVideoTrack { generation, .. } => {
                    self.push_event(PlayerEvent::Warning {
                        message: "track selection not yet wired to demux".into(),
                        generation,
                    });
                }
                PlayerCommand::SetPlaybackRate { rate, generation } => {
                    self.clock.clock_mut().set_rate(rate);
                    self.push_event(PlayerEvent::Snapshot(self.snapshot(generation)));
                }
                PlayerCommand::Shutdown { .. } => {
                    self.playing = false;
                    self.state = PlayerState::Idle;
                    self.shut_down = true;
                    self.events.clear();
                }
            }
        }
    }

    fn snapshot(&self, generation: u64) -> PlayerSnapshot {
        PlayerSnapshot {
            playing: self.playing,
            position: self.position,
            duration: self.duration,
            volume: self.volume,
            generation,
            status: format!("{:?}", self.state),
        }
    }

    fn push_snapshot(&mut self) {
        let g = self.generation();
        self.push_event(PlayerEvent::Snapshot(self.snapshot(g)));
    }

    fn push_event(&mut self, ev: PlayerEvent) {
        let _ = self.events.push(ev);
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
    fn play_pause_updates_state() {
        let mut p = Player::builder().input("dummy.mp4").build().unwrap();
        p.play().unwrap();
        assert_eq!(p.state(), PlayerState::Playing);
        p.pause().unwrap();
        assert_eq!(p.state(), PlayerState::Paused);
        // Drain events
        let mut saw_pause = false;
        while let Some(ev) = p.poll_event() {
            if let PlayerEvent::Snapshot(s) = ev {
                if !s.playing {
                    saw_pause = true;
                }
            }
        }
        assert!(saw_pause);
    }

    #[test]
    fn seek_bumps_generation() {
        let mut p = Player::builder().input("x.mp4").build().unwrap();
        let g0 = p.generation();
        p.seek(Duration::from_secs(10)).unwrap();
        assert!(p.generation() > g0);
        let mut completed = false;
        while let Some(ev) = p.poll_event() {
            if matches!(ev, PlayerEvent::SeekCompleted { .. }) {
                completed = true;
            }
        }
        assert!(completed);
    }

    #[test]
    fn twenty_pause_resume_cycles() {
        let mut p = Player::builder().input("x.mp4").build().unwrap();
        for _ in 0..20 {
            p.play().unwrap();
            assert_eq!(p.state(), PlayerState::Playing);
            p.pause().unwrap();
            assert_eq!(p.state(), PlayerState::Paused);
        }
    }
}
