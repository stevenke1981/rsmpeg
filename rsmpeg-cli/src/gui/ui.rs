//! egui UI layout for the rsmpeg media player.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::MediaApp;
use eframe::egui::{self, vec2, Color32, Frame, Margin, Rounding, Stroke};

const MEDIA_EXTENSIONS: &[&str] = &["mp4", "mkv", "avi", "flac", "mp3", "wav", "ogg", "m4a"];

pub fn render_ui(app: &mut MediaApp, ctx: &egui::Context) {
    handle_file_drop(app, ctx);
    if app.file_path.is_some() {
        render_player(app, ctx);
    } else {
        render_welcome(app, ctx);
    }
}

fn is_media_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            MEDIA_EXTENSIONS
                .iter()
                .any(|allowed| ext.eq_ignore_ascii_case(allowed))
        })
}

fn handle_file_drop(app: &mut MediaApp, ctx: &egui::Context) {
    let dropped: Vec<egui::DroppedFile> = ctx.input(|i| i.raw.dropped_files.clone());
    if dropped.is_empty() {
        return;
    }
    let path: Option<PathBuf> = dropped
        .iter()
        .filter_map(|f| f.path.clone())
        .find(|p| is_media_path(p))
        .or_else(|| dropped.into_iter().find_map(|f| f.path));
    if let Some(path) = path {
        app.load_file(&path.to_string_lossy());
    }
}

fn hovering_files(ctx: &egui::Context) -> bool {
    ctx.input(|i| !i.raw.hovered_files.is_empty())
}

fn render_welcome(app: &mut MediaApp, ctx: &egui::Context) {
    let hovering = hovering_files(ctx);
    egui::CentralPanel::default().show(ctx, |ui| {
        if hovering {
            let rect = ui.max_rect();
            ui.painter().rect(
                rect,
                Rounding::same(8.0),
                Color32::from_rgba_unmultiplied(40, 80, 140, 40),
                Stroke::new(2.0, Color32::from_rgb(80, 160, 255)),
            );
        }
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.28);
            ui.heading("rsmpeg");
            ui.label("Pure Rust Multimedia Player");
            ui.add_space(10.0);
            ui.label(&app.status);
            ui.add_space(14.0);
            if ui.button("Open Media File").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Media", MEDIA_EXTENSIONS)
                    .pick_file()
                {
                    app.load_file(&path.to_string_lossy());
                }
            }
            ui.add_space(16.0);
            if hovering {
                ui.colored_label(
                    Color32::from_rgb(120, 190, 255),
                    "Release to open media file",
                );
            } else {
                ui.colored_label(Color32::GRAY, "or drag & drop a video / audio file here");
            }
        });
    });
}

fn render_player(app: &mut MediaApp, ctx: &egui::Context) {
    let playing = app.playing;
    let pos = app.position_sec;
    let dur = app.duration_sec;
    let status = app.status.clone();
    let hovering = hovering_files(ctx);

    egui::CentralPanel::default()
        .frame(Frame::none().fill(Color32::BLACK))
        .show(ctx, |ui| {
            let avail = ui.available_size();
            let video_height = (avail.y - 72.0).max(100.0);
            let video_rect =
                egui::Rect::from_min_size(ui.cursor().min, vec2(avail.x, video_height));

            if let Some(ref tex) = app.texture {
                let tex_size = tex.size_vec2();
                let scale = (video_rect.width() / tex_size.x)
                    .min(video_rect.height() / tex_size.y)
                    .min(1.0);
                let scaled = tex_size * scale;
                let offset = vec2(
                    ((video_rect.width() - scaled.x) / 2.0).max(0.0),
                    ((video_rect.height() - scaled.y) / 2.0).max(0.0),
                );
                let (resp, painter) = ui.allocate_painter(video_rect.size(), egui::Sense::hover());
                let image_rect = egui::Rect::from_min_size(resp.rect.min + offset, scaled);
                painter.rect_filled(resp.rect, 0.0, Color32::BLACK);
                painter.image(
                    tex.id(),
                    image_rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            } else {
                ui.allocate_ui_with_layout(
                    vec2(avail.x, video_height),
                    egui::Layout::centered_and_justified(egui::Direction::TopDown),
                    |ui| {
                        ui.colored_label(Color32::GRAY, "Loading video...");
                    },
                );
            }

            if hovering {
                let tip = "Drop file to open";
                let galley = ui.painter().layout_no_wrap(
                    tip.to_owned(),
                    egui::TextStyle::Heading.resolve(ui.style()),
                    Color32::from_rgb(180, 220, 255),
                );
                let pos = video_rect.center() - galley.size() * 0.5;
                ui.painter().rect_filled(
                    video_rect,
                    0.0,
                    Color32::from_rgba_unmultiplied(0, 0, 0, 120),
                );
                ui.painter().galley(pos, galley, Color32::WHITE);
            }

            ui.add_space(4.0);
            egui::Frame::none()
                .fill(Color32::from_rgb(30, 30, 30))
                .rounding(Rounding::same(4.0))
                .inner_margin(Margin::symmetric(8.0, 6.0))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(format_time(pos));
                        let mut fraction = if dur > 0.0 {
                            (pos / dur).clamp(0.0, 1.0) as f32
                        } else {
                            0.0
                        };
                        let timeline_w = (ui.available_width() - 56.0).max(80.0);
                        let slider = ui.add_sized(
                            [timeline_w, 18.0],
                            egui::Slider::new(&mut fraction, 0.0..=1.0)
                                .show_value(false)
                                .clamp_to_range(true)
                                .trailing_fill(true),
                        );
                        if slider.changed() && dur > 0.0 {
                            let target = (fraction as f64) * dur;
                            app.position_sec = target;
                            if slider.drag_stopped() || !slider.dragged() {
                                if let Some(p) = app.player.as_mut() {
                                    let _ = p.seek(Duration::from_secs_f64(target));
                                }
                            }
                        }
                        if slider.drag_stopped() && dur > 0.0 {
                            let target = (fraction as f64) * dur;
                            app.position_sec = target;
                            if let Some(p) = app.player.as_mut() {
                                let _ = p.seek(Duration::from_secs_f64(target));
                            }
                        }
                        ui.label(format_time(dur));
                    });

                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        let play_label = if playing {
                            "\u{23F8} Pause"
                        } else {
                            "\u{25B6} Play"
                        };
                        if ui.button(play_label).clicked() {
                            if let Some(p) = app.player.as_mut() {
                                if playing {
                                    let _ = p.pause();
                                    app.playing = false;
                                } else {
                                    let _ = p.play();
                                    app.playing = true;
                                }
                            }
                        }
                        if ui.button("\u{23F9} Stop").clicked() {
                            app.stop_playback();
                        }
                        ui.label("\u{1F50A}");
                        let mut vol = app.volume;
                        if ui
                            .add(egui::Slider::new(&mut vol, 0.0..=1.0).text(""))
                            .changed()
                        {
                            app.volume = vol;
                            if let Some(p) = app.player.as_mut() {
                                let _ = p.set_volume(vol);
                            }
                        }
                        if ui.button("\u{1F4C2}").clicked() {
                            if let Some(path) = rfd::FileDialog::new()
                                .add_filter("Media", MEDIA_EXTENSIONS)
                                .pick_file()
                            {
                                app.load_file(&path.to_string_lossy());
                            }
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(&status);
                        });
                    });
                });
        });
}

fn format_time(sec: f64) -> String {
    let total = sec.max(0.0) as u64;
    let m = total / 60;
    let s = total % 60;
    if m >= 60 {
        format!("{:02}:{:02}:{:02}", m / 60, m % 60, s)
    } else {
        format!("{:02}:{:02}", m, s)
    }
}
