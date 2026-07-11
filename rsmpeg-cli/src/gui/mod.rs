//! egui/eframe GUI — hosts only send commands and poll [`rsmpeg_player`] events.
//!
//! Demux / decode never run on the UI thread (todos.md Phase 0 / 9.1).

pub mod ui;

use eframe::egui;
use rsmpeg_player::{Player, PlayerEvent, PlayerState};

/// Main egui application state.
pub struct MediaApp {
    player: Option<Player>,
    texture: Option<egui::TextureHandle>,
    file_path: Option<String>,
    status: String,
    volume: f32,
    /// Cached playhead for UI (updated from events).
    position_sec: f64,
    duration_sec: f64,
    playing: bool,
}

impl MediaApp {
    pub fn new(path: Option<String>) -> Self {
        let mut app = Self {
            player: None,
            texture: None,
            file_path: None,
            status: "Open a media file to start playback".into(),
            volume: 0.8,
            position_sec: 0.0,
            duration_sec: 0.0,
            playing: false,
        };
        if let Some(p) = path {
            app.load_file(&p);
        }
        app
    }

    pub fn load_file(&mut self, path: &str) {
        if let Some(mut p) = self.player.take() {
            let _ = p.shutdown();
        }
        self.texture = None;
        self.position_sec = 0.0;
        self.duration_sec = 0.0;
        self.playing = false;

        match Player::builder()
            .input(path)
            .volume(self.volume)
            .autoplay(true)
            .build()
        {
            Ok(player) => {
                self.file_path = Some(path.to_string());
                self.status = format!("Playing: {}", path);
                self.playing = true;
                self.player = Some(player);
            }
            Err(e) => {
                self.status = format!("Error: {}", e);
                self.file_path = None;
            }
        }
    }

    pub fn stop_playback(&mut self) {
        if let Some(mut p) = self.player.take() {
            let _ = p.stop();
            let _ = p.shutdown();
        }
        self.texture = None;
        self.file_path = None;
        self.playing = false;
        self.position_sec = 0.0;
        self.duration_sec = 0.0;
        self.status = "Stopped. Open a media file to start playback.".into();
    }
}

impl eframe::App for MediaApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let mut got_frame = false;
        let mut clear_player = false;

        if let Some(player) = self.player.as_mut() {
            let (events, latest_frame) = player.poll_all();
            for ev in events {
                match ev {
                    PlayerEvent::Snapshot(s) => {
                        self.playing = s.playing;
                        self.position_sec = s.position.as_secs_f64();
                        self.duration_sec = s.duration.as_secs_f64();
                        self.volume = s.volume;
                        if !s.status.is_empty() && s.status != "playing" {
                            // keep path-based status unless error-ish
                        }
                    }
                    PlayerEvent::PositionChanged { position, .. } => {
                        self.position_sec = position.as_secs_f64();
                    }
                    PlayerEvent::SeekCompleted { position, .. } => {
                        self.position_sec = position.as_secs_f64();
                    }
                    PlayerEvent::Ended { .. } => {
                        self.status = "Playback complete".into();
                        self.playing = false;
                        clear_player = true;
                    }
                    PlayerEvent::Error { message, .. } => {
                        self.status = format!("Playback error: {}", message);
                        self.file_path = None;
                        clear_player = true;
                    }
                    PlayerEvent::Warning { message, .. } => {
                        self.status = message;
                    }
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
                got_frame = true;
                self.position_sec = pts.as_secs_f64();
                let color_image = egui::ColorImage::from_rgba_unmultiplied([width, height], &rgba);
                match self.texture.as_mut() {
                    Some(tex) if tex.size() == [width, height] => {
                        tex.set(color_image, egui::TextureOptions::LINEAR);
                    }
                    _ => {
                        self.texture = Some(ctx.load_texture(
                            "video_frame",
                            color_image,
                            egui::TextureOptions::LINEAR,
                        ));
                    }
                }
            }

            // Sync duration/playing from player cache
            self.duration_sec = player.duration().as_secs_f64();
            self.playing = player.is_playing();
            if player.state() == PlayerState::Error {
                if let Some(err) = player.last_error() {
                    self.status = format!("Playback error: {}", err);
                }
                clear_player = true;
            }
        }

        if clear_player {
            if let Some(mut p) = self.player.take() {
                let _ = p.shutdown();
            }
            ctx.request_repaint();
        }

        ui::render_ui(self, ctx);

        if self.player.is_some() {
            if got_frame {
                ctx.request_repaint();
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(8));
            }
        }
    }
}

/// Launch the egui GUI window.
pub fn run_gui(path: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(egui::vec2(960.0, 600.0))
            .with_min_inner_size(egui::vec2(480.0, 320.0))
            .with_drag_and_drop(true),
        ..Default::default()
    };

    let path_owned = path.map(|s| s.to_string());
    eframe::run_native(
        "rsmpeg",
        options,
        Box::new(|_cc| Ok(Box::new(MediaApp::new(path_owned)))),
    )?;
    Ok(())
}
