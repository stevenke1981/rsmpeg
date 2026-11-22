//! Dark cinema theme palette and typography tokens for `rsmpeg-gui`.

use gpui::{rgb, rgba, Hsla, Rgba};

#[cfg(target_os = "windows")]
pub const UI_FONT_FAMILY: &str = "Microsoft JhengHei UI";

#[cfg(target_os = "macos")]
pub const UI_FONT_FAMILY: &str = "PingFang TC";

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub const UI_FONT_FAMILY: &str = "Noto Sans CJK TC";

#[cfg(target_os = "windows")]
pub const MONO_FONT_FAMILY: &str = "Consolas";

#[cfg(not(target_os = "windows"))]
pub const MONO_FONT_FAMILY: &str = "monospace";

pub fn bg_canvas() -> Rgba {
    rgb(0x090c11)
}

pub fn bg_window() -> Rgba {
    rgb(0x0f131b)
}

pub fn bg_surface() -> Rgba {
    rgb(0x141923)
}

pub fn bg_elevated() -> Rgba {
    rgb(0x1c2331)
}

pub fn bg_hover() -> Rgba {
    rgb(0x263044)
}

pub fn bg_active() -> Rgba {
    rgb(0x2f3d57)
}

pub fn border_subtle() -> Rgba {
    rgb(0x232c3d)
}

pub fn border_strong() -> Rgba {
    rgb(0x364561)
}

pub fn accent() -> Rgba {
    rgb(0x38bdf8)
}

pub fn accent_hover() -> Rgba {
    rgb(0x7dd3fc)
}

pub fn accent_soft() -> Rgba {
    rgba(0x38bdf828)
}

pub fn accent_text_on_fill() -> Rgba {
    rgb(0x06131c)
}

pub fn text_primary() -> Rgba {
    rgb(0xf1f5f9)
}

pub fn text_secondary() -> Rgba {
    rgb(0x94a3b8)
}

pub fn text_muted() -> Rgba {
    rgb(0x64748b)
}

pub fn status_playing() -> Rgba {
    rgb(0x34d399)
}

pub fn status_paused() -> Rgba {
    rgb(0xfbbf24)
}

pub fn error_bg() -> Rgba {
    rgb(0x2c1419)
}

pub fn error_border() -> Rgba {
    rgb(0xef4444)
}

pub fn error_text() -> Rgba {
    rgb(0xfecaca)
}

pub fn warn_bg() -> Rgba {
    rgb(0x2b2110)
}

pub fn warn_border() -> Rgba {
    rgb(0xf59e0b)
}

pub fn warn_text() -> Rgba {
    rgb(0xfde68a)
}

pub fn track_bg() -> Rgba {
    rgb(0x273247)
}

pub fn waveform_bar_color(level: f32) -> Hsla {
    let clamped = level.clamp(0.0, 1.0);
    // Shift gently from deep cyan to bright aqua-sky as amplitude rises.
    let base: Hsla = accent().into();
    Hsla {
        h: base.h,
        s: 0.78,
        l: 0.38 + clamped * 0.28,
        a: 0.45 + clamped * 0.55,
    }
}
