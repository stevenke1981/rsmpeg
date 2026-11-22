//! Pure UI playback state machine and pixel/layout math.
//!
//! Kept completely independent of GPUI window handles so all transitions,
//! edge cases, and math can be unit-tested headlessly in CI.

use std::collections::VecDeque;
use std::time::Duration;

use rsmpeg_player::{PlayerEvent, MAX_PLAYBACK_RATE, MIN_PLAYBACK_RATE};

use crate::media_info::MediaInfo;
use crate::playlist::Playlist;

/// Preset playback speed steps offered by the UI and `[` / `]` shortcuts.
pub const PLAYBACK_RATES: &[f64] = &[0.5, 0.75, 1.0, 1.25, 1.5, 2.0];

/// Number of historical audio amplitude bars displayed on the audio-only stage.
pub const WAVEFORM_BARS: usize = 48;

/// High-level playback lifecycle displayed in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlaybackStatus {
    #[default]
    Idle,
    Opening,
    Playing,
    Paused,
    Seeking,
    Stopped,
    Ended,
    Error,
}

/// Collapsible right-hand inspector drawer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SidePanel {
    #[default]
    None,
    MediaInfo,
    Playlist,
}

/// Side effect returned by [`PlaybackUiState::apply_player_event`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventOutcome {
    None,
    /// Current track reached natural EOF and playlist has a next track.
    AutoAdvanceNext,
}

/// Complete headless UI state for the media player.
#[derive(Debug, Clone)]
pub struct PlaybackUiState {
    pub status: PlaybackStatus,
    pub position: Duration,
    pub duration: Duration,
    /// Unmuted volume level (`0.0..=1.0`).
    pub volume: f32,
    pub muted: bool,
    pub pre_mute_volume: f32,
    pub playback_rate: f64,
    pub scrubbing: bool,
    pub scrub_fraction: f64,
    pub waveform: VecDeque<f32>,
    pub current_rms: f32,
    pub current_peak: f32,
    pub has_video_frame: bool,
    pub video_unsupported: bool,
    pub video_resolution: Option<(usize, usize)>,
    pub error_message: Option<String>,
    pub warning_message: Option<String>,
    pub active_panel: SidePanel,
    pub is_fullscreen: bool,
    pub is_probing: bool,
    pub playlist: Playlist,
    pub media_info: Option<MediaInfo>,
}

impl Default for PlaybackUiState {
    fn default() -> Self {
        Self::new()
    }
}

impl PlaybackUiState {
    pub fn new() -> Self {
        Self {
            status: PlaybackStatus::Idle,
            position: Duration::ZERO,
            duration: Duration::ZERO,
            volume: 0.8,
            muted: false,
            pre_mute_volume: 0.8,
            playback_rate: 1.0,
            scrubbing: false,
            scrub_fraction: 0.0,
            waveform: VecDeque::from(vec![0.0; WAVEFORM_BARS]),
            current_rms: 0.0,
            current_peak: 0.0,
            has_video_frame: false,
            video_unsupported: false,
            video_resolution: None,
            error_message: None,
            warning_message: None,
            active_panel: SidePanel::None,
            is_fullscreen: false,
            is_probing: false,
            playlist: Playlist::new(),
            media_info: None,
        }
    }

    /// Prepare state for loading a new file.
    pub fn begin_loading(&mut self) {
        self.status = PlaybackStatus::Opening;
        self.position = Duration::ZERO;
        self.duration = Duration::ZERO;
        self.scrubbing = false;
        self.scrub_fraction = 0.0;
        self.has_video_frame = false;
        self.video_unsupported = false;
        self.video_resolution = None;
        self.error_message = None;
        self.warning_message = None;
        self.media_info = None;
        self.is_probing = true;
        self.reset_waveform();
    }

    pub fn reset_waveform(&mut self) {
        self.waveform.clear();
        self.waveform.resize(WAVEFORM_BARS, 0.0);
        self.current_rms = 0.0;
        self.current_peak = 0.0;
    }

    pub fn push_audio_level(&mut self, rms: f32, peak: f32) {
        let rms = rms.clamp(0.0, 1.0);
        let peak = peak.clamp(0.0, 1.0);
        self.current_rms = rms;
        self.current_peak = peak;
        // Blend RMS and peak for a responsive yet smooth bar height.
        let bar = (rms * 0.65 + peak * 0.35).clamp(0.0, 1.0);
        if self.waveform.len() >= WAVEFORM_BARS {
            self.waveform.pop_front();
        }
        self.waveform.push_back(bar);
    }

    /// Effective volume sent to [`rsmpeg_player::Player`].
    pub fn effective_volume(&self) -> f32 {
        if self.muted {
            0.0
        } else {
            self.volume.clamp(0.0, 1.0)
        }
    }

    /// Set volume (`0.0..=1.0`), automatically unmuting if non-zero.
    /// Returns the effective volume to forward to the player core.
    pub fn set_volume(&mut self, volume: f32) -> f32 {
        let v = if volume.is_finite() {
            volume.clamp(0.0, 1.0)
        } else {
            self.volume
        };
        self.volume = v;
        if v > 0.0 {
            self.muted = false;
            self.pre_mute_volume = v;
        } else {
            self.muted = true;
        }
        self.effective_volume()
    }

    /// Adjust volume by `delta` (e.g. `+0.05` / `-0.05`).
    pub fn adjust_volume(&mut self, delta: f32) -> f32 {
        let base = if self.muted { 0.0 } else { self.volume };
        self.set_volume(base + delta)
    }

    /// Toggle mute while remembering the previous non-zero volume.
    pub fn toggle_mute(&mut self) -> f32 {
        if self.muted {
            self.muted = false;
            if self.volume <= 0.001 {
                self.volume = if self.pre_mute_volume > 0.001 {
                    self.pre_mute_volume
                } else {
                    0.5
                };
            }
        } else {
            if self.volume > 0.001 {
                self.pre_mute_volume = self.volume;
            }
            self.muted = true;
        }
        self.effective_volume()
    }

    /// Cycle through [`PLAYBACK_RATES`] (e.g. `1.0 -> 1.25 -> ... -> 2.0 -> 0.5`).
    pub fn cycle_rate(&mut self) -> f64 {
        let next_idx = PLAYBACK_RATES
            .iter()
            .position(|&r| r > self.playback_rate + 0.01)
            .unwrap_or(0);
        self.playback_rate = PLAYBACK_RATES[next_idx];
        self.playback_rate
    }

    /// Step playback rate up to the next preset in [`PLAYBACK_RATES`].
    pub fn step_rate_up(&mut self) -> f64 {
        if let Some(&higher) = PLAYBACK_RATES
            .iter()
            .find(|&&r| r > self.playback_rate + 0.01)
        {
            self.playback_rate = higher;
        }
        self.playback_rate
            .clamp(MIN_PLAYBACK_RATE, MAX_PLAYBACK_RATE)
    }

    /// Step playback rate down to the previous preset in [`PLAYBACK_RATES`].
    pub fn step_rate_down(&mut self) -> f64 {
        if let Some(&lower) = PLAYBACK_RATES
            .iter()
            .rev()
            .find(|&&r| r < self.playback_rate - 0.01)
        {
            self.playback_rate = lower;
        }
        self.playback_rate
            .clamp(MIN_PLAYBACK_RATE, MAX_PLAYBACK_RATE)
    }

    /// Compute clamped target position for a relative seek (`+/- delta_secs`).
    pub fn seek_relative_target(&self, delta_secs: f64) -> Duration {
        let base = self.displayed_position().as_secs_f64();
        let mut target = (base + delta_secs).max(0.0);
        let dur = self.duration.as_secs_f64();
        if dur > 0.0 {
            target = target.min(dur);
        }
        Duration::from_secs_f64(target)
    }

    /// Position shown to the user (follows scrub preview while dragging the seek bar).
    pub fn displayed_position(&self) -> Duration {
        if self.scrubbing && !self.duration.is_zero() {
            Duration::from_secs_f64(
                self.duration.as_secs_f64() * self.scrub_fraction.clamp(0.0, 1.0),
            )
        } else {
            self.position
        }
    }

    /// Progress fraction (`0.0..=1.0`) for rendering the seek bar.
    pub fn progress_fraction(&self) -> f64 {
        if self.scrubbing {
            return self.scrub_fraction.clamp(0.0, 1.0);
        }
        let dur = self.duration.as_secs_f64();
        if dur <= 0.0 {
            0.0
        } else {
            (self.position.as_secs_f64() / dur).clamp(0.0, 1.0)
        }
    }

    /// Toggle a collapsible side panel (clicking the active panel closes it).
    pub fn toggle_panel(&mut self, panel: SidePanel) {
        if self.active_panel == panel {
            self.active_panel = SidePanel::None;
        } else {
            self.active_panel = panel;
        }
    }

    /// Whether the stage should show the audio-only visualizer instead of video.
    pub fn is_audio_only_stage(&self) -> bool {
        if self.has_video_frame {
            return false;
        }
        self.video_unsupported
            || self
                .media_info
                .as_ref()
                .is_some_and(MediaInfo::is_audio_only)
    }

    /// Apply a [`PlayerEvent`] emitted by `rsmpeg-player`.
    pub fn apply_player_event(&mut self, ev: &PlayerEvent) -> EventOutcome {
        match ev {
            PlayerEvent::Snapshot(snap) => {
                if !snap.duration.is_zero() {
                    self.duration = snap.duration;
                    if let Some(info) = self.media_info.as_mut() {
                        info.update_duration_if_missing(snap.duration);
                    }
                }
                if !self.scrubbing {
                    self.position = snap.position;
                }
                if snap.playing {
                    self.status = PlaybackStatus::Playing;
                } else if snap.status == "paused" {
                    self.status = PlaybackStatus::Paused;
                } else if snap.status == "stopped" {
                    self.status = PlaybackStatus::Stopped;
                } else if snap.status.starts_with("opening") {
                    self.status = PlaybackStatus::Opening;
                }
                EventOutcome::None
            }
            PlayerEvent::PositionChanged { position, .. } => {
                if !self.scrubbing {
                    self.position = *position;
                }
                EventOutcome::None
            }
            PlayerEvent::VideoFrame {
                width, height, pts, ..
            } => {
                self.has_video_frame = true;
                self.video_resolution = Some((*width, *height));
                if !self.scrubbing {
                    self.position = *pts;
                }
                if self.status == PlaybackStatus::Opening {
                    self.status = PlaybackStatus::Playing;
                }
                EventOutcome::None
            }
            PlayerEvent::SeekCompleted { position, .. } => {
                if !self.scrubbing {
                    self.position = *position;
                }
                if self.status == PlaybackStatus::Seeking {
                    self.status = PlaybackStatus::Playing;
                }
                EventOutcome::None
            }
            PlayerEvent::Ended { .. } => {
                self.status = PlaybackStatus::Ended;
                if !self.duration.is_zero() {
                    self.position = self.duration;
                }
                if self.playlist.has_next() {
                    EventOutcome::AutoAdvanceNext
                } else {
                    EventOutcome::None
                }
            }
            PlayerEvent::Error { message, .. } => {
                self.status = PlaybackStatus::Error;
                self.error_message = Some(localize_player_error(message));
                EventOutcome::None
            }
            PlayerEvent::Warning { message, .. } => {
                if message.contains("Unsupported video codec")
                    || message.contains("failed to open OpenH264")
                {
                    self.video_unsupported = true;
                }
                // Keep internal pipeline info warnings quiet; surface codec/decoder issues.
                if !message.starts_with("using native demux")
                    && !message.starts_with("native pipeline unavailable")
                {
                    self.warning_message = Some(localize_player_warning(message));
                }
                EventOutcome::None
            }
            PlayerEvent::AudioLevel { rms, peak, .. } => {
                self.push_audio_level(*rms, *peak);
                EventOutcome::None
            }
        }
    }
}

/// Map a horizontal pointer position onto `[0.0, 1.0]` inside a slider track.
pub fn fraction_from_pointer(mouse_x: f32, track_left: f32, track_width: f32) -> f64 {
    if !track_width.is_finite() || track_width <= 0.0 {
        return 0.0;
    }
    f64::from(((mouse_x - track_left) / track_width).clamp(0.0, 1.0))
}

/// Convert packed RGBA8 pixels into BGRA8 pixel order expected by `gpui::RenderImage`.
pub fn rgba_to_bgra_bytes(rgba: &[u8]) -> Vec<u8> {
    let mut bgra = Vec::with_capacity(rgba.len());
    for chunk in rgba.as_chunks::<4>().0 {
        bgra.push(chunk[2]); // B
        bgra.push(chunk[1]); // G
        bgra.push(chunk[0]); // R
        bgra.push(chunk[3]); // A
    }
    bgra
}

/// Compute aspect-ratio-preserving fitted dimensions inside a viewport box.
pub fn compute_aspect_fit(src_w: f32, src_h: f32, dst_w: f32, dst_h: f32) -> (f32, f32) {
    if src_w <= 0.0 || src_h <= 0.0 || dst_w <= 0.0 || dst_h <= 0.0 {
        return (0.0, 0.0);
    }
    let scale = (dst_w / src_w).min(dst_h / src_h);
    (src_w * scale, src_h * scale)
}

/// Convert raw engine errors into clear Traditional Chinese messages while preserving technical detail.
pub fn localize_player_error(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("no such file")
        || lower.contains("cannot find the file")
        || lower.contains("not found")
        || lower.contains("os error 2")
    {
        format!("找不到指定的媒體檔案（{raw}）")
    } else if lower.contains("unsupported")
        || lower.contains("no playable")
        || lower.contains("no tracks")
    {
        format!("不支援此媒體格式或無可播放串流（{raw}）")
    } else if lower.contains("eof")
        || lower.contains("corrupt")
        || lower.contains("invalid")
        || lower.contains("malformed")
    {
        format!("檔案格式無效或已損毀（{raw}）")
    } else {
        format!("播放失敗：{raw}")
    }
}

/// Format non-fatal engine warnings into user-friendly Traditional Chinese text.
pub fn localize_player_warning(raw: &str) -> String {
    if raw.contains("Unsupported video codec") {
        format!("視訊編碼不受支援，已改以純音訊模式播放（{raw}）")
    } else if raw.contains("audio muted") {
        format!("音訊解碼器不可用，將以靜音模式播放（{raw}）")
    } else {
        raw.to_string()
    }
}

/// Expand dropped paths (files + 1-level directories of supported media) off the UI thread.
pub fn expand_dropped_paths(paths: Vec<std::path::PathBuf>) -> Vec<std::path::PathBuf> {
    use crate::playlist::is_supported_media_extension;

    let mut result = Vec::new();
    for path in paths {
        if path.is_file() {
            result.push(path);
        } else if path.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&path) {
                let mut child_media: Vec<std::path::PathBuf> = entries
                    .filter_map(Result::ok)
                    .map(|e| e.path())
                    .filter(|p| p.is_file() && is_supported_media_extension(p))
                    .collect();
                child_media.sort();
                result.extend(child_media);
            }
        }
    }
    result.sort_by_key(|p| !is_supported_media_extension(p));
    result
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use rsmpeg_player::PlayerSnapshot;

    use super::*;

    #[test]
    fn volume_and_mute_preserve_previous_level() {
        let mut st = PlaybackUiState::new();
        assert!((st.effective_volume() - 0.8).abs() < 1e-6);

        assert_eq!(st.toggle_mute(), 0.0);
        assert!(st.muted);

        assert!((st.toggle_mute() - 0.8).abs() < 1e-6);
        assert!(!st.muted);

        // Muting at zero restores sensible 0.5 fallback when unmuted.
        st.set_volume(0.0);
        st.pre_mute_volume = 0.0;
        assert!((st.toggle_mute() - 0.5).abs() < 1e-6);

        // Adjusting up from muted unmutes smoothly.
        st.toggle_mute();
        assert!((st.adjust_volume(0.05) - 0.05).abs() < 1e-6);
        assert!(!st.muted);
    }

    #[test]
    fn playback_rate_cycles_and_steps_within_bounds() {
        let mut st = PlaybackUiState::new();
        assert!((st.playback_rate - 1.0).abs() < 1e-6);
        assert!((st.step_rate_up() - 1.25).abs() < 1e-6);
        assert!((st.step_rate_up() - 1.5).abs() < 1e-6);
        assert!((st.step_rate_up() - 2.0).abs() < 1e-6);
        // Saturates at highest preset.
        assert!((st.step_rate_up() - 2.0).abs() < 1e-6);
        // Cycling wraps back to 0.5x.
        assert!((st.cycle_rate() - 0.5).abs() < 1e-6);
        // Step down saturates at lowest preset.
        assert!((st.step_rate_down() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn seek_relative_clamps_to_duration_bounds() {
        let mut st = PlaybackUiState::new();
        st.duration = Duration::from_secs(60);
        st.position = Duration::from_secs(3);

        assert_eq!(st.seek_relative_target(-5.0), Duration::ZERO);
        assert_eq!(st.seek_relative_target(5.0), Duration::from_secs(8));

        st.position = Duration::from_secs(58);
        assert_eq!(st.seek_relative_target(5.0), Duration::from_secs(60));
    }

    #[test]
    fn scrubbing_overrides_displayed_position_until_released() {
        let mut st = PlaybackUiState::new();
        st.duration = Duration::from_secs(100);
        st.position = Duration::from_secs(20);
        st.scrubbing = true;
        st.scrub_fraction = 0.75;

        assert_eq!(st.displayed_position(), Duration::from_secs(75));
        assert!((st.progress_fraction() - 0.75).abs() < 1e-6);

        // Incoming player position updates must not fight the user's drag thumb.
        st.apply_player_event(&PlayerEvent::PositionChanged {
            position: Duration::from_secs(21),
            generation: 1,
        });
        assert_eq!(st.displayed_position(), Duration::from_secs(75));
    }

    #[test]
    fn ended_event_triggers_auto_advance_when_playlist_has_next() {
        let mut st = PlaybackUiState::new();
        st.playlist
            .add_paths([PathBuf::from("one.wav"), PathBuf::from("two.wav")]);
        st.playlist.select(0);
        st.duration = Duration::from_secs(10);

        let outcome = st.apply_player_event(&PlayerEvent::Ended { generation: 1 });
        assert_eq!(outcome, EventOutcome::AutoAdvanceNext);
        assert_eq!(st.status, PlaybackStatus::Ended);

        st.playlist.select(1);
        let outcome_last = st.apply_player_event(&PlayerEvent::Ended { generation: 2 });
        assert_eq!(outcome_last, EventOutcome::None);
    }

    #[test]
    fn waveform_ring_buffer_stays_bounded() {
        let mut st = PlaybackUiState::new();
        for i in 0..(WAVEFORM_BARS * 3) {
            let level = (i % 10) as f32 / 10.0;
            st.apply_player_event(&PlayerEvent::AudioLevel {
                rms: level,
                peak: level,
                generation: 1,
            });
        }
        assert_eq!(st.waveform.len(), WAVEFORM_BARS);
    }

    #[test]
    fn error_and_video_events_update_state_cleanly() {
        let mut st = PlaybackUiState::new();
        st.apply_player_event(&PlayerEvent::Snapshot(PlayerSnapshot {
            playing: true,
            position: Duration::from_secs(1),
            duration: Duration::from_secs(30),
            volume: 0.8,
            generation: 1,
            status: "playing".into(),
        }));
        assert_eq!(st.status, PlaybackStatus::Playing);
        assert_eq!(st.duration, Duration::from_secs(30));

        st.apply_player_event(&PlayerEvent::VideoFrame {
            width: 1280,
            height: 720,
            rgba: Arc::new(vec![0; 4]),
            pts: Duration::from_secs(2),
            generation: 1,
        });
        assert!(st.has_video_frame);
        assert_eq!(st.video_resolution, Some((1280, 720)));

        st.apply_player_event(&PlayerEvent::Error {
            message: "unsupported codec".into(),
            generation: 1,
        });
        assert_eq!(st.status, PlaybackStatus::Error);
        assert!(
            st.error_message
                .as_deref()
                .is_some_and(|m| m.contains("unsupported codec") && m.contains("不支援")),
            "got {:?}",
            st.error_message
        );
    }

    #[test]
    fn unsupported_video_warning_switches_stage_to_audio_only() {
        let mut st = PlaybackUiState::new();
        assert!(!st.is_audio_only_stage());

        st.apply_player_event(&PlayerEvent::Warning {
            message: "Unsupported video codec 'hevc' — audio only".into(),
            generation: 1,
        });
        assert!(st.video_unsupported);
        assert!(st.is_audio_only_stage());
        assert!(st
            .warning_message
            .as_deref()
            .is_some_and(|m| m.contains("純音訊模式")));
    }

    #[test]
    fn localize_player_error_categorizes_common_failures() {
        assert!(localize_player_error("os error 2: not found").contains("找不到"));
        assert!(localize_player_error("No playable audio tracks").contains("不支援"));
        assert!(localize_player_error("unexpected EOF in header").contains("損毀"));
        assert!(localize_player_error("custom failure").contains("播放失敗"));
    }

    #[test]
    fn expand_dropped_paths_scans_directory_and_prioritizes_media() {
        let dir = std::env::temp_dir().join("rsmpeg_gui_drop_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a_txt = dir.join("notes.txt");
        let b_wav = dir.join("clip.wav");
        let c_mp4 = dir.join("demo.mp4");
        std::fs::write(&a_txt, b"text").unwrap();
        std::fs::write(&b_wav, b"wav").unwrap();
        std::fs::write(&c_mp4, b"mp4").unwrap();

        // Direct drop of txt + directory containing wav & mp4 -> media files sorted first.
        let expanded = expand_dropped_paths(vec![a_txt.clone(), dir.clone()]);
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(expanded.len(), 3);
        assert_eq!(expanded[0], b_wav);
        assert_eq!(expanded[1], c_mp4);
        assert_eq!(expanded[2], a_txt);
    }

    #[test]
    fn rgba_to_bgra_swaps_red_and_blue_channels() {
        let rgba = [10u8, 20, 30, 255, 200, 100, 50, 128];
        let bgra = rgba_to_bgra_bytes(&rgba);
        assert_eq!(bgra, vec![30, 20, 10, 255, 50, 100, 200, 128]);
    }

    #[test]
    fn pointer_fraction_and_aspect_fit_math() {
        assert!((fraction_from_pointer(50.0, 0.0, 200.0) - 0.25).abs() < 1e-6);
        assert_eq!(fraction_from_pointer(-10.0, 0.0, 200.0), 0.0);
        assert_eq!(fraction_from_pointer(250.0, 0.0, 200.0), 1.0);
        assert_eq!(fraction_from_pointer(50.0, 0.0, 0.0), 0.0);

        let (w, h) = compute_aspect_fit(1920.0, 1080.0, 960.0, 600.0);
        assert!((w - 960.0).abs() < 1e-3);
        assert!((h - 540.0).abs() < 1e-3);

        let (w2, h2) = compute_aspect_fit(1080.0, 1920.0, 960.0, 600.0);
        assert!((h2 - 600.0).abs() < 1e-3);
        assert!((w2 - 337.5).abs() < 1e-3);
    }

    #[test]
    fn toggle_panel_switches_and_closes() {
        let mut st = PlaybackUiState::new();
        assert_eq!(st.active_panel, SidePanel::None);
        st.toggle_panel(SidePanel::MediaInfo);
        assert_eq!(st.active_panel, SidePanel::MediaInfo);
        st.toggle_panel(SidePanel::Playlist);
        assert_eq!(st.active_panel, SidePanel::Playlist);
        st.toggle_panel(SidePanel::Playlist);
        assert_eq!(st.active_panel, SidePanel::None);
    }
}
