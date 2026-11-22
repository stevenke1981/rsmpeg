//! Collapsible right-hand inspector drawer: Media Info (rsmpeg probe) and Playlist.

use gpui::{
    div, prelude::*, px, AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    Styled,
};

use crate::format_time::{
    format_bitrate, format_channels, format_duration, format_file_size, format_sample_rate,
};
use crate::i18n::UiStrings;
use crate::media_info::{MediaInfo, StreamMediaKind, StreamSummary};
use crate::state::{PlaybackUiState, SidePanel};
use crate::ui::{theme, PlayerView};

/// Render the active right-hand side panel, if any.
pub fn render_side_panel(
    state: &PlaybackUiState,
    strings: &UiStrings,
    cx: &mut Context<PlayerView>,
) -> Option<AnyElement> {
    match state.active_panel {
        SidePanel::None => None,
        SidePanel::MediaInfo => Some(render_media_info_panel(state, strings, cx)),
        SidePanel::Playlist => Some(render_playlist_panel(state, strings, cx)),
    }
}

fn render_panel_shell(
    title: String,
    header_actions: Option<AnyElement>,
    body: AnyElement,
    cx: &mut Context<PlayerView>,
) -> AnyElement {
    div()
        .flex()
        .flex_none()
        .flex_col()
        .w(px(336.0))
        .h_full()
        .bg(theme::bg_surface())
        .border_l_1()
        .border_color(theme::border_subtle())
        .child(
            div()
                .flex()
                .flex_none()
                .flex_row()
                .items_center()
                .justify_between()
                .gap_2()
                .px_3p5()
                .py_2p5()
                .border_b_1()
                .border_color(theme::border_subtle())
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::text_primary())
                        .child(title),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1p5()
                        .when_some(header_actions, |el, act| el.child(act))
                        .child(
                            div()
                                .id("close-side-panel")
                                .px_1p5()
                                .py_0p5()
                                .rounded_sm()
                                .bg(theme::bg_elevated())
                                .hover(|s| s.bg(theme::bg_hover()))
                                .text_xs()
                                .text_color(theme::text_secondary())
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.focus_root(window);
                                    this.close_side_panel(cx);
                                }))
                                .child("✕"),
                        ),
                ),
        )
        .child(body)
        .into_any_element()
}

fn render_media_info_panel(
    state: &PlaybackUiState,
    strings: &UiStrings,
    cx: &mut Context<PlayerView>,
) -> AnyElement {
    let body: AnyElement = if state.is_probing {
        div()
            .flex()
            .flex_1()
            .items_center()
            .justify_center()
            .p_6()
            .text_sm()
            .text_color(theme::text_secondary())
            .child(strings.info_probing)
            .into_any_element()
    } else if let Some(info) = state.media_info.as_ref() {
        render_media_info_content(info, state, strings)
    } else {
        div()
            .flex()
            .flex_1()
            .items_center()
            .justify_center()
            .p_6()
            .text_sm()
            .text_color(theme::text_muted())
            .text_center()
            .child(strings.info_empty)
            .into_any_element()
    };

    render_panel_shell(strings.info_panel_title.to_string(), None, body, cx)
}

fn render_media_info_content(
    info: &MediaInfo,
    state: &PlaybackUiState,
    strings: &UiStrings,
) -> AnyElement {
    let container_text = info
        .container_format
        .as_deref()
        .map(str::to_ascii_uppercase)
        .unwrap_or_else(|| strings.info_unknown.to_string());
    let file_size_text = info
        .file_size_bytes
        .map(format_file_size)
        .unwrap_or_else(|| strings.info_unknown.to_string());
    let effective_duration = if !info.duration.is_zero() {
        info.duration
    } else {
        state.duration
    };
    let duration_text = if !effective_duration.is_zero() {
        format_duration(effective_duration)
    } else {
        strings.info_unknown.to_string()
    };
    let bitrate_text = if info.bit_rate > 0 {
        format_bitrate(info.bit_rate)
    } else {
        strings.info_unknown.to_string()
    };

    let stream_cards: Vec<AnyElement> = info
        .streams
        .iter()
        .map(|st| render_stream_card(st, state.video_resolution, strings))
        .collect();

    div()
        .id("media-info-scroll")
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .p_3()
        .gap_3()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .p_3()
                .rounded_lg()
                .bg(theme::bg_elevated())
                .border_1()
                .border_color(theme::border_subtle())
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::accent())
                        .child(strings.info_section_container),
                )
                .child(info_row(strings.info_file_name, info.file_name.clone()))
                .child(info_row(strings.info_container, container_text))
                .child(info_row(strings.info_file_size, file_size_text))
                .child(info_row(strings.info_duration, duration_text))
                .child(info_row(strings.info_bitrate, bitrate_text))
                .child(info_row(
                    strings.info_stream_count,
                    info.streams.len().to_string(),
                )),
        )
        .when_some(info.probe_warning.clone(), |el, warn| {
            el.child(
                div()
                    .p_2p5()
                    .rounded_lg()
                    .bg(theme::warn_bg())
                    .border_1()
                    .border_color(theme::warn_border())
                    .text_xs()
                    .text_color(theme::warn_text())
                    .child(warn),
            )
        })
        .when(!stream_cards.is_empty(), |el| {
            el.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .px_0p5()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme::text_secondary())
                            .child(strings.info_section_streams),
                    )
                    .children(stream_cards),
            )
        })
        .into_any_element()
}

fn render_stream_card(
    stream: &StreamSummary,
    decoded_video_res: Option<(usize, usize)>,
    strings: &UiStrings,
) -> AnyElement {
    let kind_label = match stream.kind {
        StreamMediaKind::Video => strings.stream_video,
        StreamMediaKind::Audio => strings.stream_audio,
        StreamMediaKind::Subtitle => strings.stream_subtitle,
        StreamMediaKind::Data => strings.stream_data,
        StreamMediaKind::Other => strings.stream_other,
    };

    let resolution_text = match (stream.width, stream.height) {
        (Some(w), Some(h)) => Some(format!("{w} × {h}")),
        _ if stream.kind == StreamMediaKind::Video => {
            decoded_video_res.map(|(w, h)| format!("{w} × {h}"))
        }
        _ => None,
    };

    let mut card = div()
        .flex()
        .flex_col()
        .gap_1p5()
        .p_3()
        .rounded_lg()
        .bg(theme::bg_elevated())
        .border_1()
        .border_color(theme::border_subtle())
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1p5()
                        .child(
                            div()
                                .px_1p5()
                                .py_0p5()
                                .rounded_sm()
                                .bg(theme::accent_soft())
                                .text_xs()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme::accent())
                                .child(kind_label),
                        )
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(theme::text_primary())
                                .child(format!("#{}", stream.index)),
                        ),
                )
                .child(
                    div()
                        .font_family(theme::MONO_FONT_FAMILY)
                        .text_xs()
                        .text_color(theme::text_secondary())
                        .child(stream.codec_name.to_ascii_uppercase()),
                ),
        )
        .child(info_row(
            strings.info_codec,
            stream.codec_name.to_ascii_uppercase(),
        ));

    if let Some(res) = resolution_text {
        card = card.child(info_row(strings.info_resolution, res));
    }
    if let Some(fps) = stream.fps {
        card = card.child(info_row(strings.info_frame_rate, format!("{fps:.2} fps")));
    }
    if let Some(ref pix) = stream.pixel_format {
        card = card.child(info_row(strings.info_pixel_format, pix.clone()));
    }
    if let Some(sr) = stream.sample_rate {
        card = card.child(info_row(strings.info_sample_rate, format_sample_rate(sr)));
    }
    if let Some(ch) = stream.channels {
        card = card.child(info_row(strings.info_channels, format_channels(ch)));
    }
    if let Some(ref sf) = stream.sample_format {
        card = card.child(info_row(strings.info_sample_format, sf.clone()));
    }
    if let Some(br) = stream.bit_rate {
        card = card.child(info_row(strings.info_bitrate, format_bitrate(br)));
    }
    if !stream.duration.is_zero() {
        card = card.child(info_row(
            strings.info_duration,
            format_duration(stream.duration),
        ));
    }
    for (k, v) in &stream.metadata {
        card = card.child(info_row(k.clone(), v.clone()));
    }

    card.into_any_element()
}

fn info_row(label: impl Into<String>, value: impl Into<String>) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .items_start()
        .justify_between()
        .gap_2()
        .child(
            div()
                .flex_none()
                .text_xs()
                .text_color(theme::text_secondary())
                .child(label.into()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_right()
                .text_xs()
                .text_color(theme::text_primary())
                .truncate()
                .child(value.into()),
        )
        .into_any_element()
}

fn render_playlist_panel(
    state: &PlaybackUiState,
    strings: &UiStrings,
    cx: &mut Context<PlayerView>,
) -> AnyElement {
    let count = state.playlist.len();
    let title = format!("{} ({count})", strings.playlist_title);

    let header_actions = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1p5()
        .child(
            div()
                .id("playlist-add-btn")
                .px_2()
                .py_0p5()
                .rounded_sm()
                .bg(theme::accent_soft())
                .hover(|s| s.bg(theme::bg_hover()))
                .text_xs()
                .text_color(theme::accent())
                .cursor_pointer()
                .on_click(cx.listener(|this, _, window, cx| {
                    this.focus_root(window);
                    this.open_file_dialog_inner(false, window, cx);
                }))
                .child(format!("+ {}", strings.playlist_add)),
        )
        .when(count > 0, |el| {
            el.child(
                div()
                    .id("playlist-clear-btn")
                    .px_2()
                    .py_0p5()
                    .rounded_sm()
                    .bg(theme::bg_elevated())
                    .hover(|s| s.bg(theme::bg_hover()))
                    .text_xs()
                    .text_color(theme::text_secondary())
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.focus_root(window);
                        this.clear_playlist(window, cx);
                    }))
                    .child(strings.playlist_clear),
            )
        })
        .into_any_element();

    let current_idx = state.playlist.current_index();

    let body: AnyElement = if state.playlist.is_empty() {
        div()
            .flex()
            .flex_1()
            .items_center()
            .justify_center()
            .p_6()
            .text_sm()
            .text_color(theme::text_muted())
            .text_center()
            .child(strings.playlist_empty)
            .into_any_element()
    } else {
        let rows: Vec<AnyElement> = state
            .playlist
            .items()
            .iter()
            .enumerate()
            .map(|(idx, item)| {
                let is_active = current_idx == Some(idx);
                let name = item.display_name.clone();
                let now_playing_label = strings.playlist_now_playing;

                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_2p5()
                    .py_2()
                    .rounded_lg()
                    .bg(if is_active {
                        theme::accent_soft()
                    } else {
                        theme::bg_elevated()
                    })
                    .border_1()
                    .border_color(if is_active {
                        theme::accent()
                    } else {
                        theme::border_subtle()
                    })
                    .child(
                        div()
                            .id(("playlist-row", idx))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .flex_1()
                            .min_w_0()
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.focus_root(window);
                                this.play_index(idx, window, cx);
                            }))
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(20.0))
                                    .font_family(theme::MONO_FONT_FAMILY)
                                    .text_xs()
                                    .text_color(if is_active {
                                        theme::accent()
                                    } else {
                                        theme::text_muted()
                                    })
                                    .child(if is_active {
                                        "▶".to_string()
                                    } else {
                                        format!("{}", idx + 1)
                                    }),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(if is_active {
                                                gpui::FontWeight::SEMIBOLD
                                            } else {
                                                gpui::FontWeight::NORMAL
                                            })
                                            .text_color(theme::text_primary())
                                            .truncate()
                                            .child(name),
                                    )
                                    .when(is_active, |el| {
                                        el.child(
                                            div()
                                                .text_xs()
                                                .text_color(theme::accent())
                                                .child(now_playing_label),
                                        )
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .id(("playlist-remove", idx))
                            .flex_none()
                            .px_1p5()
                            .py_0p5()
                            .rounded_sm()
                            .hover(|s| s.bg(theme::bg_hover()))
                            .text_xs()
                            .text_color(theme::text_muted())
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.focus_root(window);
                                this.remove_playlist_item(idx, window, cx);
                            }))
                            .child("✕"),
                    )
                    .into_any_element()
            })
            .collect();

        div()
            .id("playlist-scroll")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_3()
            .gap_2()
            .children(rows)
            .into_any_element()
    };

    render_panel_shell(title, Some(header_actions), body, cx)
}
