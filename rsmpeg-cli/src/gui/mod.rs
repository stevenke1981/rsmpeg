//! AcmeUI Native GUI host.
//!
//! The UI owns only presentation state. Demux and decode stay behind the
//! [`rsmpeg_player::Player`] command/event boundary.

mod ui;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use acme_core::Scene;
use acme_platform::{Application, FrameContext, PlatformEvent, WindowConfig, WindowId};
use rsmpeg_player::{Player, PlayerEvent, PlayerState};

use self::ui::{PlayerUi, UiIntent, UiModel};

const PLAYER_POLL_INTERVAL: Duration = Duration::from_millis(8);
const TIMELINE_PREVIEW_INTERVAL: Duration = Duration::from_millis(75);

const MEDIA_EXTENSIONS: &[&str] = &[
    "mp4", "mkv", "avi", "mov", "webm", "flac", "mp3", "wav", "ogg", "m4a",
];

#[derive(Clone)]
pub(crate) struct VideoPreview {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rgba: Arc<[u8]>,
    pub(crate) revision: u64,
}

pub struct MediaApp {
    player: Option<Player>,
    preview: Option<VideoPreview>,
    file_path: Option<PathBuf>,
    status: String,
    volume: f32,
    position_sec: f64,
    duration_sec: f64,
    playing: bool,
    scrub_position_sec: Option<f64>,
    timeline_drag_active: bool,
    last_timeline_preview: Option<Instant>,
    ui: PlayerUi,
}

impl MediaApp {
    pub fn new(path: Option<PathBuf>) -> Self {
        let mut app = Self {
            player: None,
            preview: None,
            file_path: None,
            status: "Open or drop a media file to begin".into(),
            volume: 0.8,
            position_sec: 0.0,
            duration_sec: 0.0,
            playing: false,
            scrub_position_sec: None,
            timeline_drag_active: false,
            last_timeline_preview: None,
            ui: PlayerUi::new(),
        };
        if let Some(path) = path {
            app.load_file(path);
        }
        app
    }

    fn load_file(&mut self, path: PathBuf) {
        self.close_player();
        self.reset_playback_view();

        match Player::builder()
            .input(&path)
            .volume(self.volume)
            .autoplay(true)
            .build()
        {
            Ok(player) => {
                self.status = format!("Opening {}", display_name(&path));
                self.file_path = Some(path);
                self.playing = true;
                self.player = Some(player);
            }
            Err(error) => {
                self.status = format!("Open failed: {error}");
                self.file_path = None;
            }
        }
    }

    fn stop_playback(&mut self) {
        if let Some(player) = self.player.as_mut() {
            if let Err(error) = player.stop() {
                self.status = format!("Stop failed: {error}");
            }
        }
        self.close_player();
        self.reset_playback_view();
        self.file_path = None;
        self.status = "Stopped. Open or drop a media file to begin.".into();
    }

    fn close_player(&mut self) {
        if let Some(mut player) = self.player.take() {
            let _ = player.shutdown();
        }
    }

    fn reset_playback_view(&mut self) {
        self.preview = None;
        self.position_sec = 0.0;
        self.duration_sec = 0.0;
        self.playing = false;
        self.scrub_position_sec = None;
        self.timeline_drag_active = false;
        self.last_timeline_preview = None;
    }

    fn handle_intent(&mut self, intent: UiIntent) {
        match intent {
            UiIntent::OpenFile => self.open_file_dialog(),
            UiIntent::TogglePlayback => {
                if self.player.is_none() {
                    if let Some(path) = self.file_path.clone() {
                        self.load_file(path);
                    }
                    return;
                }
                let player = self.player.as_mut().expect("player checked above");
                let result = if self.playing {
                    player.pause()
                } else {
                    player.play()
                };
                match result {
                    Ok(()) => self.playing = !self.playing,
                    Err(error) => self.status = format!("Playback control failed: {error}"),
                }
            }
            UiIntent::Stop => self.stop_playback(),
            UiIntent::Seek {
                fraction,
                final_commit,
            } => self.seek_from_fraction(fraction, final_commit),
            UiIntent::Volume(volume) => {
                self.volume = volume.clamp(0.0, 1.0);
                if let Some(player) = self.player.as_mut() {
                    if let Err(error) = player.set_volume(self.volume) {
                        self.status = format!("Volume change failed: {error}");
                    }
                }
            }
        }
    }

    fn open_file_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Media", MEDIA_EXTENSIONS)
            .pick_file()
        {
            self.load_file(path);
        }
    }

    fn seek_from_fraction(&mut self, fraction: f32, final_commit: bool) {
        if self.duration_sec <= 0.0 || !self.duration_sec.is_finite() {
            return;
        }
        let target = f64::from(fraction.clamp(0.0, 1.0)) * self.duration_sec;
        self.position_sec = target;
        self.scrub_position_sec = Some(target);
        self.timeline_drag_active = !final_commit;
        self.seek_timeline_preview(target, final_commit);
    }

    fn seek_timeline_preview(&mut self, target: f64, force: bool) {
        if !target.is_finite() {
            return;
        }
        let now = Instant::now();
        if !should_dispatch_timeline_seek(self.last_timeline_preview, now, force) {
            return;
        }
        if let Some(player) = self.player.as_mut() {
            match player.seek(Duration::from_secs_f64(target.max(0.0))) {
                Ok(()) => self.last_timeline_preview = Some(now),
                Err(error) => self.status = format!("Seek failed: {error}"),
            }
        }
    }

    fn poll_player(&mut self) -> bool {
        let Some(player) = self.player.as_mut() else {
            return false;
        };

        let (events, latest_frame) = player.poll_all();
        let mut changed = !events.is_empty() || latest_frame.is_some();
        let mut clear_player = false;

        for event in events {
            match event {
                PlayerEvent::Snapshot(snapshot) => {
                    self.playing = snapshot.playing;
                    self.position_sec = snapshot.position.as_secs_f64();
                    self.duration_sec = snapshot.duration.as_secs_f64();
                    self.volume = snapshot.volume;
                    self.status = if snapshot.playing {
                        "Playing".into()
                    } else {
                        "Paused".into()
                    };
                }
                PlayerEvent::PositionChanged { position, .. } => {
                    if !self.timeline_drag_active {
                        self.position_sec = position.as_secs_f64();
                    }
                }
                PlayerEvent::SeekCompleted { position, .. } => {
                    self.position_sec = position.as_secs_f64();
                    if !self.timeline_drag_active {
                        self.scrub_position_sec = None;
                    }
                }
                PlayerEvent::Ended { .. } => {
                    self.status = "Playback complete".into();
                    self.playing = false;
                    clear_player = true;
                }
                PlayerEvent::Error { message, .. } => {
                    self.status = format!("Playback error: {message}");
                    self.playing = false;
                    clear_player = true;
                }
                PlayerEvent::Warning { message, .. } => self.status = message,
                PlayerEvent::VideoFrame { .. } => {}
            }
        }

        if let Some(PlayerEvent::VideoFrame {
            width,
            height,
            rgba,
            pts,
            ..
        }) = latest_frame
        {
            let current_revision = self.preview.as_ref().map_or(0, |preview| preview.revision);
            match video_preview(width, height, rgba, current_revision) {
                Ok(preview) => {
                    self.preview = Some(preview);
                    self.status = if self.playing {
                        "Playing".into()
                    } else {
                        "Paused".into()
                    };
                }
                Err(message) => self.status = message,
            }
            if !self.timeline_drag_active {
                self.position_sec = pts.as_secs_f64();
                self.scrub_position_sec = None;
            }
        }

        self.duration_sec = player.duration().as_secs_f64();
        self.playing = player.is_playing();
        if player.state() == PlayerState::Error {
            if let Some(error) = player.last_error() {
                self.status = format!("Playback error: {error}");
            }
            clear_player = true;
            changed = true;
        }

        if clear_player {
            self.close_player();
        }
        changed
    }

    fn handle_dropped_paths(&mut self, paths: Vec<String>) {
        let mut candidates = paths.into_iter().map(PathBuf::from);
        let path = candidates
            .clone()
            .find(|path| is_media_path(path))
            .or_else(|| candidates.next());
        if let Some(path) = path {
            self.load_file(path);
        }
    }

    fn model(&self) -> UiModel {
        UiModel {
            has_media: self.file_path.is_some(),
            file_name: self.file_path.as_deref().map(display_name),
            status: self.status.clone(),
            playing: self.playing,
            position_sec: self.scrub_position_sec.unwrap_or(self.position_sec),
            duration_sec: self.duration_sec,
            volume: self.volume,
        }
    }
}

impl Application for MediaApp {
    fn window_config(&self) -> WindowConfig {
        WindowConfig {
            title: "rsmpeg".into(),
            width: 320.0,
            height: 220.0,
        }
    }

    fn minimum_window_size(&self) -> Option<(f64, f64)> {
        Some((320.0, 220.0))
    }

    fn event(&mut self, event: PlatformEvent) -> bool {
        let (intents, visual_changed) = match event {
            PlatformEvent::PointerMoved { x, y, .. } => (self.ui.pointer_moved(x, y), true),
            PlatformEvent::PointerButton {
                pressed,
                x,
                y,
                button: 0,
                ..
            } => (self.ui.pointer_button(pressed, x, y), true),
            PlatformEvent::Key {
                key,
                pressed: true,
                shift,
                ..
            } => (self.ui.key_pressed(key, shift, &self.model()), true),
            PlatformEvent::FileDropped { paths, .. } => {
                self.handle_dropped_paths(paths);
                return true;
            }
            PlatformEvent::Resized { .. } => return true,
            _ => return false,
        };
        let changed = visual_changed || !intents.is_empty();
        for intent in intents {
            self.handle_intent(intent);
        }
        changed
    }

    fn background_poll_interval(&self) -> Option<Duration> {
        self.player.as_ref().map(|_| PLAYER_POLL_INTERVAL)
    }

    fn background_tick(&mut self) -> bool {
        self.poll_player()
    }

    fn on_gpu_recovered(&mut self, window: WindowId) {
        self.ui.on_gpu_recovered(window);
    }

    fn frame(&mut self, context: FrameContext) -> Scene {
        let model = self.model();
        self.ui.frame(context, &model, self.preview.as_ref())
    }
}

fn should_dispatch_timeline_seek(last: Option<Instant>, now: Instant, force: bool) -> bool {
    force
        || last
            .map(|previous| now.saturating_duration_since(previous) >= TIMELINE_PREVIEW_INTERVAL)
            .unwrap_or(true)
}

fn is_media_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            MEDIA_EXTENSIONS
                .iter()
                .any(|allowed| extension.eq_ignore_ascii_case(allowed))
        })
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn video_preview(
    width: usize,
    height: usize,
    rgba: Vec<u8>,
    current_revision: u64,
) -> Result<VideoPreview, String> {
    let expected_len = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4));
    if expected_len != Some(rgba.len())
        || u32::try_from(width).is_err()
        || u32::try_from(height).is_err()
    {
        return Err(format!(
            "Dropped invalid RGBA frame: {width}x{height}, {} bytes",
            rgba.len()
        ));
    }
    Ok(VideoPreview {
        width: width as u32,
        height: height as u32,
        rgba: Arc::from(rgba),
        revision: current_revision.wrapping_add(1),
    })
}

pub fn run_gui(path: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let path = path.map(PathBuf::from);
    acme_platform::run(MediaApp::new(path))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeline_preview_is_throttled_but_commit_is_immediate() {
        let start = Instant::now();
        assert!(should_dispatch_timeline_seek(None, start, false));
        assert!(!should_dispatch_timeline_seek(
            Some(start),
            start + Duration::from_millis(74),
            false
        ));
        assert!(should_dispatch_timeline_seek(
            Some(start),
            start + TIMELINE_PREVIEW_INTERVAL,
            false
        ));
        assert!(should_dispatch_timeline_seek(
            Some(start),
            start + Duration::from_millis(1),
            true
        ));
    }

    #[test]
    fn media_extension_matching_is_case_insensitive() {
        assert!(is_media_path(Path::new("clip.MP4")));
        assert!(is_media_path(Path::new("audio.flac")));
        assert!(!is_media_path(Path::new("notes.txt")));
    }

    #[test]
    fn video_preview_validates_shape_and_advances_revision() {
        let preview = video_preview(2, 2, vec![0; 16], 7).expect("valid frame");
        assert_eq!((preview.width, preview.height, preview.revision), (2, 2, 8));
        assert!(video_preview(2, 2, vec![0; 15], 0).is_err());
        assert!(video_preview(usize::MAX, 2, Vec::new(), 0).is_err());
    }

    #[test]
    fn pointer_motion_requests_visual_redraw_without_an_intent() {
        let mut app = MediaApp::new(None);
        assert!(app.event(PlatformEvent::PointerMoved {
            window: WindowId(1),
            x: 10.0,
            y: 10.0,
        }));
    }
}
