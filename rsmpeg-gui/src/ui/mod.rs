//! Main GPUI view hosting the `rsmpeg-player` session, polling loop, and UI composition.

pub mod controls;
pub mod panels;
pub mod stage;
pub mod theme;

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    div, prelude::*, px, Bounds, Context, ExternalPaths, FocusHandle, Focusable, IntoElement,
    KeyDownEvent, MouseButton, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Render,
    RenderImage, ScrollDelta, ScrollWheelEvent, Styled, Task, Window,
};
use rsmpeg_player::{Player, PlayerEvent};

use crate::format_time::{format_duration, format_rate, format_volume_percent};
use crate::i18n::{strings, Locale, UiStrings};
use crate::media_info::probe_file;
use crate::playlist::{RemoveOutcome, RepeatMode, MEDIA_EXTENSIONS, SUBTITLE_EXTENSIONS};
use crate::shortcuts::{
    map_key_to_action, KeyInput, ShortcutAction, SEEK_FINE_STEP_SECS, SEEK_LONG_STEP_SECS,
    SEEK_STEP_SECS, VOLUME_STEP,
};
use crate::state::{
    expand_dropped_items, expand_dropped_paths, fraction_from_pointer, AspectMode, EventOutcome,
    PlaybackStatus, PlaybackUiState, SidePanel, SubtitleSize, TrackMenu,
};
use crate::video::{build_render_image_transformed, save_screenshot_png};

const POLL_INTERVAL: Duration = Duration::from_millis(16);
const AB_LOOP_COOLDOWN: Duration = Duration::from_millis(250);

pub(crate) fn repeat_mode_label(s: &UiStrings, mode: RepeatMode) -> &'static str {
    match mode {
        RepeatMode::Off => s.repeat_off,
        RepeatMode::All => s.repeat_all,
        RepeatMode::One => s.repeat_one,
    }
}

pub(crate) fn aspect_mode_label(s: &UiStrings, mode: AspectMode) -> &'static str {
    match mode {
        AspectMode::Contain => s.aspect_contain,
        AspectMode::Cover => s.aspect_cover,
        AspectMode::Fill => s.aspect_fill,
    }
}

pub(crate) fn subtitle_size_label(s: &UiStrings, size: SubtitleSize) -> &'static str {
    match size {
        SubtitleSize::Small => s.subtitle_size_small,
        SubtitleSize::Medium => s.subtitle_size_medium,
        SubtitleSize::Large => s.subtitle_size_large,
    }
}

/// Root GPUI view for `rsmpeg-gui`.
pub struct PlayerView {
    pub(crate) state: PlaybackUiState,
    focus_handle: FocusHandle,
    player: Option<Player>,
    current_frame: Option<Arc<RenderImage>>,
    last_raw_frame: Option<(usize, usize, Arc<Vec<u8>>)>,
    ab_loop_last_jump: Option<Instant>,
    /// Incremented on each file load so stale background probes/subtitles are ignored.
    load_nonce: u64,
    _poll_task: Task<()>,
    probe_task: Option<Task<()>>,
    subtitle_task: Option<Task<()>>,
    screenshot_task: Option<Task<()>>,
    dialog_task: Option<Task<()>>,
    seek_track_bounds: Rc<Cell<Bounds<Pixels>>>,
    volume_track_bounds: Rc<Cell<Bounds<Pixels>>>,
    dragging_volume: bool,
}

impl Focusable for PlayerView {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Drop for PlayerView {
    fn drop(&mut self) {
        if let Some(mut p) = self.player.take() {
            let _ = p.shutdown();
        }
    }
}

impl PlayerView {
    pub fn new(
        initial_files: Vec<PathBuf>,
        initial_locale: Locale,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window);

        let poll_task = cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor().timer(POLL_INTERVAL).await;
            let res = this.update_in(cx, |view, window, cx| {
                view.poll_player(window, cx);
            });
            if res.is_err() {
                break;
            }
        });

        let mut state = PlaybackUiState::new();
        state.set_locale(initial_locale);

        let mut view = Self {
            state,
            focus_handle,
            player: None,
            current_frame: None,
            last_raw_frame: None,
            ab_loop_last_jump: None,
            load_nonce: 0,
            _poll_task: poll_task,
            probe_task: None,
            subtitle_task: None,
            screenshot_task: None,
            dialog_task: None,
            seek_track_bounds: Rc::new(Cell::new(Bounds::default())),
            volume_track_bounds: Rc::new(Cell::new(Bounds::default())),
            dragging_volume: false,
        };

        if !initial_files.is_empty() {
            view.enqueue_and_play(initial_files, true, window, cx);
        }

        view
    }

    pub(crate) fn strings(&self) -> &'static UiStrings {
        strings(self.state.locale)
    }

    pub(crate) fn toggle_locale(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.toggle_locale();
        self.sync_window_title(window);
        cx.notify();
    }

    fn sync_window_title(&self, window: &mut Window) {
        let title = self
            .state
            .playlist
            .current_item()
            .map(|it| format!("{} — {}", it.display_name, self.strings().app_title))
            .unwrap_or_else(|| self.strings().app_title.to_string());
        window.set_window_title(&title);
    }

    pub(crate) fn focus_root(&self, window: &mut Window) {
        self.focus_handle.focus(window);
    }

    pub(crate) fn dismiss_banner(&mut self, is_error: bool, cx: &mut Context<Self>) {
        if is_error {
            self.state.clear_error();
        } else {
            self.state.clear_warning();
        }
        cx.notify();
    }

    pub(crate) fn toggle_fullscreen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.toggle_fullscreen();
        self.state.is_fullscreen = window.is_fullscreen();
        cx.notify();
    }

    pub(crate) fn toggle_play_pause(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.state.status {
            PlaybackStatus::Playing | PlaybackStatus::Opening => {
                if let Some(p) = self.player.as_mut() {
                    if let Err(err) = p.pause() {
                        self.state.report_command_error(&err);
                    }
                    cx.notify();
                } else {
                    self.state.status = PlaybackStatus::Paused;
                    cx.notify();
                }
            }
            PlaybackStatus::Paused | PlaybackStatus::Seeking => {
                if let Some(p) = self.player.as_mut() {
                    if let Err(err) = p.play() {
                        self.state.report_command_error(&err);
                    }
                    cx.notify();
                } else if let Some(item) = self.state.playlist.current_item().cloned() {
                    let resume_at = if self.state.position.is_zero() {
                        None
                    } else {
                        Some(self.state.position)
                    };
                    self.load_path(item.path, true, resume_at, window, cx);
                }
            }
            PlaybackStatus::Stopped | PlaybackStatus::Ended | PlaybackStatus::Error => {
                if let Some(item) = self.state.playlist.current_item().cloned() {
                    self.load_path(item.path, true, None, window, cx);
                } else {
                    self.open_file_dialog(window, cx);
                }
            }
            PlaybackStatus::Idle => {
                if let Some(item) = self.state.playlist.current_item().cloned() {
                    self.load_path(item.path, true, None, window, cx);
                } else {
                    self.open_file_dialog(window, cx);
                }
            }
        }
    }

    pub(crate) fn stop_playback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(mut p) = self.player.take() {
            let _ = p.stop();
            let _ = p.shutdown();
        }
        if let Some(old_img) = self.current_frame.take() {
            let _ = window.drop_image(old_img);
        }
        self.last_raw_frame = None;
        self.ab_loop_last_jump = None;
        self.state.has_video_frame = false;
        self.state.position = Duration::ZERO;
        self.state.scrubbing = false;
        self.state.scrub_fraction = 0.0;
        self.state.seek_hover_fraction = None;
        self.state.reset_waveform();
        self.state.status = if self.state.playlist.current_item().is_some() {
            PlaybackStatus::Stopped
        } else {
            PlaybackStatus::Idle
        };
        cx.notify();
    }

    pub(crate) fn seek_to(
        &mut self,
        target: Duration,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.seek_to_mode(target, false, window, cx);
    }

    pub(crate) fn seek_to_mode(
        &mut self,
        target: Duration,
        precise: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let clamped = if !self.state.duration.is_zero() {
            target.min(self.state.duration)
        } else {
            target
        };

        if let Some(p) = self.player.as_mut() {
            let res = if precise {
                p.seek_precise(clamped)
            } else {
                p.seek(clamped)
            };
            if let Err(err) = res {
                self.state.report_command_error(&err);
            }
            cx.notify();
        } else if let Some(item) = self.state.playlist.current_item().cloned() {
            let autoplay = self.state.status == PlaybackStatus::Playing;
            self.load_path(item.path, autoplay, Some(clamped), window, cx);
        }
    }

    pub(crate) fn seek_relative_mode(
        &mut self,
        delta_secs: f64,
        precise: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state.playlist.current_item().is_none() {
            return;
        }
        let target = self.state.seek_relative_target(delta_secs);
        self.state.set_osd(format!(
            "{} / {}",
            format_duration(target),
            format_duration(self.state.duration)
        ));
        self.seek_to_mode(target, precise, window, cx);
    }

    pub(crate) fn begin_seek_drag(&mut self, mouse_x: Pixels, cx: &mut Context<Self>) {
        if self.state.duration.is_zero() {
            return;
        }
        let bounds = self.seek_track_bounds.get();
        let frac = fraction_from_pointer(
            f32::from(mouse_x),
            f32::from(bounds.origin.x),
            f32::from(bounds.size.width),
        );
        self.state.scrubbing = true;
        self.state.scrub_fraction = frac;
        cx.notify();
    }

    pub(crate) fn update_seek_drag(&mut self, mouse_x: Pixels, cx: &mut Context<Self>) {
        if !self.state.scrubbing {
            return;
        }
        let bounds = self.seek_track_bounds.get();
        let frac = fraction_from_pointer(
            f32::from(mouse_x),
            f32::from(bounds.origin.x),
            f32::from(bounds.size.width),
        );
        self.state.scrub_fraction = frac;
        cx.notify();
    }

    pub(crate) fn finish_seek_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.state.scrubbing {
            return;
        }
        let target = self.state.displayed_position();
        self.state.scrubbing = false;
        self.seek_to(target, window, cx);
    }

    pub(crate) fn update_seek_hover(&mut self, mouse_x: Pixels, cx: &mut Context<Self>) {
        if self.state.duration.is_zero() {
            if self.state.seek_hover_fraction.take().is_some() {
                cx.notify();
            }
            return;
        }
        let bounds = self.seek_track_bounds.get();
        let frac = fraction_from_pointer(
            f32::from(mouse_x),
            f32::from(bounds.origin.x),
            f32::from(bounds.size.width),
        );
        self.state.seek_hover_fraction = Some(frac);
        cx.notify();
    }

    pub(crate) fn clear_seek_hover(&mut self, cx: &mut Context<Self>) {
        if self.state.seek_hover_fraction.take().is_some() {
            cx.notify();
        }
    }

    pub(crate) fn begin_volume_drag(&mut self, mouse_x: Pixels, cx: &mut Context<Self>) {
        self.dragging_volume = true;
        self.apply_volume_from_x(mouse_x, cx);
    }

    fn apply_volume_from_x(&mut self, mouse_x: Pixels, cx: &mut Context<Self>) {
        let bounds = self.volume_track_bounds.get();
        let frac = fraction_from_pointer(
            f32::from(mouse_x),
            f32::from(bounds.origin.x),
            f32::from(bounds.size.width),
        );
        let intent = self.state.preview_volume(frac as f32);
        if let Some(p) = self.player.as_mut() {
            match p.set_volume(intent.effective) {
                Ok(()) => self.state.queue_volume_intent(intent),
                Err(err) => self.state.report_command_error(&err),
            }
        } else {
            self.state.apply_volume_intent(intent);
        }
        cx.notify();
    }

    pub(crate) fn adjust_volume(&mut self, delta: f32, cx: &mut Context<Self>) {
        let intent = self.state.preview_adjust_volume(delta);
        let mut ok = true;
        if let Some(p) = self.player.as_mut() {
            match p.set_volume(intent.effective) {
                Ok(()) => self.state.queue_volume_intent(intent),
                Err(err) => {
                    self.state.report_command_error(&err);
                    ok = false;
                }
            }
        } else {
            self.state.apply_volume_intent(intent);
        }
        if ok {
            self.state.set_osd(format!(
                "{}: {}",
                self.strings().volume_osd,
                format_volume_percent(intent.effective)
            ));
        }
        cx.notify();
    }

    pub(crate) fn handle_scroll_wheel(&mut self, ev: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let dy = match ev.delta {
            ScrollDelta::Lines(pt) => pt.y,
            ScrollDelta::Pixels(pt) => f32::from(pt.y) / 20.0,
        };
        if dy.abs() < 0.01 {
            return;
        }
        let step = if dy > 0.0 { VOLUME_STEP } else { -VOLUME_STEP };
        self.adjust_volume(step, cx);
    }

    pub(crate) fn toggle_mute(&mut self, cx: &mut Context<Self>) {
        let intent = self.state.preview_toggle_mute();
        let mut ok = true;
        if let Some(p) = self.player.as_mut() {
            match p.set_volume(intent.effective) {
                Ok(()) => self.state.queue_volume_intent(intent),
                Err(err) => {
                    self.state.report_command_error(&err);
                    ok = false;
                }
            }
        } else {
            self.state.apply_volume_intent(intent);
        }
        if ok {
            let msg = if intent.muted {
                self.strings().mute.to_string()
            } else {
                format!(
                    "{}: {}",
                    self.strings().volume_osd,
                    format_volume_percent(intent.effective)
                )
            };
            self.state.set_osd(msg);
        }
        cx.notify();
    }

    fn apply_playback_rate(&mut self, rate: f64, cx: &mut Context<Self>) {
        let mut ok = true;
        if let Some(p) = self.player.as_mut() {
            match p.set_playback_rate(rate) {
                Ok(()) => self.state.queue_playback_rate(rate),
                Err(err) => {
                    self.state.report_command_error(&err);
                    ok = false;
                }
            }
        } else {
            self.state.playback_rate = rate;
        }
        if ok {
            self.state
                .set_osd(format!("{}: {}", self.strings().speed, format_rate(rate)));
        }
        cx.notify();
    }

    pub(crate) fn cycle_playback_rate(&mut self, cx: &mut Context<Self>) {
        let rate = self.state.next_cycle_rate();
        self.apply_playback_rate(rate, cx);
    }

    pub(crate) fn step_playback_rate(&mut self, up: bool, cx: &mut Context<Self>) {
        let rate = if up {
            self.state.next_step_rate_up()
        } else {
            self.state.next_step_rate_down()
        };
        self.apply_playback_rate(rate, cx);
    }

    pub(crate) fn reset_playback_rate(&mut self, cx: &mut Context<Self>) {
        self.apply_playback_rate(1.0, cx);
    }

    pub(crate) fn step_frame_forward(&mut self, cx: &mut Context<Self>) {
        if let Some(p) = self.player.as_mut() {
            match p.step_forward() {
                Ok(()) => {
                    self.state.status = PlaybackStatus::Paused;
                    self.state
                        .set_osd(self.strings().step_frame_osd.to_string());
                }
                Err(err) => self.state.report_command_error(&err),
            }
            cx.notify();
        }
    }

    pub(crate) fn cycle_repeat_mode(&mut self, cx: &mut Context<Self>) {
        self.state.repeat_mode = self.state.repeat_mode.cycle();
        let label = repeat_mode_label(self.strings(), self.state.repeat_mode);
        self.state.set_osd(label.to_string());
        cx.notify();
    }

    pub(crate) fn toggle_shuffle(&mut self, cx: &mut Context<Self>) {
        self.state.shuffle = !self.state.shuffle;
        let label = if self.state.shuffle {
            self.strings().shuffle_on
        } else {
            self.strings().shuffle_off
        };
        self.state.set_osd(label.to_string());
        cx.notify();
    }

    pub(crate) fn cycle_ab_loop(&mut self, cx: &mut Context<Self>) {
        let pos = self.state.displayed_position();
        self.state.ab_loop.cycle_at(pos);
        let s = self.strings();
        let msg = match (self.state.ab_loop.point_a, self.state.ab_loop.point_b) {
            (Some(a), None) => format!("{}: {}", s.ab_loop_set_a, format_duration(a)),
            (Some(a), Some(b)) => format!(
                "{}: {} – {}",
                s.ab_loop_set_b,
                format_duration(a),
                format_duration(b)
            ),
            _ => s.ab_loop_clear.to_string(),
        };
        self.state.set_osd(msg);
        cx.notify();
    }

    pub(crate) fn clear_ab_loop(&mut self, cx: &mut Context<Self>) {
        self.state.ab_loop.clear();
        self.state.set_osd(self.strings().ab_loop_clear.to_string());
        cx.notify();
    }

    pub(crate) fn cycle_aspect_mode(&mut self, cx: &mut Context<Self>) {
        self.state.aspect_mode = self.state.aspect_mode.cycle();
        let s = self.strings();
        self.state.set_osd(format!(
            "{}: {}",
            s.aspect_mode,
            aspect_mode_label(s, self.state.aspect_mode)
        ));
        cx.notify();
    }

    fn refresh_transformed_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((w, h, ref rgba)) = self.last_raw_frame {
            if let Some(next_img) =
                build_render_image_transformed(w, h, rgba, self.state.video_transform)
            {
                if let Some(old_img) = self.current_frame.replace(next_img) {
                    let _ = window.drop_image(old_img);
                }
            }
        }
        cx.notify();
    }

    pub(crate) fn rotate_video(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.video_transform.rotate_cw();
        self.state.set_osd(format!(
            "{}: {}°",
            self.strings().rotate_btn,
            self.state.video_transform.rotation_deg
        ));
        self.refresh_transformed_frame(window, cx);
    }

    pub(crate) fn toggle_mirror(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.video_transform.mirror_horizontal =
            !self.state.video_transform.mirror_horizontal;
        let s = self.strings();
        let state_txt = if self.state.video_transform.mirror_horizontal {
            s.subtitle_on
        } else {
            s.subtitle_off
        };
        self.state.set_osd(format!("{}: {state_txt}", s.mirror_btn));
        self.refresh_transformed_frame(window, cx);
    }

    pub(crate) fn toggle_grayscale(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.video_transform.grayscale = !self.state.video_transform.grayscale;
        let s = self.strings();
        let state_txt = if self.state.video_transform.grayscale {
            s.subtitle_on
        } else {
            s.subtitle_off
        };
        self.state
            .set_osd(format!("{}: {state_txt}", s.grayscale_btn));
        self.refresh_transformed_frame(window, cx);
    }

    pub(crate) fn adjust_brightness(
        &mut self,
        delta: i16,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.video_transform.adjust_brightness(delta);
        self.state.set_osd(format!(
            "{}: {:+}",
            self.strings().brightness_label,
            self.state.video_transform.brightness
        ));
        self.refresh_transformed_frame(window, cx);
    }

    pub(crate) fn adjust_contrast(
        &mut self,
        delta: i16,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.video_transform.adjust_contrast(delta);
        self.state.set_osd(format!(
            "{}: {}%",
            self.strings().contrast_label,
            self.state.video_transform.contrast_pct
        ));
        self.refresh_transformed_frame(window, cx);
    }

    pub(crate) fn reset_video_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.video_transform.reset();
        self.state
            .set_osd(self.strings().reset_filters_btn.to_string());
        self.refresh_transformed_frame(window, cx);
    }

    pub(crate) fn take_screenshot(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((w, h, rgba)) = self.last_raw_frame.clone() else {
            self.state
                .set_osd(self.strings().screenshot_failed.to_string());
            cx.notify();
            return;
        };

        let secs = self.state.displayed_position().as_secs();
        let mins = secs / 60;
        let rem_secs = secs % 60;
        let timestamp = format!("{mins:02}m{rem_secs:02}s");
        let (dir, stem) = self
            .state
            .playlist
            .current_item()
            .map(|it| {
                let parent = it
                    .path
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| PathBuf::from("."));
                let stem = it
                    .path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .filter(|s| !s.is_empty())
                    .unwrap_or("rsmpeg")
                    .to_string();
                (parent, stem)
            })
            .unwrap_or_else(|| (PathBuf::from("."), "rsmpeg".to_string()));
        let transform = self.state.video_transform;

        self.screenshot_task = Some(cx.spawn_in(window, async move |this, cx| {
            let (file_name, res) = cx
                .background_executor()
                .spawn(async move {
                    let mut out_path = dir.join(format!("{stem}_{timestamp}.png"));
                    if out_path.exists() {
                        for suffix in 1..=99u32 {
                            let candidate = dir.join(format!("{stem}_{timestamp}_{suffix}.png"));
                            if !candidate.exists() {
                                out_path = candidate;
                                break;
                            }
                        }
                    }
                    let file_name = out_path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("screenshot.png")
                        .to_string();
                    let res = save_screenshot_png(&out_path, w, h, &rgba, transform);
                    (file_name, res)
                })
                .await;

            let _ = this.update_in(cx, |view, _window, cx| {
                let msg = match res {
                    Ok(()) => format!("{}: {file_name}", view.strings().screenshot_saved),
                    Err(err) => format!("{}: {err}", view.strings().screenshot_failed),
                };
                view.state.set_osd(msg);
                cx.notify();
            });
        }));
    }

    pub(crate) fn toggle_subtitles(&mut self, cx: &mut Context<Self>) {
        self.state.subtitles_enabled = !self.state.subtitles_enabled;
        let s = self.strings();
        let state_txt = if self.state.subtitles_enabled {
            s.subtitle_on
        } else {
            s.subtitle_off
        };
        self.state
            .set_osd(format!("{}: {state_txt}", s.subtitle_toggle_btn));
        cx.notify();
    }

    pub(crate) fn adjust_subtitle_offset(&mut self, delta_ms: i64, cx: &mut Context<Self>) {
        self.state.subtitle_offset_ms = self
            .state
            .subtitle_offset_ms
            .saturating_add(delta_ms)
            .clamp(-60_000, 60_000);
        self.state.set_osd(format!(
            "{}: {:+} ms",
            self.strings().subtitle_offset_label,
            self.state.subtitle_offset_ms
        ));
        cx.notify();
    }

    pub(crate) fn cycle_subtitle_size(&mut self, cx: &mut Context<Self>) {
        self.state.subtitle_size = self.state.subtitle_size.cycle();
        let s = self.strings();
        self.state.set_osd(format!(
            "{}: {}",
            s.subtitle_size_label,
            subtitle_size_label(s, self.state.subtitle_size)
        ));
        cx.notify();
    }

    pub(crate) fn toggle_remaining_time(&mut self, cx: &mut Context<Self>) {
        self.state.show_remaining_time = !self.state.show_remaining_time;
        cx.notify();
    }

    pub(crate) fn toggle_shortcuts_help(&mut self, cx: &mut Context<Self>) {
        self.state.show_shortcuts_help = !self.state.show_shortcuts_help;
        cx.notify();
    }

    pub(crate) fn close_shortcuts_help(&mut self, cx: &mut Context<Self>) {
        if self.state.show_shortcuts_help {
            self.state.show_shortcuts_help = false;
            cx.notify();
        }
    }

    pub(crate) fn move_playlist_item(&mut self, index: usize, up: bool, cx: &mut Context<Self>) {
        let moved = if up {
            self.state.playlist.move_up(index)
        } else {
            self.state.playlist.move_down(index)
        };
        if moved {
            cx.notify();
        }
    }

    pub(crate) fn play_next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.shuffle_seed = self.state.shuffle_seed.wrapping_add(1);
        let manual_repeat = if self.state.repeat_mode == RepeatMode::One {
            RepeatMode::All
        } else {
            self.state.repeat_mode
        };
        if let Some(item) = self
            .state
            .playlist
            .advance_with_mode(manual_repeat, self.state.shuffle, self.state.shuffle_seed)
            .cloned()
        {
            self.load_path(item.path, true, None, window, cx);
        }
    }

    pub(crate) fn play_prev(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(item) = self
            .state
            .playlist
            .step_prev_with_mode(self.state.repeat_mode)
            .cloned()
        {
            self.load_path(item.path, true, None, window, cx);
        }
    }

    pub(crate) fn toggle_panel(&mut self, panel: SidePanel, cx: &mut Context<Self>) {
        self.state.toggle_panel(panel);
        cx.notify();
    }

    pub(crate) fn close_side_panel(&mut self, cx: &mut Context<Self>) {
        self.state.active_panel = SidePanel::None;
        cx.notify();
    }

    pub(crate) fn toggle_track_menu(&mut self, menu: TrackMenu, cx: &mut Context<Self>) {
        self.state.toggle_track_menu(menu);
        cx.notify();
    }

    pub(crate) fn close_track_menu(&mut self, cx: &mut Context<Self>) {
        if self.state.open_track_menu != TrackMenu::None {
            self.state.open_track_menu = TrackMenu::None;
            cx.notify();
        }
    }

    pub(crate) fn select_audio_track(&mut self, index: usize, cx: &mut Context<Self>) {
        self.state.open_track_menu = TrackMenu::None;
        if let Some(p) = self.player.as_mut() {
            if let Err(err) = p.select_audio_track(index) {
                self.state.report_command_error(&err);
            }
        }
        cx.notify();
    }

    pub(crate) fn select_video_track(&mut self, index: usize, cx: &mut Context<Self>) {
        self.state.open_track_menu = TrackMenu::None;
        if let Some(p) = self.player.as_mut() {
            if let Err(err) = p.select_video_track(index) {
                self.state.report_command_error(&err);
            }
        }
        cx.notify();
    }

    pub(crate) fn remove_playlist_item(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.state.playlist.remove(index) {
            RemoveOutcome::ActiveUnchanged => cx.notify(),
            RemoveOutcome::ActiveReplaced(Some(next_path)) => {
                self.load_path(next_path, true, None, window, cx);
            }
            RemoveOutcome::ActiveReplaced(None) => {
                self.reset_to_idle(window, cx);
            }
        }
    }

    pub(crate) fn clear_playlist(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.playlist.clear();
        self.reset_to_idle(window, cx);
    }

    fn reset_to_idle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(mut p) = self.player.take() {
            let _ = p.stop();
            let _ = p.shutdown();
        }
        if let Some(old_img) = self.current_frame.take() {
            let _ = window.drop_image(old_img);
        }
        self.last_raw_frame = None;
        self.ab_loop_last_jump = None;
        self.load_nonce = self.load_nonce.wrapping_add(1);
        self.probe_task = None;
        self.subtitle_task = None;
        self.state.status = PlaybackStatus::Idle;
        self.state.position = Duration::ZERO;
        self.state.duration = Duration::ZERO;
        self.state.scrub_fraction = 0.0;
        self.state.seek_hover_fraction = None;
        self.state.has_video_frame = false;
        self.state.video_unsupported = false;
        self.state.video_resolution = None;
        self.state.media_info = None;
        self.state.decoder_info = None;
        self.state.ab_loop.clear();
        self.state.subtitle_track = None;
        self.state.subtitle_name = None;
        self.state.subtitle_offset_ms = 0;
        self.state.open_track_menu = TrackMenu::None;
        self.state.clear_pending_controls();
        self.state.video_tracks.clear();
        self.state.audio_tracks.clear();
        self.state.active_video_track = None;
        self.state.active_audio_track = None;
        self.state.clear_error();
        self.state.clear_warning();
        self.state.is_probing = false;
        self.state.reset_waveform();
        window.set_window_title(self.strings().app_title);
        cx.notify();
    }

    fn handle_key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key_input = KeyInput {
            key: &ev.keystroke.key,
            ctrl: ev.keystroke.modifiers.control,
            alt: ev.keystroke.modifiers.alt,
            shift: ev.keystroke.modifiers.shift,
            meta: ev.keystroke.modifiers.platform,
        };
        let Some(action) = map_key_to_action(key_input) else {
            return;
        };

        match action {
            ShortcutAction::TogglePlayPause => self.toggle_play_pause(window, cx),
            ShortcutAction::SeekBackward => {
                self.seek_relative_mode(-SEEK_STEP_SECS, false, window, cx)
            }
            ShortcutAction::SeekForward => {
                self.seek_relative_mode(SEEK_STEP_SECS, false, window, cx)
            }
            ShortcutAction::SeekBackwardLong => {
                self.seek_relative_mode(-SEEK_LONG_STEP_SECS, false, window, cx)
            }
            ShortcutAction::SeekForwardLong => {
                self.seek_relative_mode(SEEK_LONG_STEP_SECS, false, window, cx)
            }
            ShortcutAction::SeekBackwardFine => {
                self.seek_relative_mode(-SEEK_FINE_STEP_SECS, true, window, cx)
            }
            ShortcutAction::SeekForwardFine => {
                self.seek_relative_mode(SEEK_FINE_STEP_SECS, true, window, cx)
            }
            ShortcutAction::JumpToPercent(pct) => {
                if !self.state.duration.is_zero() {
                    let target = Duration::from_secs_f64(
                        self.state.duration.as_secs_f64() * (f64::from(pct) / 10.0),
                    );
                    self.state.set_osd(format!("{}%", u16::from(pct) * 10));
                    self.seek_to_mode(target, false, window, cx);
                }
            }
            ShortcutAction::VolumeUp => self.adjust_volume(VOLUME_STEP, cx),
            ShortcutAction::VolumeDown => self.adjust_volume(-VOLUME_STEP, cx),
            ShortcutAction::ToggleFullscreen => self.toggle_fullscreen(window, cx),
            ShortcutAction::ExitFullscreen => {
                if self.state.show_shortcuts_help {
                    self.state.show_shortcuts_help = false;
                    cx.notify();
                } else if self.state.open_track_menu != TrackMenu::None {
                    self.state.open_track_menu = TrackMenu::None;
                    cx.notify();
                } else if window.is_fullscreen() {
                    self.toggle_fullscreen(window, cx);
                } else if self.state.active_panel != SidePanel::None {
                    self.state.active_panel = SidePanel::None;
                    cx.notify();
                }
            }
            ShortcutAction::ToggleMute => self.toggle_mute(cx),
            ShortcutAction::OpenFile => self.open_file_dialog(window, cx),
            ShortcutAction::Stop => self.stop_playback(window, cx),
            ShortcutAction::NextTrack => self.play_next(window, cx),
            ShortcutAction::PrevTrack => self.play_prev(window, cx),
            ShortcutAction::ToggleInfoPanel => {
                self.state.toggle_panel(SidePanel::MediaInfo);
                cx.notify();
            }
            ShortcutAction::TogglePlaylistPanel => {
                self.state.toggle_panel(SidePanel::Playlist);
                cx.notify();
            }
            ShortcutAction::ToggleVideoTrackMenu => {
                if !self.state.video_tracks.is_empty() {
                    self.state.toggle_track_menu(TrackMenu::Video);
                    cx.notify();
                }
            }
            ShortcutAction::ToggleAudioTrackMenu => {
                if !self.state.audio_tracks.is_empty() {
                    self.state.toggle_track_menu(TrackMenu::Audio);
                    cx.notify();
                }
            }
            ShortcutAction::ToggleLocale => self.toggle_locale(window, cx),
            ShortcutAction::SpeedDown => self.step_playback_rate(false, cx),
            ShortcutAction::SpeedUp => self.step_playback_rate(true, cx),
            ShortcutAction::ResetSpeed => self.reset_playback_rate(cx),
            ShortcutAction::CycleRepeatMode => self.cycle_repeat_mode(cx),
            ShortcutAction::RotateVideo => self.rotate_video(window, cx),
            ShortcutAction::ToggleShuffle => self.toggle_shuffle(cx),
            ShortcutAction::CycleAbLoop => self.cycle_ab_loop(cx),
            ShortcutAction::StepFrameForward => self.step_frame_forward(cx),
            ShortcutAction::ToggleSubtitles => self.toggle_subtitles(cx),
            ShortcutAction::LoadSubtitle => self.open_subtitle_dialog(window, cx),
            ShortcutAction::CycleAspectMode => self.cycle_aspect_mode(cx),
            ShortcutAction::ToggleToolsMenu => {
                self.state.toggle_track_menu(TrackMenu::Tools);
                cx.notify();
            }
            ShortcutAction::TakeScreenshot => self.take_screenshot(window, cx),
            ShortcutAction::ToggleShortcutsHelp => self.toggle_shortcuts_help(cx),
        }
    }

    /// Open a native multi-file picker off the UI thread and load/enqueue selected files.
    pub fn open_file_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_file_dialog_inner(true, window, cx);
    }

    pub(crate) fn open_file_dialog_inner(
        &mut self,
        play_immediately: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let filter_label = self.strings().media_filter_label;
        let dialog_title = self.strings().open_files_dialog_title;

        self.dialog_task = Some(cx.spawn_in(window, async move |this, cx| {
            let picked = cx
                .background_executor()
                .spawn(async move {
                    rfd::FileDialog::new()
                        .set_title(dialog_title)
                        .add_filter(filter_label, MEDIA_EXTENSIONS)
                        .pick_files()
                })
                .await;

            if let Some(paths) = picked {
                if !paths.is_empty() {
                    let _ = this.update_in(cx, |view, window, cx| {
                        view.enqueue_and_play(paths, play_immediately, window, cx);
                    });
                }
            }
        }));
    }

    /// Open a native subtitle file picker off the UI thread and parse the chosen `.srt` / `.vtt`.
    pub(crate) fn open_subtitle_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let filter_label = self.strings().subtitle_filter_label;
        let dialog_title = self.strings().subtitle_dialog_title;

        self.dialog_task = Some(cx.spawn_in(window, async move |this, cx| {
            let picked = cx
                .background_executor()
                .spawn(async move {
                    rfd::FileDialog::new()
                        .set_title(dialog_title)
                        .add_filter(filter_label, SUBTITLE_EXTENSIONS)
                        .pick_file()
                })
                .await;

            if let Some(path) = picked {
                let _ = this.update_in(cx, |view, window, cx| {
                    view.load_subtitle_path(path, window, cx);
                });
            }
        }));
    }

    /// Open a native folder picker off the UI thread and queue all supported media inside.
    pub(crate) fn open_folder_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dialog_title = self.strings().folder_dialog_title;

        self.dialog_task = Some(cx.spawn_in(window, async move |this, cx| {
            let expanded = cx
                .background_executor()
                .spawn(async move {
                    rfd::FileDialog::new()
                        .set_title(dialog_title)
                        .pick_folder()
                        .map(|folder| expand_dropped_paths(vec![folder]))
                        .unwrap_or_default()
                })
                .await;

            if !expanded.is_empty() {
                let _ = this.update_in(cx, |view, window, cx| {
                    view.enqueue_and_play(expanded, false, window, cx);
                });
            }
        }));
    }

    pub(crate) fn load_subtitle_path(
        &mut self,
        sub_path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let nonce = self.load_nonce;
        self.subtitle_task = Some(cx.spawn_in(window, async move |this, cx| {
            let (name, res) = cx
                .background_executor()
                .spawn(async move {
                    let name = sub_path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map(ToOwned::to_owned)
                        .unwrap_or_else(|| sub_path.to_string_lossy().into_owned());
                    let res = rsmpeg::codec::parse_subtitle_file(&sub_path);
                    (name, res)
                })
                .await;

            let _ = this.update_in(cx, |view, _window, cx| {
                if view.load_nonce != nonce {
                    return;
                }
                match res {
                    Ok(track) => {
                        view.state.subtitle_track = Some(track);
                        view.state.subtitle_name = Some(name.clone());
                        view.state.subtitles_enabled = true;
                        view.state.subtitle_offset_ms = 0;
                        view.state
                            .set_osd(format!("{}: {name}", view.strings().subtitle_loaded));
                    }
                    Err(err) => {
                        view.state
                            .set_osd(format!("{}: {err}", view.strings().subtitle_failed));
                    }
                }
                cx.notify();
            });
        }));
    }

    /// Handle OS drag-and-drop onto the window (file scanning runs off the UI thread).
    pub(crate) fn handle_external_drop(
        &mut self,
        external: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let raw_paths = external.paths().to_vec();
        if raw_paths.is_empty() {
            return;
        }
        self.dialog_task = Some(cx.spawn_in(window, async move |this, cx| {
            let (media, subtitle) = cx
                .background_executor()
                .spawn(async move { expand_dropped_items(raw_paths) })
                .await;
            if !media.is_empty() || subtitle.is_some() {
                let _ = this.update_in(cx, |view, window, cx| {
                    if !media.is_empty() {
                        view.enqueue_and_play(media, true, window, cx);
                    }
                    if let Some(sub_path) = subtitle {
                        view.load_subtitle_path(sub_path, window, cx);
                    }
                });
            }
        }));
    }

    /// Add one or more paths to the playlist; optionally start playing the first added track.
    pub(crate) fn enqueue_and_play(
        &mut self,
        paths: Vec<PathBuf>,
        play_immediately: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let was_idle = self.state.playlist.current_item().is_none();
        if let Some(first_idx) = self.state.playlist.add_paths(paths) {
            if play_immediately || was_idle {
                self.play_index(first_idx, window, cx);
            } else {
                cx.notify();
            }
        }
    }

    /// Select a playlist index and start playback.
    pub(crate) fn play_index(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.state.playlist.select(index).cloned() else {
            return;
        };
        self.load_path(item.path, true, None, window, cx);
    }

    /// Open a media path through `rsmpeg-player` and probe metadata in the background.
    pub(crate) fn load_path(
        &mut self,
        path: PathBuf,
        autoplay: bool,
        initial_seek: Option<Duration>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(mut prev) = self.player.take() {
            let _ = prev.shutdown();
        }
        if let Some(old_img) = self.current_frame.take() {
            let _ = window.drop_image(old_img);
        }
        self.last_raw_frame = None;
        self.ab_loop_last_jump = None;

        self.load_nonce = self.load_nonce.wrapping_add(1);
        let nonce = self.load_nonce;

        self.state.begin_loading();

        let title = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|name| format!("{name} — {}", self.strings().app_title))
            .unwrap_or_else(|| self.strings().app_title.to_string());
        window.set_window_title(&title);

        let probe_path = path.clone();
        self.probe_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { probe_file(&probe_path) })
                .await;

            let _ = this.update_in(cx, |view, _window, cx| {
                if view.load_nonce != nonce {
                    return;
                }
                view.state.is_probing = false;
                match result {
                    Ok(mut info) => {
                        if view.state.duration.is_zero() && !info.duration.is_zero() {
                            view.state.duration = info.duration;
                        } else {
                            info.update_duration_if_missing(view.state.duration);
                        }
                        view.state.media_info = Some(info);
                    }
                    Err(err) => {
                        if view.state.error_message.is_none() {
                            view.state.warning_message = Some(err);
                        }
                    }
                }
                cx.notify();
            });
        }));

        let sidecar_media = path.clone();
        self.subtitle_task = Some(cx.spawn_in(window, async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    let sub_path = PlaybackUiState::find_sidecar_subtitle(&sidecar_media)?;
                    let name = sub_path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map(ToOwned::to_owned)
                        .unwrap_or_else(|| sub_path.to_string_lossy().into_owned());
                    Some((name, rsmpeg::codec::parse_subtitle_file(&sub_path)))
                })
                .await;

            if let Some((name, res)) = loaded {
                let _ = this.update_in(cx, |view, _window, cx| {
                    if view.load_nonce != nonce {
                        return;
                    }
                    match res {
                        Ok(track) => {
                            view.state.subtitle_track = Some(track);
                            view.state.subtitle_name = Some(name);
                            cx.notify();
                        }
                        Err(err) => {
                            view.state.warning_message =
                                Some(format!("{}: {err}", view.strings().subtitle_failed));
                            cx.notify();
                        }
                    }
                });
            }
        }));

        match Player::builder()
            .input(&path)
            .prefer_native_pipeline(true)
            .volume(self.state.effective_volume())
            .playback_rate(self.state.playback_rate)
            .autoplay(autoplay)
            .build()
        {
            Ok(mut player) => {
                if let Some(target) = initial_seek {
                    if let Err(err) = player.seek(target) {
                        self.state.report_command_error(&err);
                    }
                }
                self.state.status = if autoplay {
                    PlaybackStatus::Opening
                } else {
                    PlaybackStatus::Paused
                };
                self.player = Some(player);
            }
            Err(err) => {
                self.state.status = PlaybackStatus::Error;
                self.state.error_message =
                    Some(format!("{}：{err}", self.strings().error_open_failed));
            }
        }

        cx.notify();
    }

    /// Poll `rsmpeg-player` without blocking the UI thread and upload the newest RGBA frame.
    fn poll_player(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut changed = self.state.expire_osd();
        let mut should_clear_player = false;
        let mut auto_advance = false;
        let mut replay_current = false;

        let fullscreen_now = window.is_fullscreen();
        if fullscreen_now != self.state.is_fullscreen {
            self.state.is_fullscreen = fullscreen_now;
            changed = true;
        }

        if let Some(player) = self.player.as_mut() {
            let (events, latest_frame) = player.poll_all();

            for ev in &events {
                changed = true;
                match self.state.apply_player_event(ev) {
                    EventOutcome::AutoAdvanceNext => {
                        auto_advance = true;
                        should_clear_player = true;
                    }
                    EventOutcome::ReplayCurrent => {
                        replay_current = true;
                        should_clear_player = true;
                    }
                    EventOutcome::None => {}
                }
                if matches!(ev, PlayerEvent::Ended { .. } | PlayerEvent::Error { .. }) {
                    should_clear_player = true;
                }
            }

            if let Some(frame_ev) = latest_frame {
                if self.state.is_fresh_generation(frame_ev.generation()) {
                    changed = true;
                    self.state.apply_player_event(&frame_ev);
                    if let PlayerEvent::VideoFrame {
                        width,
                        height,
                        rgba,
                        ..
                    } = frame_ev
                    {
                        if let Some(next_img) = build_render_image_transformed(
                            width,
                            height,
                            &rgba,
                            self.state.video_transform,
                        ) {
                            if let Some(old_img) = self.current_frame.replace(next_img) {
                                let _ = window.drop_image(old_img);
                            }
                        }
                        self.last_raw_frame = Some((width, height, rgba));
                    }
                }
            }
        }

        if should_clear_player {
            if let Some(mut p) = self.player.take() {
                let _ = p.shutdown();
            }
        }

        if replay_current {
            if let Some(item) = self.state.playlist.current_item().cloned() {
                self.load_path(item.path, true, None, window, cx);
                return;
            }
        }

        if auto_advance {
            self.state.shuffle_seed = self.state.shuffle_seed.wrapping_add(1);
            if let Some(item) = self
                .state
                .playlist
                .advance_with_mode(
                    self.state.repeat_mode,
                    self.state.shuffle,
                    self.state.shuffle_seed,
                )
                .cloned()
            {
                self.load_path(item.path, true, None, window, cx);
                return;
            }
        }

        if !self.state.scrubbing && self.state.status == PlaybackStatus::Playing {
            if let Some(target_a) = self.state.check_ab_loop() {
                let cooldown_ok = self
                    .ab_loop_last_jump
                    .is_none_or(|t| t.elapsed() >= AB_LOOP_COOLDOWN);
                if cooldown_ok {
                    self.ab_loop_last_jump = Some(Instant::now());
                    self.seek_to_mode(target_a, true, window, cx);
                    return;
                }
            }
        }

        if changed {
            cx.notify();
        }
    }

    fn status_badge(&self) -> (&'static str, gpui::Rgba) {
        let s = self.strings();
        match self.state.status {
            PlaybackStatus::Idle => (s.status_idle, theme::text_muted()),
            PlaybackStatus::Opening => (s.status_opening, theme::accent()),
            PlaybackStatus::Playing => (s.status_playing, theme::status_playing()),
            PlaybackStatus::Paused => (s.status_paused, theme::status_paused()),
            PlaybackStatus::Seeking => (s.status_seeking, theme::accent()),
            PlaybackStatus::Stopped => (s.status_stopped, theme::text_muted()),
            PlaybackStatus::Ended => (s.status_ended, theme::text_secondary()),
            PlaybackStatus::Error => (s.status_error, theme::error_border()),
        }
    }

    fn render_top_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let s = self.strings();
        let (status_label, status_color) = self.status_badge();
        let track_title = self
            .state
            .playlist
            .current_item()
            .map(|it| it.display_name.clone())
            .unwrap_or_else(|| s.no_file_loaded.to_string());

        let resolution_chip = self.state.video_resolution.map(|(w, h)| format!("{w}×{h}"));
        let decoder_chip = self.state.decoder_info.as_ref().map(|d| {
            d.video_decoder
                .clone()
                .or_else(|| d.audio_decoder.clone())
                .unwrap_or_else(|| d.demuxer.clone())
        });
        let playlist_pos_chip = if self.state.playlist.len() > 1 {
            self.state
                .playlist
                .current_index()
                .map(|i| format!("{} / {}", i + 1, self.state.playlist.len()))
        } else {
            None
        };

        div()
            .flex()
            .flex_none()
            .flex_row()
            .items_center()
            .justify_between()
            .h(px(44.0))
            .px_4()
            .gap_3()
            .bg(theme::bg_surface())
            .border_b_1()
            .border_color(theme::border_subtle())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2p5()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .bg(theme::accent_soft())
                            .text_xs()
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(theme::accent())
                            .child("rsmpeg"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme::text_primary())
                            .truncate()
                            .child(track_title),
                    )
                    .when_some(resolution_chip, |el, res| {
                        el.child(
                            div()
                                .flex_none()
                                .px_1p5()
                                .py_0p5()
                                .rounded_sm()
                                .bg(theme::bg_elevated())
                                .text_xs()
                                .text_color(theme::text_secondary())
                                .child(res),
                        )
                    })
                    .when_some(decoder_chip, |el, dec| {
                        el.child(
                            div()
                                .flex_none()
                                .max_w(px(180.0))
                                .px_1p5()
                                .py_0p5()
                                .rounded_sm()
                                .bg(theme::bg_elevated())
                                .font_family(theme::MONO_FONT_FAMILY)
                                .text_xs()
                                .text_color(theme::accent())
                                .truncate()
                                .child(dec),
                        )
                    })
                    .when_some(playlist_pos_chip, |el, pos| {
                        el.child(
                            div()
                                .flex_none()
                                .px_1p5()
                                .py_0p5()
                                .rounded_sm()
                                .bg(theme::bg_elevated())
                                .font_family(theme::MONO_FONT_FAMILY)
                                .text_xs()
                                .text_color(theme::text_secondary())
                                .child(pos),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1p5()
                            .px_2()
                            .py_0p5()
                            .rounded_full()
                            .bg(theme::bg_elevated())
                            .child(div().w(px(7.0)).h(px(7.0)).rounded_full().bg(status_color))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme::text_secondary())
                                    .child(status_label),
                            ),
                    )
                    .child(
                        div()
                            .id("topbar-info-btn")
                            .px_2p5()
                            .py_1()
                            .rounded_md()
                            .bg(if self.state.active_panel == SidePanel::MediaInfo {
                                theme::accent_soft()
                            } else {
                                theme::bg_elevated()
                            })
                            .hover(|st| st.bg(theme::bg_hover()))
                            .border_1()
                            .border_color(if self.state.active_panel == SidePanel::MediaInfo {
                                theme::accent()
                            } else {
                                theme::border_subtle()
                            })
                            .text_xs()
                            .text_color(if self.state.active_panel == SidePanel::MediaInfo {
                                theme::accent()
                            } else {
                                theme::text_primary()
                            })
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.focus_root(window);
                                this.toggle_panel(SidePanel::MediaInfo, cx);
                            }))
                            .child(s.info_panel_btn),
                    )
                    .child(
                        div()
                            .id("topbar-playlist-btn")
                            .px_2p5()
                            .py_1()
                            .rounded_md()
                            .bg(if self.state.active_panel == SidePanel::Playlist {
                                theme::accent_soft()
                            } else {
                                theme::bg_elevated()
                            })
                            .hover(|st| st.bg(theme::bg_hover()))
                            .border_1()
                            .border_color(if self.state.active_panel == SidePanel::Playlist {
                                theme::accent()
                            } else {
                                theme::border_subtle()
                            })
                            .text_xs()
                            .text_color(if self.state.active_panel == SidePanel::Playlist {
                                theme::accent()
                            } else {
                                theme::text_primary()
                            })
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.focus_root(window);
                                this.toggle_panel(SidePanel::Playlist, cx);
                            }))
                            .child(format!(
                                "{} ({})",
                                s.playlist_panel_btn,
                                self.state.playlist.len()
                            )),
                    )
                    .child(
                        div()
                            .id("topbar-help-btn")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(if self.state.show_shortcuts_help {
                                theme::accent_soft()
                            } else {
                                theme::bg_elevated()
                            })
                            .hover(|st| st.bg(theme::bg_hover()))
                            .border_1()
                            .border_color(if self.state.show_shortcuts_help {
                                theme::accent()
                            } else {
                                theme::border_subtle()
                            })
                            .font_family(theme::MONO_FONT_FAMILY)
                            .text_xs()
                            .text_color(if self.state.show_shortcuts_help {
                                theme::accent()
                            } else {
                                theme::text_secondary()
                            })
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.focus_root(window);
                                this.toggle_shortcuts_help(cx);
                            }))
                            .child("?"),
                    )
                    .child(
                        div()
                            .id("topbar-locale-btn")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(theme::bg_elevated())
                            .hover(|st| st.bg(theme::bg_hover()))
                            .border_1()
                            .border_color(theme::border_subtle())
                            .font_family(theme::MONO_FONT_FAMILY)
                            .text_xs()
                            .text_color(theme::text_secondary())
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.focus_root(window);
                                this.toggle_locale(window, cx);
                            }))
                            .child(self.state.locale.short_label()),
                    )
                    .child(
                        div()
                            .id("topbar-open-btn")
                            .px_2p5()
                            .py_1()
                            .rounded_md()
                            .bg(theme::bg_elevated())
                            .hover(|st| st.bg(theme::bg_hover()))
                            .border_1()
                            .border_color(theme::border_subtle())
                            .text_xs()
                            .text_color(theme::text_primary())
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_file_dialog(window, cx);
                            }))
                            .child(s.open_short_btn),
                    ),
            )
    }
}

impl Render for PlayerView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = self.strings();
        let stage_el = stage::render_stage(&self.state, self.current_frame.as_ref(), s, cx);
        let side_panel = panels::render_side_panel(&self.state, s, cx);
        let track_popover = controls::render_track_menu_popover(&self.state, s, cx);
        let tools_popover = controls::render_tools_popover(&self.state, s, cx);
        let controls_el = controls::render_controls(
            &self.state,
            self.seek_track_bounds.clone(),
            self.volume_track_bounds.clone(),
            s,
            cx,
        );

        div()
            .id("rsmpeg-root")
            .relative()
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::bg_window())
            .text_color(theme::text_primary())
            .font_family(theme::UI_FONT_FAMILY)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                this.handle_key_down(ev, window, cx);
            }))
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _window, cx| {
                if this.state.scrubbing {
                    this.update_seek_drag(ev.position.x, cx);
                } else if this.dragging_volume {
                    this.apply_volume_from_x(ev.position.x, cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _ev: &MouseUpEvent, window, cx| {
                    if this.state.scrubbing {
                        this.finish_seek_drag(window, cx);
                    }
                    this.dragging_volume = false;
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _ev: &MouseUpEvent, window, cx| {
                    if this.state.scrubbing {
                        this.finish_seek_drag(window, cx);
                    }
                    this.dragging_volume = false;
                }),
            )
            .drag_over::<ExternalPaths>(|style, _, _, _| {
                style.border_2().border_color(theme::accent())
            })
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.handle_external_drop(paths, window, cx);
            }))
            .child(self.render_top_bar(cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .child(stage_el)
                    .when_some(side_panel, |el, panel| el.child(panel)),
            )
            .child(controls_el)
            .when_some(track_popover, |el, popover| el.child(popover))
            .when_some(tools_popover, |el, popover| el.child(popover))
    }
}
