//! Bottom transport bar: draggable seek bar, playback controls, volume/mute, speed, fullscreen.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    canvas, div, prelude::*, px, relative, AnyElement, Bounds, Context, InteractiveElement,
    IntoElement, MouseButton, ParentElement, Pixels, Styled,
};

use crate::format_time::{format_duration, format_rate, format_volume_percent};
use crate::i18n::UiStrings;
use crate::state::{format_track_label_for, PlaybackStatus, PlaybackUiState, TrackMenu};
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
    let has_video_tracks = !state.video_tracks.is_empty();
    let has_audio_tracks = !state.audio_tracks.is_empty();
    let video_btn_label = if state.video_tracks.len() > 1 {
        format!(
            "{} ({}) ▾",
            strings.track_video_btn,
            state.video_tracks.len()
        )
    } else {
        format!("{} ▾", strings.track_video_btn)
    };
    let audio_btn_label = if state.audio_tracks.len() > 1 {
        format!(
            "{} ({}) ▾",
            strings.track_audio_btn,
            state.audio_tracks.len()
        )
    } else {
        format!("{} ▾", strings.track_audio_btn)
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
        .when(has_video_tracks, |el| {
            let open = state.open_track_menu == TrackMenu::Video;
            el.child(
                div()
                    .id("btn-video-tracks")
                    .px_2p5()
                    .py_1()
                    .rounded_md()
                    .bg(if open {
                        theme::accent_soft()
                    } else {
                        theme::bg_elevated()
                    })
                    .hover(|s| s.bg(theme::bg_hover()))
                    .border_1()
                    .border_color(if open {
                        theme::accent()
                    } else {
                        theme::border_subtle()
                    })
                    .text_xs()
                    .text_color(if open {
                        theme::accent()
                    } else {
                        theme::text_primary()
                    })
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.focus_root(window);
                        this.toggle_track_menu(TrackMenu::Video, cx);
                    }))
                    .child(video_btn_label),
            )
        })
        .when(has_audio_tracks, |el| {
            let open = state.open_track_menu == TrackMenu::Audio;
            el.child(
                div()
                    .id("btn-audio-tracks")
                    .px_2p5()
                    .py_1()
                    .rounded_md()
                    .bg(if open {
                        theme::accent_soft()
                    } else {
                        theme::bg_elevated()
                    })
                    .hover(|s| s.bg(theme::bg_hover()))
                    .border_1()
                    .border_color(if open {
                        theme::accent()
                    } else {
                        theme::border_subtle()
                    })
                    .text_xs()
                    .text_color(if open {
                        theme::accent()
                    } else {
                        theme::text_primary()
                    })
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.focus_root(window);
                        this.toggle_track_menu(TrackMenu::Audio, cx);
                    }))
                    .child(audio_btn_label),
            )
        })
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

/// Render the floating audio/video track switcher dropdown menu when open.
pub fn render_track_menu_popover(
    state: &PlaybackUiState,
    strings: &UiStrings,
    cx: &mut Context<PlayerView>,
) -> Option<AnyElement> {
    let (title, is_video, tracks, active_idx) = match state.open_track_menu {
        TrackMenu::None => return None,
        TrackMenu::Video => (
            strings.track_menu_video_title,
            true,
            &state.video_tracks,
            state.active_video_track,
        ),
        TrackMenu::Audio => (
            strings.track_menu_audio_title,
            false,
            &state.audio_tracks,
            state.active_audio_track,
        ),
    };

    let rows: Vec<AnyElement> = if tracks.is_empty() {
        vec![div()
            .px_3()
            .py_2()
            .text_xs()
            .text_color(theme::text_muted())
            .child(strings.track_none)
            .into_any_element()]
    } else {
        tracks
            .iter()
            .map(|track| {
                let is_active = active_idx == Some(track.index);
                let label = format_track_label_for(state.locale, track, strings.track_unsupported);
                let lang_chip = track.language.as_deref().map(str::to_ascii_uppercase);
                let track_index = track.index;
                let supported = track.supported;

                div()
                    .id((
                        if is_video {
                            "video-track-opt"
                        } else {
                            "audio-track-opt"
                        },
                        track_index,
                    ))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(if is_active {
                        theme::accent_soft()
                    } else {
                        theme::bg_surface()
                    })
                    .hover(|s| s.bg(theme::bg_hover()))
                    .border_1()
                    .border_color(if is_active {
                        theme::accent()
                    } else {
                        theme::border_subtle()
                    })
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.focus_root(window);
                        if is_video {
                            this.select_video_track(track_index, cx);
                        } else {
                            this.select_audio_track(track_index, cx);
                        }
                    }))
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
                                    .flex_none()
                                    .w(px(14.0))
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(if is_active {
                                        theme::accent()
                                    } else {
                                        theme::text_muted()
                                    })
                                    .child(if is_active { "✓" } else { "·" }),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(if supported {
                                        theme::text_primary()
                                    } else {
                                        theme::warn_text()
                                    })
                                    .truncate()
                                    .child(label),
                            ),
                    )
                    .when_some(lang_chip, |el, lang| {
                        el.child(
                            div()
                                .flex_none()
                                .px_1p5()
                                .py_0p5()
                                .rounded_sm()
                                .bg(theme::bg_elevated())
                                .font_family(theme::MONO_FONT_FAMILY)
                                .text_xs()
                                .text_color(theme::text_secondary())
                                .child(lang),
                        )
                    })
                    .into_any_element()
            })
            .collect()
    };

    Some(
        div()
            .id("track-menu-popover")
            .occlude()
            .absolute()
            .bottom(px(86.0))
            .right(px(16.0))
            .w(px(340.0))
            .flex()
            .flex_col()
            .gap_1p5()
            .p_2p5()
            .rounded_lg()
            .bg(theme::bg_elevated())
            .border_1()
            .border_color(theme::border_strong())
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .px_1()
                    .pb_1()
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme::accent())
                            .child(title),
                    )
                    .child(
                        div()
                            .id("close-track-menu")
                            .px_1p5()
                            .py_0p5()
                            .rounded_sm()
                            .hover(|s| s.bg(theme::bg_hover()))
                            .text_xs()
                            .text_color(theme::text_secondary())
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.focus_root(window);
                                this.close_track_menu(cx);
                            }))
                            .child("✕"),
                    ),
            )
            .children(rows)
            .into_any_element(),
    )
}
