//! egui/eframe GUI media player for rsmpeg.
//!
//! Provides a native window with video display, play/pause/stop controls,
//! seek progress, volume, and an open-file dialog.

pub mod engine;
pub mod state;
pub mod ui;

use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use eframe::egui;
use engine::PlaybackEngine;
use state::FrameData;

// ---------------------------------------------------------------------------
// Application
// ---------------------------------------------------------------------------

/// Main egui application state.
pub struct MediaApp {
    /// Current playback engine (None when stopped / no file).
    engine: Option<PlaybackEngine>,
    /// Shared playback state (cross-thread).
    state: Arc<Mutex<state::PlaybackState>>,
    /// Latest decoded frame for display.
    _latest_frame: Option<FrameData>,
    /// egui texture handle for the current video frame.
    texture: Option<egui::TextureHandle>,
    /// Current file path (set when a file is loaded).
    file_path: Option<String>,
    /// Status message shown in the UI.
    status: String,
    /// Audio volume (0.0 – 1.0).
    volume: f32,
}

impl MediaApp {
    /// Create a new media player.  If `path` is provided, immediately
    /// starts loading that file.
    pub fn new(path: Option<String>) -> Self {
        let mut app = Self {
            engine: None,
            state: Arc::new(Mutex::new(state::PlaybackState::default())),
            _latest_frame: None,
            texture: None,
            file_path: None,
            status: "Open a media file to start playback".into(),
            volume: 0.8,
        };
        if let Some(p) = path {
            app.load_file(&p);
        }
        app
    }

    /// Load (or reload) a media file, stopping any current playback.
    pub fn load_file(&mut self, path: &str) {
        // Stop existing playback
        self.engine = None;
        self._latest_frame = None;
        self.texture = None;

        match PlaybackEngine::new(path) {
            Ok(engine) => {
                self.state = engine.state.clone();
                self.file_path = Some(path.to_string());
                self.status = format!("Playing: {}", path);
                self.engine = Some(engine);
            }
            Err(e) => {
                self.status = format!("Error: {}", e);
            }
        }
    }
}

impl eframe::App for MediaApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Drain decoded video frames from the engine
        if let Some(ref engine) = self.engine {
            let mut got_any_frame = false;
            while let Ok(frame) = engine.frame_rx.try_recv() {
                got_any_frame = true;
                let color_image = egui::ColorImage::from_rgba_unmultiplied(
                    [frame.width as usize, frame.height as usize],
                    &frame.rgba,
                );
                self.texture = Some(ctx.load_texture(
                    "video_frame",
                    color_image,
                    egui::TextureOptions::LINEAR,
                ));
                self._latest_frame = Some(frame);
            }

            // Check if engine thread exited without ever sending a frame
            if !got_any_frame && self._latest_frame.is_none() {
                match engine.frame_rx.try_recv() {
                    Err(mpsc::TryRecvError::Disconnected) => {
                        // Engine died before producing any output
                        let s = state::lock_state(&self.state);
                        if s.status != "ended" && !s.status.is_empty() {
                            self.status = format!("Playback error: {}", s.status);
                        } else {
                            self.status =
                                "Error: Could not open or decode file. Try another file.".into();
                        }
                        self.file_path = None;
                        self.engine = None;
                        ctx.request_repaint();
                    }
                    _ => {}
                }
            }

            // Check if playback ended (poison-safe)
            if self.engine.is_some() {
                let s = state::lock_state(&self.state);
                if s.status == "ended" {
                    self.status = "Playback complete".into();
                }
            }
        }

        // Render the UI
        ui::render_ui(self, ctx);

        // Keep repainting while playing (drives video updates)
        if self.engine.is_some() {
            ctx.request_repaint();
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Launch the egui GUI window.
///
/// If `path` is `Some(...)`, opens that media file on startup.
pub fn run_gui(path: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(egui::vec2(960.0, 600.0))
            .with_min_inner_size(egui::vec2(480.0, 320.0)),
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
