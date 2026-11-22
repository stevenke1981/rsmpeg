//! Video viewport, audio-only visualizer stage, welcome screen, and alert banners.

use std::sync::Arc;

use gpui::{
    div, img, prelude::*, px, AnyElement, Context, InteractiveElement, IntoElement, MouseButton,
    ObjectFit, ParentElement, RenderImage, Styled,
};

use crate::format_time::{format_channels, format_sample_rate};
use crate::i18n::UiStrings;
use crate::state::{PlaybackStatus, PlaybackUiState};
use crate::ui::{theme, PlayerView};

/// Render the central media viewport (video canvas, audio stage, welcome screen, or error view).
pub fn render_stage(
    state: &PlaybackUiState,
    current_frame: Option<&Arc<RenderImage>>,
    strings: &UiStrings,
    cx: &mut Context<PlayerView>,
) -> AnyElement {
    let has_loaded_item = state.playlist.current_item().is_some();

    let body: AnyElement = if !has_loaded_item && state.error_message.is_none() {
        render_welcome(strings, cx)
    } else if let Some(frame) = current_frame {
        render_video_canvas(frame.clone(), cx)
    } else if state.status == PlaybackStatus::Error {
        render_error_stage(state, strings, cx)
    } else if state.is_audio_only_stage() {
        render_audio_stage(state, strings)
    } else {
        render_loading_stage(state, strings)
    };

    div()
        .relative()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .bg(theme::bg_canvas())
        .overflow_hidden()
        .child(body)
        .when(
            state.error_message.is_some() && current_frame.is_some(),
            |el| {
                let msg = state.error_message.clone().unwrap_or_default();
                el.child(render_floating_banner(
                    strings.error_title,
                    msg,
                    true,
                    strings.dismiss,
                    cx,
                ))
            },
        )
        .when_some(state.warning_message.clone(), |el, msg| {
            el.child(render_floating_banner(
                strings.warning_title,
                msg,
                false,
                strings.dismiss,
                cx,
            ))
        })
        .into_any_element()
}

fn render_video_canvas(frame: Arc<RenderImage>, cx: &mut Context<PlayerView>) -> AnyElement {
    div()
        .id("video-canvas")
        .flex()
        .size_full()
        .items_center()
        .justify_center()
        .bg(theme::bg_canvas())
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, ev: &gpui::MouseDownEvent, window, cx| {
                this.focus_root(window);
                if ev.click_count >= 2 {
                    this.toggle_fullscreen(window, cx);
                }
            }),
        )
        .child(img(frame).size_full().object_fit(ObjectFit::Contain))
        .into_any_element()
}

fn render_welcome(strings: &UiStrings, cx: &mut Context<PlayerView>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .size_full()
        .items_center()
        .justify_center()
        .p_6()
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_4()
                .max_w(px(520.0))
                .p_8()
                .rounded_xl()
                .bg(theme::bg_surface())
                .border_1()
                .border_color(theme::border_subtle())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .w(px(64.0))
                        .h(px(64.0))
                        .rounded_full()
                        .bg(theme::accent_soft())
                        .border_1()
                        .border_color(theme::accent())
                        .text_2xl()
                        .text_color(theme::accent())
                        .child("▶"),
                )
                .child(
                    div()
                        .text_xl()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::text_primary())
                        .child(strings.welcome_title),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(theme::text_secondary())
                        .text_center()
                        .child(strings.welcome_hint),
                )
                .child(
                    div()
                        .id("welcome-open-btn")
                        .mt_2()
                        .px_6()
                        .py_2p5()
                        .rounded_lg()
                        .bg(theme::accent())
                        .hover(|s| s.bg(theme::accent_hover()))
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::accent_text_on_fill())
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_file_dialog(window, cx);
                        }))
                        .child(strings.open_file_btn),
                )
                .child(
                    div()
                        .mt_1()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .text_center()
                        .child(strings.drop_overlay_hint),
                )
                .child(
                    div()
                        .mt_2()
                        .pt_3()
                        .w_full()
                        .border_t_1()
                        .border_color(theme::border_subtle())
                        .text_xs()
                        .text_color(theme::text_muted())
                        .text_center()
                        .child(strings.shortcuts_hint),
                ),
        )
        .into_any_element()
}

fn render_audio_stage(state: &PlaybackUiState, strings: &UiStrings) -> AnyElement {
    let title = state
        .playlist
        .current_item()
        .map(|it| it.display_name.clone())
        .unwrap_or_else(|| strings.no_file_loaded.to_string());

    let detail_line = build_audio_detail_line(state, strings);

    let waveform_bars = state
        .waveform
        .iter()
        .copied()
        .enumerate()
        .map(|(i, level)| {
            // Ensure a subtle baseline floor so silence still reads as an active meter.
            let bar_height = 8.0 + level.clamp(0.0, 1.0) * 108.0;
            div()
                .id(("wave-bar", i))
                .w(px(6.0))
                .h(px(bar_height))
                .rounded_full()
                .bg(theme::waveform_bar_color(level))
        });

    div()
        .flex()
        .flex_col()
        .size_full()
        .items_center()
        .justify_center()
        .p_6()
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_5()
                .w(px(560.0))
                .max_w_full()
                .p_6()
                .rounded_xl()
                .bg(theme::bg_surface())
                .border_1()
                .border_color(theme::border_subtle())
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_4()
                        .w_full()
                        .child(
                            div()
                                .flex()
                                .flex_none()
                                .items_center()
                                .justify_center()
                                .w(px(72.0))
                                .h(px(72.0))
                                .rounded_xl()
                                .bg(theme::bg_elevated())
                                .border_1()
                                .border_color(theme::border_strong())
                                .text_3xl()
                                .text_color(theme::accent())
                                .child("♪"),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w_0()
                                .gap_1p5()
                                .child(
                                    div().flex().items_center().gap_2().child(
                                        div()
                                            .px_2()
                                            .py_0p5()
                                            .rounded_sm()
                                            .bg(theme::accent_soft())
                                            .text_xs()
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .text_color(theme::accent())
                                            .child(strings.audio_only_badge),
                                    ),
                                )
                                .child(
                                    div()
                                        .text_lg()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(theme::text_primary())
                                        .truncate()
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme::text_secondary())
                                        .truncate()
                                        .child(detail_line),
                                ),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_end()
                        .justify_center()
                        .gap(px(4.0))
                        .w_full()
                        .h(px(136.0))
                        .px_4()
                        .py_3()
                        .rounded_lg()
                        .bg(theme::bg_canvas())
                        .border_1()
                        .border_color(theme::border_subtle())
                        .children(waveform_bars),
                ),
        )
        .into_any_element()
}

fn build_audio_detail_line(state: &PlaybackUiState, strings: &UiStrings) -> String {
    let Some(info) = state.media_info.as_ref() else {
        return strings.info_probing.to_string();
    };
    let mut parts = Vec::new();
    if let Some(fmt) = info.container_format.as_deref() {
        parts.push(fmt.to_ascii_uppercase());
    }
    if let Some(audio) = info.primary_audio() {
        parts.push(audio.codec_name.to_ascii_uppercase());
        if let Some(sr) = audio.sample_rate {
            parts.push(format_sample_rate(sr));
        }
        if let Some(ch) = audio.channels {
            parts.push(format_channels(ch));
        }
    }
    if parts.is_empty() {
        strings.audio_only_badge.to_string()
    } else {
        parts.join(" · ")
    }
}

fn render_loading_stage(state: &PlaybackUiState, strings: &UiStrings) -> AnyElement {
    let title = state
        .playlist
        .current_item()
        .map(|it| it.display_name.clone())
        .unwrap_or_default();

    div()
        .flex()
        .flex_col()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .child(
            div()
                .text_base()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::text_primary())
                .child(strings.loading_video),
        )
        .when(!title.is_empty(), |el| {
            el.child(
                div()
                    .text_sm()
                    .text_color(theme::text_secondary())
                    .child(title),
            )
        })
        .into_any_element()
}

fn render_error_stage(
    state: &PlaybackUiState,
    strings: &UiStrings,
    cx: &mut Context<PlayerView>,
) -> AnyElement {
    let detail = state
        .error_message
        .clone()
        .unwrap_or_else(|| strings.error_unsupported_or_corrupt.to_string());
    let file_name = state
        .playlist
        .current_item()
        .map(|it| it.display_name.clone())
        .unwrap_or_default();

    div()
        .flex()
        .flex_col()
        .size_full()
        .items_center()
        .justify_center()
        .p_6()
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_3()
                .max_w(px(520.0))
                .p_6()
                .rounded_xl()
                .bg(theme::error_bg())
                .border_1()
                .border_color(theme::error_border())
                .child(
                    div()
                        .text_lg()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::error_text())
                        .child(strings.error_title),
                )
                .when(!file_name.is_empty(), |el| {
                    el.child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme::text_primary())
                            .child(file_name),
                    )
                })
                .child(
                    div()
                        .text_sm()
                        .text_color(theme::error_text())
                        .text_center()
                        .child(detail),
                )
                .child(
                    div()
                        .mt_2()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .id("error-open-btn")
                                .px_5()
                                .py_2()
                                .rounded_lg()
                                .bg(theme::bg_elevated())
                                .hover(|s| s.bg(theme::bg_hover()))
                                .border_1()
                                .border_color(theme::border_strong())
                                .text_sm()
                                .text_color(theme::text_primary())
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_file_dialog(window, cx);
                                }))
                                .child(strings.open_file_btn),
                        )
                        .when(state.playlist.has_next(), |el| {
                            el.child(
                                div()
                                    .id("error-next-btn")
                                    .px_4()
                                    .py_2()
                                    .rounded_lg()
                                    .bg(theme::accent())
                                    .hover(|s| s.bg(theme::accent_hover()))
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(theme::accent_text_on_fill())
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.play_next(window, cx);
                                    }))
                                    .child(format!("{} ⏭", strings.next_track)),
                            )
                        }),
                ),
        )
        .into_any_element()
}

fn render_floating_banner(
    title: &'static str,
    message: String,
    is_error: bool,
    dismiss_label: &'static str,
    cx: &mut Context<PlayerView>,
) -> AnyElement {
    let (bg, border, text_color) = if is_error {
        (
            theme::error_bg(),
            theme::error_border(),
            theme::error_text(),
        )
    } else {
        (theme::warn_bg(), theme::warn_border(), theme::warn_text())
    };

    div()
        .absolute()
        .top_3()
        .left_3()
        .right_3()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_3()
        .px_3()
        .py_2()
        .rounded_lg()
        .bg(bg)
        .border_1()
        .border_color(border)
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(text_color)
                        .child(title),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_primary())
                        .truncate()
                        .child(message),
                ),
        )
        .child(
            div()
                .id(if is_error {
                    "dismiss-error-banner"
                } else {
                    "dismiss-warn-banner"
                })
                .px_2()
                .py_0p5()
                .rounded_sm()
                .bg(theme::bg_elevated())
                .hover(|s| s.bg(theme::bg_hover()))
                .text_xs()
                .text_color(theme::text_primary())
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.dismiss_banner(is_error, cx);
                }))
                .child(dismiss_label),
        )
        .into_any_element()
}
