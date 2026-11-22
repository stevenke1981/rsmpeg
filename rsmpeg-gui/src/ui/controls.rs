//! Bottom transport bar: draggable seek bar, playback controls, volume/mute, speed, fullscreen.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    canvas, div, prelude::*, px, relative, AnyElement, Bounds, Context, InteractiveElement,
    IntoElement, MouseButton, ParentElement, Pixels, Styled,
};

use crate::format_time::{format_duration, format_rate, format_volume_percent};
use crate::i18n::UiStrings;
use crate::state::{PlaybackStatus, PlaybackUiState};
use crate::ui::{theme, PlayerView};

/// Render the full bottom playback control deck.
pub fn render_controls(
    state: &PlaybackUiState,
    seek_bounds: Rc<Cell<Bounds<Pixels>>>,
    volume_bounds: Rc<Cell<Bounds<Pixels>>>,
    strings: &UiStrings,
    cx: &mut Context<PlayerView>,
) -> AnyElement {
    let has_media = state.playlist.current_item().is_some();
    let is_playing = matches!(
        state.status,
        PlaybackStatus::Playing | PlaybackStatus::Opening
    );

    div()
        .flex()
        .flex_none()
        .flex_col()
        .gap_2()
        .px_4()
        .py_3()
        .bg(theme::bg_surface())
        .border_t_1()
        .border_color(theme::border_subtle())
        .child(render_seek_row(state, seek_bounds, has_media, cx))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap_3()
                .child(render_transport_buttons(
                    state, has_media, is_playing, strings, cx,
                ))
                .child(render_right_controls(state, volume_bounds, strings, cx)),
        )
        .into_any_element()
}

fn render_seek_row(
    state: &PlaybackUiState,
    seek_bounds: Rc<Cell<Bounds<Pixels>>>,
    has_media: bool,
    cx: &mut Context<PlayerView>,
) -> AnyElement {
    let pos_text = format_duration(state.displayed_position());
    let dur_text = format_duration(state.duration);
    let frac = state.progress_fraction().clamp(0.0, 1.0) as f32;
    let can_seek = has_media && !state.duration.is_zero();

    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
        .w_full()
        .child(
            div()
                .flex_none()
                .w(px(58.0))
                .font_family(theme::MONO_FONT_FAMILY)
                .text_xs()
                .text_color(theme::text_primary())
                .child(pos_text),
        )
        .child(
            div()
                .id("seek-bar")
                .relative()
                .flex()
                .flex_1()
                .items_center()
                .h(px(22.0))
                .when(can_seek, |el| el.cursor_pointer())
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, ev: &gpui::MouseDownEvent, window, cx| {
                        this.focus_root(window);
                        this.begin_seek_drag(ev.position.x, cx);
                    }),
                )
                .child(
                    // Background rail
                    div()
                        .relative()
                        .w_full()
                        .h(px(6.0))
                        .rounded_full()
                        .bg(theme::track_bg())
                        .overflow_hidden()
                        .child(
                            div()
                                .h_full()
                                .w(relative(frac))
                                .rounded_full()
                                .bg(theme::accent()),
                        ),
                )
                .when(can_seek, |el| {
                    // Thumb indicator aligned to current fraction
                    el.child(
                        div()
                            .absolute()
                            .left(relative(frac))
                            .ml(px(-7.0))
                            .w(px(14.0))
                            .h(px(14.0))
                            .rounded_full()
                            .bg(theme::text_primary())
                            .border_2()
                            .border_color(theme::accent()),
                    )
                })
                .child(
                    // Invisible bounds tracker for pixel-accurate pointer -> fraction mapping
                    canvas(
                        move |bounds, _window, _cx| {
                            seek_bounds.set(bounds);
                        },
                        |_bounds, (), _window, _cx| {},
                    )
                    .absolute()
                    .inset_0()
                    .size_full(),
                ),
        )
        .child(
            div()
                .flex_none()
                .w(px(58.0))
                .text_right()
                .font_family(theme::MONO_FONT_FAMILY)
                .text_xs()
                .text_color(theme::text_secondary())
                .child(dur_text),
        )
        .into_any_element()
}

fn render_transport_buttons(
    state: &PlaybackUiState,
    has_media: bool,
    is_playing: bool,
    strings: &UiStrings,
    cx: &mut Context<PlayerView>,
) -> AnyElement {
    let has_prev = state.playlist.has_prev();
    let has_next = state.playlist.has_next();

    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .child(
            div()
                .id("btn-prev")
                .px_2p5()
                .py_1p5()
                .rounded_md()
                .bg(theme::bg_elevated())
                .border_1()
                .border_color(theme::border_subtle())
                .text_xs()
                .when(has_prev, |el| {
                    el.text_color(theme::text_primary())
                        .hover(|s| s.bg(theme::bg_hover()))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.focus_root(window);
                            this.play_prev(window, cx);
                        }))
                })
                .when(!has_prev, |el| el.text_color(theme::text_muted()))
                .child(format!("|◀ {}", strings.prev_track)),
        )
        .child(
            div()
                .id("btn-play-pause")
                .px_4()
                .py_1p5()
                .rounded_md()
                .bg(theme::accent())
                .hover(|s| s.bg(theme::accent_hover()))
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::accent_text_on_fill())
                .cursor_pointer()
                .on_click(cx.listener(|this, _, window, cx| {
                    this.focus_root(window);
                    this.toggle_play_pause(window, cx);
                }))
                .child(if is_playing {
                    format!("❚❚ {}", strings.pause)
                } else {
                    format!("▶ {}", strings.play)
                }),
        )
        .child(
            div()
                .id("btn-stop")
                .px_2p5()
                .py_1p5()
                .rounded_md()
                .bg(theme::bg_elevated())
                .border_1()
                .border_color(theme::border_subtle())
                .text_xs()
                .when(has_media, |el| {
                    el.text_color(theme::text_primary())
                        .hover(|s| s.bg(theme::bg_hover()))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.focus_root(window);
                            this.stop_playback(window, cx);
                        }))
                })
                .when(!has_media, |el| el.text_color(theme::text_muted()))
                .child(format!("■ {}", strings.stop)),
        )
        .child(
            div()
                .id("btn-next")
                .px_2p5()
                .py_1p5()
                .rounded_md()
                .bg(theme::bg_elevated())
                .border_1()
                .border_color(theme::border_subtle())
                .text_xs()
                .when(has_next, |el| {
                    el.text_color(theme::text_primary())
                        .hover(|s| s.bg(theme::bg_hover()))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.focus_root(window);
                            this.play_next(window, cx);
                        }))
                })
                .when(!has_next, |el| el.text_color(theme::text_muted()))
                .child(format!("{} ▶|", strings.next_track)),
        )
        .into_any_element()
}

fn render_right_controls(
    state: &PlaybackUiState,
    volume_bounds: Rc<Cell<Bounds<Pixels>>>,
    strings: &UiStrings,
    cx: &mut Context<PlayerView>,
) -> AnyElement {
    let eff_vol = state.effective_volume();
    let vol_pct = format_volume_percent(eff_vol);
    let mute_label = if state.muted || eff_vol <= 0.001 {
        format!("× {}", strings.unmute)
    } else {
        format!("♪ {}", strings.mute)
    };
    let rate_text = format_rate(state.playback_rate);
    let fs_label = if state.is_fullscreen {
        strings.fullscreen_exit
    } else {
        strings.fullscreen_enter
    };

    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2p5()
        .child(
            // Mute toggle button
            div()
                .id("btn-mute")
                .px_2()
                .py_1()
                .rounded_md()
                .bg(theme::bg_elevated())
                .hover(|s| s.bg(theme::bg_hover()))
                .border_1()
                .border_color(theme::border_subtle())
                .text_xs()
                .text_color(if state.muted {
                    theme::warn_text()
                } else {
                    theme::text_primary()
                })
                .cursor_pointer()
                .on_click(cx.listener(|this, _, window, cx| {
                    this.focus_root(window);
                    this.toggle_mute(cx);
                }))
                .child(mute_label),
        )
        .child(
            // Draggable volume slider
            div()
                .id("volume-slider")
                .relative()
                .flex()
                .items_center()
                .w(px(92.0))
                .h(px(20.0))
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, ev: &gpui::MouseDownEvent, window, cx| {
                        this.focus_root(window);
                        this.begin_volume_drag(ev.position.x, cx);
                    }),
                )
                .child(
                    div()
                        .relative()
                        .w_full()
                        .h(px(5.0))
                        .rounded_full()
                        .bg(theme::track_bg())
                        .overflow_hidden()
                        .child(div().h_full().w(relative(eff_vol)).rounded_full().bg(
                            if state.muted {
                                theme::text_muted()
                            } else {
                                theme::accent()
                            },
                        )),
                )
                .child(
                    div()
                        .absolute()
                        .left(relative(eff_vol))
                        .ml(px(-5.5))
                        .w(px(11.0))
                        .h(px(11.0))
                        .rounded_full()
                        .bg(theme::text_primary())
                        .border_1()
                        .border_color(theme::accent()),
                )
                .child(
                    canvas(
                        move |bounds, _window, _cx| {
                            volume_bounds.set(bounds);
                        },
                        |_bounds, (), _window, _cx| {},
                    )
                    .absolute()
                    .inset_0()
                    .size_full(),
                ),
        )
        .child(
            div()
                .w(px(36.0))
                .font_family(theme::MONO_FONT_FAMILY)
                .text_xs()
                .text_color(theme::text_secondary())
                .child(vol_pct),
        )
        .child(
            // Playback speed selector button
            div()
                .id("btn-speed")
                .px_2p5()
                .py_1()
                .rounded_md()
                .bg(theme::bg_elevated())
                .hover(|s| s.bg(theme::bg_hover()))
                .border_1()
                .border_color(if (state.playback_rate - 1.0).abs() > 0.01 {
                    theme::accent()
                } else {
                    theme::border_subtle()
                })
                .font_family(theme::MONO_FONT_FAMILY)
                .text_xs()
                .text_color(if (state.playback_rate - 1.0).abs() > 0.01 {
                    theme::accent()
                } else {
                    theme::text_primary()
                })
                .cursor_pointer()
                .on_click(cx.listener(|this, _, window, cx| {
                    this.focus_root(window);
                    this.cycle_playback_rate(cx);
                }))
                .child(rate_text),
        )
        .child(
            // Fullscreen toggle button
            div()
                .id("btn-fullscreen")
                .px_2p5()
                .py_1()
                .rounded_md()
                .bg(theme::bg_elevated())
                .hover(|s| s.bg(theme::bg_hover()))
                .border_1()
                .border_color(theme::border_subtle())
                .text_xs()
                .text_color(theme::text_primary())
                .cursor_pointer()
                .on_click(cx.listener(|this, _, window, cx| {
                    this.focus_root(window);
                    this.toggle_fullscreen(window, cx);
                }))
                .child(fs_label),
        )
        .into_any_element()
}
