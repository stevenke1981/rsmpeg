//! Main GPUI view hosting the `rsmpeg-player` session, polling loop, and UI composition.

pub mod controls;
pub mod panels;
pub mod stage;
pub mod theme;

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    div, prelude::*, px, Bounds, Context, ExternalPaths, FocusHandle, Focusable, IntoElement,
    KeyDownEvent, MouseButton, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Render,
    RenderImage, Styled, Task, Window,
};
use rsmpeg_player::{Player, PlayerEvent};

use crate::i18n::{strings, Locale, UiStrings};
use crate::media_info::probe_file;
use crate::playlist::{RemoveOutcome, MEDIA_EXTENSIONS};
use crate::shortcuts::{map_key_to_action, KeyInput, ShortcutAction, SEEK_STEP_SECS, VOLUME_STEP};
use crate::state::{
    expand_dropped_paths, fraction_from_pointer, EventOutcome, PlaybackStatus, PlaybackUiState,
    SidePanel, TrackMenu,
};
use crate::video::build_render_image;

const POLL_INTERVAL: Duration = Duration::from_millis(16);

/// Root GPUI view for `rsmpeg-gui`.
pub struct PlayerView {
    pub(crate) state: PlaybackUiState,
    focus_handle: FocusHandle,
    player: Option<Player>,
    current_frame: Option<Arc<RenderImage>>,
    /// Incremented on each file load so stale background probes are ignored.
    load_nonce: u64,
    _poll_task: Task<()>,
    probe_task: Option<Task<()>>,
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
            load_nonce: 0,
            _poll_task: poll_task,
            probe_task: None,
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
        self.state.has_video_frame = false;
        self.state.position = Duration::ZERO;
        self.state.scrubbing = false;
        self.state.scrub_fraction = 0.0;
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
        let clamped = if !self.state.duration.is_zero() {
            target.min(self.state.duration)
        } else {
            target
        };

        if let Some(p) = self.player.as_mut() {
            if let Err(err) = p.seek(clamped) {
                self.state.report_command_error(&err);
            }
            cx.notify();
        } else if let Some(item) = self.state.playlist.current_item().cloned() {
            let autoplay = self.state.status == PlaybackStatus::Playing;
            self.load_path(item.path, autoplay, Some(clamped), window, cx);
        }
    }

    pub(crate) fn seek_relative(
        &mut self,
        delta_secs: f64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state.playlist.current_item().is_none() {
            return;
        }
        let target = self.state.seek_relative_target(delta_secs);
        self.seek_to(target, window, cx);
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

    pub(crate) fn toggle_mute(&mut self, cx: &mut Context<Self>) {
        let intent = self.state.preview_toggle_mute();
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

    pub(crate) fn cycle_playback_rate(&mut self, cx: &mut Context<Self>) {
        let rate = self.state.next_cycle_rate();
        if let Some(p) = self.player.as_mut() {
            match p.set_playback_rate(rate) {
                Ok(()) => self.state.queue_playback_rate(rate),
                Err(err) => self.state.report_command_error(&err),
            }
        } else {
            self.state.playback_rate = rate;
        }
        cx.notify();
    }

    pub(crate) fn step_playback_rate(&mut self, up: bool, cx: &mut Context<Self>) {
        let rate = if up {
            self.state.next_step_rate_up()
        } else {
            self.state.next_step_rate_down()
        };
        if let Some(p) = self.player.as_mut() {
            match p.set_playback_rate(rate) {
                Ok(()) => self.state.queue_playback_rate(rate),
                Err(err) => self.state.report_command_error(&err),
            }
        } else {
            self.state.playback_rate = rate;
        }
        cx.notify();
    }

    pub(crate) fn play_next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(item) = self.state.playlist.advance_next().cloned() {
            self.load_path(item.path, true, None, window, cx);
        }
    }

    pub(crate) fn play_prev(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(item) = self.state.playlist.step_prev().cloned() {
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
        self.load_nonce = self.load_nonce.wrapping_add(1);
        self.probe_task = None;
        self.state.status = PlaybackStatus::Idle;
        self.state.position = Duration::ZERO;
        self.state.duration = Duration::ZERO;
        self.state.has_video_frame = false;
        self.state.video_unsupported = false;
        self.state.video_resolution = None;
        self.state.media_info = None;
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
            ShortcutAction::SeekBackward => self.seek_relative(-SEEK_STEP_SECS, window, cx),
            ShortcutAction::SeekForward => self.seek_relative(SEEK_STEP_SECS, window, cx),
            ShortcutAction::VolumeUp => self.adjust_volume(VOLUME_STEP, cx),
            ShortcutAction::VolumeDown => self.adjust_volume(-VOLUME_STEP, cx),
            ShortcutAction::ToggleFullscreen => self.toggle_fullscreen(window, cx),
            ShortcutAction::ExitFullscreen => {
                if self.state.open_track_menu != TrackMenu::None {
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
            let expanded = cx
                .background_executor()
                .spawn(async move { expand_dropped_paths(raw_paths) })
                .await;
            if !expanded.is_empty() {
                let _ = this.update_in(cx, |view, window, cx| {
                    view.enqueue_and_play(expanded, true, window, cx);
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
        let mut changed = false;
        let mut should_clear_player = false;
        let mut auto_advance = false;

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
                        if let Some(next_img) = build_render_image(width, height, &rgba) {
                            if let Some(old_img) = self.current_frame.replace(next_img) {
                                let _ = window.drop_image(old_img);
                            }
                        }
                    }
                }
            }
        }

        if should_clear_player {
            if let Some(mut p) = self.player.take() {
                let _ = p.shutdown();
            }
        }

        if auto_advance {
            if let Some(next_idx) = self.state.playlist.current_index().map(|i| i + 1) {
                self.play_index(next_idx, window, cx);
                return;
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
    }
}
