//! egui/eframe GUI media player for rsmpeg.
//!
//! Provides a native window with video display, play/pause/stop controls,
//! a draggable seek timeline, volume, open-file dialog, and drag-and-drop
//! file loading.

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
        if let Some(engine) = self.engine.take() {
            engine.stop();
            drop(engine);
        }
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
        // Drain decoded video frames from the engine — only upload the *latest*
        // to the GPU (uploading every intermediate frame causes stutter).
        let mut clear_engine = false;
        let mut got_new_frame = false;
        if let Some(engine) = self.engine.as_ref() {
            let mut latest: Option<state::FrameData> = None;
            let mut disconnected = false;
            loop {
                match engine.frame_rx.try_recv() {
                    Ok(frame) => {
                        latest = Some(frame);
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }

            if let Some(frame) = latest {
                got_new_frame = true;
                let color_image = egui::ColorImage::from_rgba_unmultiplied(
                    [frame.width, frame.height],
                    &frame.rgba,
                );
                // Reuse GPU texture when resolution is unchanged (much cheaper
                // than load_texture every frame).
                match self.texture.as_mut() {
                    Some(tex) if tex.size() == [frame.width, frame.height] => {
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
                self._latest_frame = Some(frame);
            }

            // Check if engine thread exited without ever sending a frame
            if disconnected && self._latest_frame.is_none() {
                let s = state::lock_state(&self.state);
                if s.status != "ended" && s.status != "stopped" && !s.status.is_empty() {
                    self.status = format!("Playback error: {}", s.status);
                } else {
                    self.status = "Error: Could not open or decode file. Try another file.".into();
                }
                self.file_path = None;
                clear_engine = true;
            }

            // Check if playback ended (poison-safe)
            let s = state::lock_state(&self.state);
            if s.status == "ended" {
                self.status = "Playback complete".into();
                clear_engine = disconnected;
            } else if s.status == "stopped" {
                clear_engine = disconnected;
            }
        }

        if clear_engine {
            self.engine = None;
            ctx.request_repaint();
        }

        // Render the UI
        ui::render_ui(self, ctx);

        // Cap UI refresh ~60–120 Hz while playing.  Continuous request_repaint()
        // maxes out a core and fights the decoder thread.
        if self.engine.is_some() {
            if got_new_frame {
                ctx.request_repaint();
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(8));
            }
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
