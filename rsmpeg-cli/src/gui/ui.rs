//! egui UI layout for the rsmpeg media player.
//!
//! Provides two views:
//! - **Welcome screen** — shown when no file is loaded.
//! - **Player view** — video display + control bar.

use super::MediaApp;
use eframe::egui::{self, vec2, Color32, Frame, Margin, Rounding};

// ---------------------------------------------------------------------------
// Top-level dispatcher
// ---------------------------------------------------------------------------

pub fn render_ui(app: &mut MediaApp, ctx: &egui::Context) {
    if app.file_path.is_some() {
        render_player(app, ctx);
    } else {
        render_welcome(app, ctx);
    }
}

// ---------------------------------------------------------------------------
// Welcome screen
// ---------------------------------------------------------------------------

fn render_welcome(app: &mut MediaApp, ctx: &egui::Context) {
    egui::CentralPanel::default().show(ctx, |ui| {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.3);

            ui.heading("rsmpeg");
            ui.label("Pure Rust Multimedia Player");
            ui.add_space(10.0);
            ui.label(&app.status);
            ui.add_space(10.0);

            if ui.button("Open Media File").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter(
                        "Media",
                        &["mp4", "mkv", "avi", "flac", "mp3", "wav", "ogg", "m4a"],
                    )
                    .pick_file()
                {
                    app.load_file(&path.to_string_lossy());
                }
            }
        });
    });
}

// ---------------------------------------------------------------------------
// Player view
// ---------------------------------------------------------------------------

fn render_player(app: &mut MediaApp, ctx: &egui::Context) {
    // Read shared state
    let state = app.state.clone();
    let state_ref = state.lock().unwrap();
    let playing = state_ref.playing;
    let pos = state_ref.position_sec;
    let dur = state_ref.duration_sec;
    let status = state_ref.status.clone();
    drop(state_ref);

    egui::CentralPanel::default()
        .frame(Frame::none().fill(Color32::BLACK))
        .show(ctx, |ui| {
            let avail = ui.available_size();

            // ── Video area (leave ~50 px at bottom for controls) ──
            let video_height = (avail.y - 50.0).max(100.0);
            let video_rect =
                egui::Rect::from_min_size(ui.cursor().min, vec2(avail.x, video_height));

            if let Some(ref tex) = app.texture {
                let tex_size = tex.size_vec2();
                // Maintain aspect ratio, centered
                let scale = (video_rect.width() / tex_size.x)
                    .min(video_rect.height() / tex_size.y)
                    .min(1.0); // never upscale
                let scaled = tex_size * scale;
                let offset = vec2(
                    ((video_rect.width() - scaled.x) / 2.0).max(0.0),
                    ((video_rect.height() - scaled.y) / 2.0).max(0.0),
                );

                let (resp, painter) = ui.allocate_painter(video_rect.size(), egui::Sense::hover());
                let image_rect = egui::Rect::from_min_size(resp.rect.min + offset, scaled);

                // Black bars
                painter.rect_filled(resp.rect, 0.0, Color32::BLACK);
                // Video frame
                painter.image(
                    tex.id(),
                    image_rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            } else {
                // Loading placeholder
                ui.allocate_ui_with_layout(
                    vec2(avail.x, video_height),
                    egui::Layout::centered_and_justified(egui::Direction::TopDown),
                    |ui| {
                        ui.colored_label(Color32::GRAY, "Loading video...");
                    },
                );
            }

            // ── Control bar ──
            ui.add_space(4.0);

            egui::Frame::none()
                .fill(Color32::from_rgb(30, 30, 30))
                .rounding(Rounding::same(4.0))
                .inner_margin(Margin::symmetric(8.0, 6.0))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        // --- Play/Pause ---
                        let play_label = if playing {
                            "\u{23F8} Pause"
                        } else {
                            "\u{25B6} Play"
                        };
                        if ui.button(play_label).clicked() {
                            let mut s = app.state.lock().unwrap();
                            s.playing = !s.playing;
                        }

                        // --- Stop ---
                        if ui.button("\u{23F9} Stop").clicked() {
                            let mut s = app.state.lock().unwrap();
                            s.playing = false;
                            s.position_sec = 0.0;
                            app._latest_frame = None;
                            app.texture = None;
                            app.engine = None;
                            app.file_path = None;
                            app.status = "Stopped. Open a media file to start playback.".into();
                        }

                        // --- Seek progress bar (read-only for MVP) ---
                        let seek_f = if dur > 0.0 {
                            (pos / dur).clamp(0.0, 1.0) as f32
                        } else {
                            0.0
                        };
                        ui.add(
                            egui::ProgressBar::new(seek_f)
                                .desired_width(ui.available_width().max(80.0).min(300.0)),
                        );

                        // --- Time display ---
                        let time_str = format!(
                            "{:02}:{:02} / {:02}:{:02}",
                            (pos as u32) / 60,
                            (pos as u32) % 60,
                            (dur as u32) / 60,
                            (dur as u32) % 60,
                        );
                        ui.label(time_str);

                        // --- Volume ---
                        ui.label("\u{1F50A}");
                        let mut vol = app.volume;
                        let resp = ui.add(egui::Slider::new(&mut vol, 0.0..=1.0).text(""));
                        if resp.changed() {
                            app.volume = vol;
                            let mut s = app.state.lock().unwrap();
                            s.volume = vol;
                        }

                        // --- Open file ---
                        if ui.button("\u{1F4C2}").clicked() {
                            if let Some(path) = rfd::FileDialog::new()
                                .add_filter(
                                    "Media",
                                    &["mp4", "mkv", "avi", "flac", "mp3", "wav", "ogg", "m4a"],
                                )
                                .pick_file()
                            {
                                app.load_file(&path.to_string_lossy());
                            }
                        }

                        // --- Status ---
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(&status);
                        });
                    });
                });
        });
}
