//! High-level [`Player`] handle — CLI and GUI entry point.
//!
//! Hosts only send commands and poll events.  Demux/decode never run on the
//! calling thread.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::command::{PlayerCommand, SeekMode};
use crate::demux_worker;
use crate::event::{PlayerEvent, PlayerSnapshot};

const CMD_CAPACITY: usize = 64;
const EVT_CAPACITY: usize = 128;
const CONTROL_EVT_CAPACITY: usize = 256;
const TERMINAL_EVT_CAPACITY: usize = 8;

/// Reliability tier for events inside the unified ordered [`EventQueue`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventClass {
    /// High-frequency telemetry (`VideoFrame`, `PositionChanged`, `AudioLevel`, `Warning`).
    /// Drops incoming events once `EVT_CAPACITY` (128) stream items are buffered.
    Stream,
    /// Single-slot latest-wins preview frame emitted after a seek or video track
    /// switch (`force_one_frame`). Never dropped due to `Stream` saturation; a
    /// newer preview overwrites any unconsumed older preview so memory stays
    /// bounded to one preview frame.
    Preview,
    /// Control acknowledgements (`Snapshot`, `SeekCompleted`, `Tracks`, `DecoderInfo`).
    /// Isolated from `Stream` backpressure; bounded at `CONTROL_EVT_CAPACITY` (256),
    /// evicting an older `Snapshot` first if saturated.
    Control,
    /// Terminal lifecycle events (`Ended`, `Error`).
    /// Never evicted by `Stream`, `Preview`, or `Control`; bounded at
    /// `TERMINAL_EVT_CAPACITY` (8).
    Terminal,
}

struct QueuedEvent {
    class: EventClass,
    event: PlayerEvent,
}

struct EventQueueInner {
    entries: VecDeque<QueuedEvent>,
    stream_count: usize,
    control_count: usize,
    terminal_count: usize,
}

impl EventQueueInner {
    fn new() -> Self {
        Self {
            entries: VecDeque::with_capacity(EVT_CAPACITY + 32),
            stream_count: 0,
            control_count: 0,
            terminal_count: 0,
        }
    }

    fn push(&mut self, class: EventClass, event: PlayerEvent) {
        match class {
            EventClass::Stream => {
                if self.stream_count >= EVT_CAPACITY {
                    return;
                }
                self.stream_count += 1;
                self.entries.push_back(QueuedEvent { class, event });
            }
            EventClass::Preview => {
                if let Some(pos) = self
                    .entries
                    .iter()
                    .position(|q| q.class == EventClass::Preview)
                {
                    self.entries.remove(pos);
                }
                self.entries.push_back(QueuedEvent { class, event });
            }
            EventClass::Control => {
                if self.control_count >= CONTROL_EVT_CAPACITY {
                    let evict_idx = self
                        .entries
                        .iter()
                        .position(|q| {
                            q.class == EventClass::Control
                                && matches!(q.event, PlayerEvent::Snapshot(_))
                        })
                        .or_else(|| {
                            self.entries
                                .iter()
                                .position(|q| q.class == EventClass::Control)
                        });
                    if let Some(idx) = evict_idx {
                        self.entries.remove(idx);
                        self.control_count -= 1;
                    }
                }
                self.control_count += 1;
                self.entries.push_back(QueuedEvent { class, event });
            }
            EventClass::Terminal => {
                if self.terminal_count >= TERMINAL_EVT_CAPACITY {
                    return;
                }
                self.terminal_count += 1;
                self.entries.push_back(QueuedEvent { class, event });
            }
        }
    }

    fn pop(&mut self) -> Option<PlayerEvent> {
        let item = self.entries.pop_front()?;
        match item.class {
            EventClass::Stream => self.stream_count -= 1,
            EventClass::Preview => {}
            EventClass::Control => self.control_count -= 1,
            EventClass::Terminal => self.terminal_count -= 1,
        }
        Some(item.event)
    }
}

/// Shared single-lock ordered event queue guaranteeing linearizable FIFO order
/// across all reliability tiers.
#[derive(Clone)]
pub(crate) struct EventQueue {
    inner: Arc<Mutex<EventQueueInner>>,
}

impl EventQueue {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(EventQueueInner::new())),
        }
    }

    fn push(&self, class: EventClass, event: PlayerEvent) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.push(class, event);
    }

    pub(crate) fn pop(&self) -> Option<PlayerEvent> {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.pop()
    }
}

/// Non-blocking event sender backed by either the unified ordered [`EventQueue`]
/// or a single `SyncSender` for backwards-compatible public worker entry points.
#[derive(Clone)]
pub(crate) enum EventSender {
    Queued(EventQueue),
    Single(mpsc::SyncSender<PlayerEvent>),
}

impl EventSender {
    pub(crate) fn new(queue: EventQueue) -> Self {
        Self::Queued(queue)
    }

    pub(crate) fn single(normal: mpsc::SyncSender<PlayerEvent>) -> Self {
        Self::Single(normal)
    }

    /// Non-blocking emit preserving strict enqueue order across all event tiers.
    pub(crate) fn send(&self, ev: PlayerEvent) {
        match self {
            Self::Single(tx) => {
                let _ = tx.try_send(ev);
            }
            Self::Queued(queue) => {
                let class = match ev {
                    PlayerEvent::Ended { .. } | PlayerEvent::Error { .. } => EventClass::Terminal,
                    PlayerEvent::Snapshot(_)
                    | PlayerEvent::SeekCompleted { .. }
                    | PlayerEvent::Tracks { .. }
                    | PlayerEvent::DecoderInfo { .. } => EventClass::Control,
                    PlayerEvent::VideoFrame { .. }
                    | PlayerEvent::PositionChanged { .. }
                    | PlayerEvent::AudioLevel { .. }
                    | PlayerEvent::Warning { .. } => EventClass::Stream,
                };
                queue.push(class, ev);
            }
        }
    }

    /// Emit a single-slot latest-wins preview frame (never dropped by `Stream` saturation).
    pub(crate) fn send_preview(&self, ev: PlayerEvent) {
        match self {
            Self::Single(tx) => {
                let _ = tx.try_send(ev);
            }
            Self::Queued(queue) => {
                queue.push(EventClass::Preview, ev);
            }
        }
    }
}

/// Minimum supported playback speed multiplier.
pub const MIN_PLAYBACK_RATE: f64 = 0.25;
/// Maximum supported playback speed multiplier.
pub const MAX_PLAYBACK_RATE: f64 = 4.0;

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
    playback_rate: f64,
    /// Start playback immediately when the worker opens the file.
    autoplay: bool,
}

impl PlayerBuilder {
    pub fn new() -> Self {
        Self {
            input: None,
            prefer_native_pipeline: true,
            volume: 0.8,
            playback_rate: 1.0,
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

    pub fn playback_rate(mut self, rate: f64) -> Self {
        self.playback_rate = clamp_playback_rate(rate);
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
            self.playback_rate,
            self.autoplay,
        ))
    }
}

fn clamp_playback_rate(rate: f64) -> f64 {
    if !rate.is_finite() {
        1.0
    } else {
        rate.clamp(MIN_PLAYBACK_RATE, MAX_PLAYBACK_RATE)
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
    event_queue: EventQueue,
    handle: Option<thread::JoinHandle<()>>,
    shut_down: bool,
    last_error: Option<String>,
}

impl Player {
    fn open(
        path: PathBuf,
        prefer_native: bool,
        volume: f32,
        playback_rate: f64,
        autoplay: bool,
    ) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::sync_channel(CMD_CAPACITY);
        let event_queue = EventQueue::new();
        let rate = clamp_playback_rate(playback_rate);
        let handle = demux_worker::spawn_worker_with_sender(
            path.clone(),
            volume,
            prefer_native,
            cmd_rx,
            EventSender::new(event_queue.clone()),
        );

        let player = Self {
            path,
            prefer_native,
            state: PlayerState::Opening,
            generation: AtomicU64::new(1),
            volume,
            playback_rate: rate,
            position: Duration::ZERO,
            duration: Duration::ZERO,
            playing: autoplay,
            cmd_tx,
            event_queue,
            handle: Some(handle),
            shut_down: false,
            last_error: None,
        };

        if (rate - 1.0).abs() > f64::EPSILON {
            let g = player.generation();
            let _ = player.send_command(PlayerCommand::SetPlaybackRate {
                rate,
                generation: g,
            });
        }
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

    /// Enqueue a command that advances `generation` ONLY if enqueue succeeds.
    fn send_with_next_generation(
        &mut self,
        build: impl FnOnce(u64) -> PlayerCommand,
    ) -> Result<u64, PlayerError> {
        let next_g = self.generation() + 1;
        self.send_command(build(next_g))?;
        self.generation.store(next_g, Ordering::Relaxed);
        Ok(next_g)
    }

    /// Enqueue a command (non-blocking).
    pub fn send_command(&self, cmd: PlayerCommand) -> Result<(), PlayerError> {
        if self.shut_down {
            return Err(PlayerError::ShutDown);
        }
        self.cmd_tx.try_send(cmd).map_err(|err| match err {
            mpsc::TrySendError::Full(_) => PlayerError::CommandQueueFull,
            mpsc::TrySendError::Disconnected(_) => PlayerError::ShutDown,
        })
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
        self.send_with_next_generation(|g| PlayerCommand::Seek {
            position,
            mode: SeekMode::Coarse,
            generation: g,
        })?;
        self.position = position;
        self.state = PlayerState::Seeking;
        Ok(())
    }

    /// Seek precisely to `position` (decodes forward from nearest keyframe and drops pre-target frames).
    pub fn seek_precise(&mut self, position: Duration) -> Result<(), PlayerError> {
        self.send_with_next_generation(|g| PlayerCommand::Seek {
            position,
            mode: SeekMode::Precise,
            generation: g,
        })?;
        self.position = position;
        self.state = PlayerState::Seeking;
        Ok(())
    }

    /// Pause playback if playing and advance by one video frame.
    pub fn step_forward(&mut self) -> Result<(), PlayerError> {
        self.send_with_next_generation(|g| PlayerCommand::StepForward { generation: g })?;
        self.playing = false;
        self.state = PlayerState::Paused;
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

    pub fn set_playback_rate(&mut self, rate: f64) -> Result<(), PlayerError> {
        let g = self.generation();
        let rate = clamp_playback_rate(rate);
        self.send_command(PlayerCommand::SetPlaybackRate {
            rate,
            generation: g,
        })?;
        self.playback_rate = rate;
        Ok(())
    }

    /// Switch the active audio stream by track index.
    pub fn select_audio_track(&mut self, index: usize) -> Result<(), PlayerError> {
        self.send_with_next_generation(|g| PlayerCommand::SelectAudioTrack {
            index,
            generation: g,
        })?;
        Ok(())
    }

    /// Switch the active video stream by track index.
    pub fn select_video_track(&mut self, index: usize) -> Result<(), PlayerError> {
        self.send_with_next_generation(|g| PlayerCommand::SelectVideoTrack {
            index,
            generation: g,
        })?;
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

    /// Poll one outbound event (non-blocking) in strict linearizable enqueue order
    /// across all reliability tiers (stream, control, terminal).
    pub fn poll_event(&mut self) -> Option<PlayerEvent> {
        let ev = self.event_queue.pop()?;
        self.apply_event(&ev);
        Some(ev)
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
            PlayerEvent::Warning { .. }
            | PlayerEvent::AudioLevel { .. }
            | PlayerEvent::Tracks { .. }
            | PlayerEvent::DecoderInfo { .. } => {}
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

    /// Construct a `Player` backed by caller-controlled channels (no background worker
    /// racing to exit) so queue saturation, generation consistency, and event backpressure
    /// can be verified deterministically.
    fn make_channel_player() -> (Player, mpsc::Receiver<PlayerCommand>, EventSender) {
        let (cmd_tx, cmd_rx) = mpsc::sync_channel(CMD_CAPACITY);
        let event_queue = EventQueue::new();
        let player = Player {
            path: PathBuf::from("synthetic.mp4"),
            prefer_native: true,
            state: PlayerState::Playing,
            generation: AtomicU64::new(1),
            volume: 0.8,
            playback_rate: 1.0,
            position: Duration::ZERO,
            duration: Duration::from_secs(10),
            playing: true,
            cmd_tx,
            event_queue: event_queue.clone(),
            handle: None,
            shut_down: false,
            last_error: None,
        };
        (player, cmd_rx, EventSender::new(event_queue))
    }

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
        let (mut p, _cmd_rx, _evt_tx) = make_channel_player();
        let g0 = p.generation();
        p.seek(Duration::from_secs(10)).unwrap();
        assert_eq!(p.generation(), g0 + 1);
    }

    #[test]
    fn twenty_pause_resume_commands() {
        let (mut p, _cmd_rx, _evt_tx) = make_channel_player();
        for _ in 0..20 {
            p.play().unwrap();
            p.pause().unwrap();
        }
        assert_eq!(p.state(), PlayerState::Paused);
    }

    #[test]
    fn set_playback_rate_clamps_and_stores() {
        let (mut p, _cmd_rx, _evt_tx) = make_channel_player();
        p.set_playback_rate(1.5).unwrap();
        assert!((p.playback_rate() - 1.5).abs() < f64::EPSILON);

        p.set_playback_rate(0.1).unwrap();
        assert!((p.playback_rate() - MIN_PLAYBACK_RATE).abs() < f64::EPSILON);

        p.set_playback_rate(10.0).unwrap();
        assert!((p.playback_rate() - MAX_PLAYBACK_RATE).abs() < f64::EPSILON);

        p.set_playback_rate(f64::NAN).unwrap();
        assert!((p.playback_rate() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn select_audio_and_video_track_bump_generation() {
        let (mut p, _cmd_rx, _evt_tx) = make_channel_player();
        let g0 = p.generation();
        p.select_audio_track(1).unwrap();
        let g1 = p.generation();
        assert_eq!(g1, g0 + 1);
        p.select_video_track(0).unwrap();
        assert_eq!(p.generation(), g1 + 1);
    }

    #[test]
    fn full_command_queue_does_not_bump_generation_or_mutate_state() {
        let (mut p, _cmd_rx, _evt_tx) = make_channel_player();
        // Saturate all 64 slots of the bounded command channel.
        for _ in 0..CMD_CAPACITY {
            p.set_volume(0.5).expect("enqueue within capacity");
        }
        let gen_before = p.generation();
        let pos_before = p.position();
        let state_before = p.state();
        let rate_before = p.playback_rate();

        assert!(matches!(
            p.select_audio_track(3),
            Err(PlayerError::CommandQueueFull)
        ));
        assert_eq!(p.generation(), gen_before);

        assert!(matches!(
            p.select_video_track(1),
            Err(PlayerError::CommandQueueFull)
        ));
        assert_eq!(p.generation(), gen_before);

        assert!(matches!(
            p.seek(Duration::from_secs(4)),
            Err(PlayerError::CommandQueueFull)
        ));
        assert_eq!(p.generation(), gen_before);
        assert_eq!(p.position(), pos_before);
        assert_eq!(p.state(), state_before);

        assert!(matches!(
            p.set_playback_rate(2.0),
            Err(PlayerError::CommandQueueFull)
        ));
        assert!((p.playback_rate() - rate_before).abs() < f64::EPSILON);
    }

    #[test]
    fn disconnected_worker_reports_shutdown_and_preserves_generation() {
        let (mut p, cmd_rx, _evt_tx) = make_channel_player();
        drop(cmd_rx);
        let gen_before = p.generation();

        assert!(matches!(p.play(), Err(PlayerError::ShutDown)));
        assert!(matches!(
            p.select_audio_track(2),
            Err(PlayerError::ShutDown)
        ));
        assert_eq!(p.generation(), gen_before);
    }

    #[test]
    fn saturated_event_channel_still_delivers_ended_and_error_terminal_events() {
        // Test Ended under full streaming backpressure.
        let (mut p1, _rx1, evt_tx1) = make_channel_player();
        for i in 0..(EVT_CAPACITY + 32) {
            evt_tx1.send(PlayerEvent::PositionChanged {
                position: Duration::from_millis(i as u64),
                generation: 1,
            });
        }
        evt_tx1.send(PlayerEvent::Ended { generation: 1 });

        let mut count = 0usize;
        let mut saw_ended = false;
        while let Some(ev) = p1.poll_event() {
            count += 1;
            if matches!(ev, PlayerEvent::Ended { generation: 1 }) {
                saw_ended = true;
            }
        }
        assert_eq!(count, EVT_CAPACITY + 1);
        assert!(saw_ended, "Ended must never be dropped by full event queue");
        assert_eq!(p1.state(), PlayerState::Ended);

        // Test Error under full streaming backpressure.
        let (mut p2, _rx2, evt_tx2) = make_channel_player();
        for i in 0..EVT_CAPACITY {
            evt_tx2.send(PlayerEvent::PositionChanged {
                position: Duration::from_millis(i as u64),
                generation: 1,
            });
        }
        evt_tx2.send(PlayerEvent::Error {
            message: "fatal decode failure".into(),
            generation: 1,
        });

        let mut saw_error = false;
        while let Some(ev) = p2.poll_event() {
            if matches!(ev, PlayerEvent::Error { ref message, .. } if message == "fatal decode failure")
            {
                saw_error = true;
            }
        }
        assert!(saw_error, "Error must never be dropped by full event queue");
        assert_eq!(p2.state(), PlayerState::Error);
    }

    #[test]
    fn saturated_stream_still_delivers_all_control_replies_in_chronological_order() {
        let (mut p, _rx, evt_tx) = make_channel_player();
        // Saturate the 128-slot streaming lane with stale frames/positions.
        for i in 0..(EVT_CAPACITY + 64) {
            evt_tx.send(PlayerEvent::PositionChanged {
                position: Duration::from_millis(i as u64),
                generation: 1,
            });
        }
        // Emit control replies after stream saturation (pause, seek, track switch, rate, volume).
        evt_tx.send(PlayerEvent::Snapshot(PlayerSnapshot {
            playing: false,
            position: Duration::from_secs(1),
            duration: Duration::from_secs(10),
            volume: 0.8,
            generation: 1,
            status: "paused".into(),
        }));
        evt_tx.send(PlayerEvent::SeekCompleted {
            position: Duration::from_secs(7),
            generation: 2,
        });
        evt_tx.send(PlayerEvent::Tracks {
            video_tracks: Vec::new(),
            audio_tracks: Vec::new(),
            active_video: None,
            active_audio: Some(3),
            generation: 3,
        });
        evt_tx.send(PlayerEvent::Snapshot(PlayerSnapshot {
            playing: false,
            position: Duration::from_secs(7),
            duration: Duration::from_secs(10),
            volume: 0.8,
            generation: 3,
            status: "rate".into(),
        }));
        evt_tx.send(PlayerEvent::Snapshot(PlayerSnapshot {
            playing: false,
            position: Duration::from_secs(7),
            duration: Duration::from_secs(10),
            volume: 0.25,
            generation: 3,
            status: "volume".into(),
        }));

        let mut drained = Vec::new();
        while let Some(ev) = p.poll_event() {
            drained.push(ev);
        }
        assert_eq!(drained.len(), EVT_CAPACITY + 5);
        // The 5 control replies must arrive strictly AFTER the 128 older streaming events
        // and in exact chronological order.
        let tail = &drained[EVT_CAPACITY..];
        assert!(matches!(&tail[0], PlayerEvent::Snapshot(s) if s.status == "paused"));
        assert!(matches!(
            &tail[1],
            PlayerEvent::SeekCompleted { position, generation: 2 } if *position == Duration::from_secs(7)
        ));
        assert!(matches!(
            &tail[2],
            PlayerEvent::Tracks {
                active_audio: Some(3),
                generation: 3,
                ..
            }
        ));
        assert!(matches!(&tail[3], PlayerEvent::Snapshot(s) if s.status == "rate"));
        assert!(
            matches!(&tail[4], PlayerEvent::Snapshot(s) if s.status == "volume" && (s.volume - 0.25).abs() < 1e-6)
        );
        assert!(!p.is_playing());
        assert_eq!(p.position(), Duration::from_secs(7));
        assert!((p.volume() - 0.25).abs() < 1e-6);
    }

    /// P1-06 regression: under concurrent producer/consumer polling, a terminal
    /// `Ended` event must NEVER be dequeued ahead of earlier `Snapshot`, `Tracks`,
    /// or `Stream` events.
    #[test]
    fn concurrent_poll_never_reorders_terminal_ended_before_earlier_snapshots() {
        for _ in 0..64 {
            let (mut p, _rx, evt_tx) = make_channel_player();
            let producer = thread::spawn(move || {
                evt_tx.send(PlayerEvent::Snapshot(PlayerSnapshot {
                    playing: true,
                    position: Duration::ZERO,
                    duration: Duration::ZERO,
                    volume: 0.8,
                    generation: 1,
                    status: "opening-native".into(),
                }));
                evt_tx.send(PlayerEvent::Tracks {
                    video_tracks: Vec::new(),
                    audio_tracks: Vec::new(),
                    active_video: None,
                    active_audio: Some(0),
                    generation: 1,
                });
                evt_tx.send(PlayerEvent::Snapshot(PlayerSnapshot {
                    playing: true,
                    position: Duration::ZERO,
                    duration: Duration::from_millis(200),
                    volume: 0.8,
                    generation: 1,
                    status: "playing-native".into(),
                }));
                for i in 0..32u64 {
                    evt_tx.send(PlayerEvent::PositionChanged {
                        position: Duration::from_millis(i * 5),
                        generation: 1,
                    });
                }
                evt_tx.send(PlayerEvent::Ended { generation: 1 });
            });

            let mut events = Vec::new();
            let mut saw_ended = false;
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while !saw_ended && std::time::Instant::now() < deadline {
                if let Some(ev) = p.poll_event() {
                    if matches!(ev, PlayerEvent::Ended { .. }) {
                        saw_ended = true;
                    }
                    events.push(ev);
                } else {
                    thread::yield_now();
                }
            }
            producer.join().expect("producer finished");
            // Drain any remaining events (must be empty after Ended!).
            while let Some(ev) = p.poll_event() {
                events.push(ev);
            }

            assert!(saw_ended, "expected Ended event");
            let ended_idx = events
                .iter()
                .position(|e| matches!(e, PlayerEvent::Ended { .. }))
                .expect("Ended present");
            assert_eq!(
                ended_idx,
                events.len() - 1,
                "Ended must be strictly the final event, never preceding earlier snapshots: {events:?}"
            );
            assert!(
                matches!(&events[0], PlayerEvent::Snapshot(s) if s.status == "opening-native"),
                "first event must be opening-native snapshot, got {:?}",
                events[0]
            );
            assert_eq!(p.state(), PlayerState::Ended);
        }
    }

    /// P1-05 regression: under `Stream` saturation, a single-slot preview frame
    /// emitted after a paused seek must NOT be dropped, and a newer preview
    /// cleanly overwrites any unconsumed older preview while preserving global order.
    #[test]
    fn saturated_stream_delivers_latest_preview_frame_in_order() {
        let (mut p, _rx, evt_tx) = make_channel_player();
        for i in 0..(EVT_CAPACITY + 40) {
            evt_tx.send(PlayerEvent::VideoFrame {
                width: 320,
                height: 180,
                rgba: Arc::new(vec![0; 4]),
                pts: Duration::from_millis(i as u64 * 10),
                generation: 1,
            });
        }
        // First seek + preview (gen 2, 3s)
        evt_tx.send(PlayerEvent::SeekCompleted {
            position: Duration::from_secs(3),
            generation: 2,
        });
        evt_tx.send_preview(PlayerEvent::VideoFrame {
            width: 320,
            height: 180,
            rgba: Arc::new(vec![1; 4]),
            pts: Duration::from_secs(3),
            generation: 2,
        });
        // Second seek + preview (gen 3, 7s) overwrites unconsumed preview slot
        evt_tx.send(PlayerEvent::SeekCompleted {
            position: Duration::from_secs(7),
            generation: 3,
        });
        evt_tx.send_preview(PlayerEvent::VideoFrame {
            width: 160,
            height: 90,
            rgba: Arc::new(vec![2; 4]),
            pts: Duration::from_secs(7),
            generation: 3,
        });

        let (events, latest_frame) = p.poll_all();
        assert_eq!(events.len(), 2, "both SeekCompleted acks must arrive");
        match latest_frame.expect("latest preview frame must be delivered") {
            PlayerEvent::VideoFrame {
                width,
                height,
                pts,
                generation,
                ..
            } => {
                assert_eq!((width, height), (160, 90));
                assert_eq!(pts, Duration::from_secs(7));
                assert_eq!(generation, 3);
            }
            other => panic!("expected VideoFrame, got {other:?}"),
        }
        assert_eq!(p.position(), Duration::from_secs(7));
    }

    #[test]
    fn seek_precise_sends_precise_mode_and_bumps_generation() {
        let (mut p, cmd_rx, _evt_tx) = make_channel_player();
        let g0 = p.generation();
        let target = Duration::from_millis(1250);
        p.seek_precise(target).unwrap();
        assert_eq!(p.generation(), g0 + 1);
        assert_eq!(p.position(), target);
        assert_eq!(p.state(), PlayerState::Seeking);
        match cmd_rx.try_recv().unwrap() {
            PlayerCommand::Seek {
                position,
                mode,
                generation,
            } => {
                assert_eq!(position, target);
                assert_eq!(mode, SeekMode::Precise);
                assert_eq!(generation, g0 + 1);
            }
            other => panic!("expected Seek(Precise), got {other:?}"),
        }
    }

    #[test]
    fn step_forward_sends_command_bumps_generation_and_pauses() {
        let (mut p, cmd_rx, _evt_tx) = make_channel_player();
        let g0 = p.generation();
        assert!(p.is_playing());
        p.step_forward().unwrap();
        assert_eq!(p.generation(), g0 + 1);
        assert!(!p.is_playing());
        assert_eq!(p.state(), PlayerState::Paused);
        match cmd_rx.try_recv().unwrap() {
            PlayerCommand::StepForward { generation } => {
                assert_eq!(generation, g0 + 1);
            }
            other => panic!("expected StepForward, got {other:?}"),
        }

        // Full command queue must not bump generation or mutate state on seek_precise/step_forward.
        for _ in 0..CMD_CAPACITY {
            p.set_volume(0.5).unwrap();
        }
        let gen_before = p.generation();
        let pos_before = p.position();
        let state_before = p.state();
        assert!(matches!(
            p.seek_precise(Duration::from_secs(3)),
            Err(PlayerError::CommandQueueFull)
        ));
        assert!(matches!(
            p.step_forward(),
            Err(PlayerError::CommandQueueFull)
        ));
        assert_eq!(p.generation(), gen_before);
        assert_eq!(p.position(), pos_before);
        assert_eq!(p.state(), state_before);
    }

    #[test]
    fn saturated_stream_delivers_decoder_info_on_control_lane() {
        let (mut p, _rx, evt_tx) = make_channel_player();
        let state_before = p.state();
        let pos_before = p.position();
        let vol_before = p.volume();
        for i in 0..(EVT_CAPACITY + 32) {
            evt_tx.send(PlayerEvent::PositionChanged {
                position: Duration::from_millis(i as u64),
                generation: 1,
            });
        }
        evt_tx.send(PlayerEvent::DecoderInfo {
            demuxer: "rsmpeg-format (flac)".into(),
            video_decoder: None,
            audio_decoder: Some("rsmpeg-rust (FLAC)".into()),
            generation: 1,
        });

        let mut drained = Vec::new();
        while let Some(ev) = p.poll_event() {
            drained.push(ev);
        }
        assert_eq!(drained.len(), EVT_CAPACITY + 1);
        assert!(matches!(
            drained.last(),
            Some(PlayerEvent::DecoderInfo {
                demuxer,
                video_decoder: None,
                audio_decoder: Some(audio),
                generation: 1,
            }) if demuxer == "rsmpeg-format (flac)" && audio == "rsmpeg-rust (FLAC)"
        ));
        // DecoderInfo does not mutate cached Player fields (except earlier PositionChanged).
        assert_eq!(p.state(), state_before);
        assert!((p.volume() - vol_before).abs() < 1e-6);
        assert!(p.position() >= pos_before);
    }

    #[test]
    fn shutdown_rejects_every_control_method_without_mutating_state() {
        let (mut p, cmd_rx, _evt_tx) = make_channel_player();
        assert!(p.shutdown().is_ok(), "first shutdown must succeed");

        // The only command that reaches the worker is the Shutdown itself.
        assert!(matches!(
            cmd_rx.try_recv(),
            Ok(PlayerCommand::Shutdown { .. })
        ));

        let gen_before = p.generation();
        let state_before = p.state();
        let playing_before = p.is_playing();
        let pos_before = p.position();

        assert!(matches!(p.play(), Err(PlayerError::ShutDown)));
        assert!(matches!(p.pause(), Err(PlayerError::ShutDown)));
        assert!(matches!(p.stop(), Err(PlayerError::ShutDown)));
        assert!(matches!(
            p.seek(Duration::from_secs(1)),
            Err(PlayerError::ShutDown)
        ));
        assert!(matches!(
            p.seek_precise(Duration::from_secs(2)),
            Err(PlayerError::ShutDown)
        ));
        assert!(matches!(p.step_forward(), Err(PlayerError::ShutDown)));
        assert!(matches!(p.set_volume(0.3), Err(PlayerError::ShutDown)));
        assert!(matches!(
            p.set_playback_rate(2.0),
            Err(PlayerError::ShutDown)
        ));
        assert!(matches!(
            p.select_audio_track(1),
            Err(PlayerError::ShutDown)
        ));
        assert!(matches!(
            p.select_video_track(0),
            Err(PlayerError::ShutDown)
        ));

        assert_eq!(p.generation(), gen_before, "generation must not advance");
        assert_eq!(p.state(), state_before, "state must not change");
        assert_eq!(p.is_playing(), playing_before);
        assert_eq!(p.position(), pos_before);
        assert!(
            cmd_rx.try_recv().is_err(),
            "rejected controls must not enqueue anything"
        );

        // A second shutdown is idempotent and enqueues nothing further.
        assert!(p.shutdown().is_ok(), "second shutdown must return Ok");
        assert!(cmd_rx.try_recv().is_err());
        assert_eq!(p.generation(), gen_before);
        assert_eq!(p.state(), state_before);
    }

    #[test]
    fn rapid_seek_burst_advances_generation_monotonically_then_saturates() {
        let (mut p, cmd_rx, _evt_tx) = make_channel_player();
        let g0 = p.generation();

        // 50 back-to-back seeks alternate between coarse and precise and must
        // each advance the generation by exactly one.
        for i in 0..50u64 {
            let target = Duration::from_millis(i * 100);
            if i % 2 == 0 {
                p.seek(target).expect("coarse seek within capacity");
            } else {
                p.seek_precise(target)
                    .expect("precise seek within capacity");
            }
            assert_eq!(
                p.generation(),
                g0 + i + 1,
                "seek #{i} must bump generation once"
            );
            assert_eq!(p.position(), target);
            assert_eq!(p.state(), PlayerState::Seeking);
        }

        // Fill the remaining command slots so the queue is exactly at capacity.
        for i in 50..(CMD_CAPACITY as u64) {
            p.seek(Duration::from_millis(i))
                .expect("fill remaining slots");
        }
        let gen_full = p.generation();
        assert_eq!(gen_full, g0 + CMD_CAPACITY as u64);
        let pos_full = p.position();
        let state_full = p.state();

        assert!(matches!(
            p.seek(Duration::from_secs(9)),
            Err(PlayerError::CommandQueueFull)
        ));
        assert!(matches!(
            p.seek_precise(Duration::from_secs(9)),
            Err(PlayerError::CommandQueueFull)
        ));
        assert!(matches!(
            p.step_forward(),
            Err(PlayerError::CommandQueueFull)
        ));
        assert_eq!(
            p.generation(),
            gen_full,
            "rejected seeks must not bump generation"
        );
        assert_eq!(p.position(), pos_full);
        assert_eq!(p.state(), state_full);

        // Every accepted command reached the worker in strict generation order.
        let mut generations = Vec::with_capacity(CMD_CAPACITY);
        while let Ok(cmd) = cmd_rx.try_recv() {
            generations.push(cmd.generation());
        }
        let expected: Vec<u64> = (g0 + 1..=g0 + CMD_CAPACITY as u64).collect();
        assert_eq!(generations, expected);
    }
}
