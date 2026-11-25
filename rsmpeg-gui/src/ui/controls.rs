//! Bottom transport bar: draggable seek bar, playback controls, volume/mute, speed, fullscreen.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    canvas, div, prelude::*, px, relative, rgba, AnyElement, Bounds, Context, InteractiveElement,
    IntoElement, MouseButton, ParentElement, Pixels, Styled, Window,
};

use crate::format_time::{format_duration, format_rate, format_volume_percent};
use crate::i18n::UiStrings;
use crate::playlist::RepeatMode;
use crate::state::{format_track_label_for, PlaybackStatus, PlaybackUiState, TrackMenu};
use crate::ui::{aspect_mode_label, repeat_mode_label, subtitle_size_label, theme, PlayerView};

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
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap_2()
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
    let dur_text = if state.show_remaining_time && !state.duration.is_zero() {
        let rem = state.duration.saturating_sub(state.displayed_position());
        format!("-{}", format_duration(rem))
    } else {
        format_duration(state.duration)
    };
    let frac = state.progress_fraction().clamp(0.0, 1.0) as f32;
    let can_seek = has_media && !state.duration.is_zero();
    let dur_secs = state.duration.as_secs_f64();

    let ab_fracs = if dur_secs > 0.0 {
        let fa = state
            .ab_loop
            .point_a
            .map(|a| (a.as_secs_f64() / dur_secs).clamp(0.0, 1.0) as f32);
        let fb = state
            .ab_loop
            .point_b
            .map(|b| (b.as_secs_f64() / dur_secs).clamp(0.0, 1.0) as f32);
        (fa, fb)
    } else {
        (None, None)
    };

    let hover_tooltip = if can_seek {
        state.seek_hover_fraction.map(|hf| {
            let clamped = hf.clamp(0.0, 1.0);
            let hover_dur = Duration::from_secs_f64(dur_secs * clamped);
            (clamped as f32, format_duration(hover_dur))
        })
    } else {
        None
    };

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
                .on_mouse_move(cx.listener(|this, ev: &gpui::MouseMoveEvent, _window, cx| {
                    this.update_seek_hover(ev.position.x, cx);
                }))
                .on_hover(cx.listener(|this, hovered: &bool, _window, cx| {
                    if !*hovered {
                        this.clear_seek_hover(cx);
                    }
                }))
                .child(
                    // Background rail
                    div()
                        .relative()
                        .w_full()
                        .h(px(6.0))
                        .rounded_full()
                        .bg(theme::track_bg())
                        .overflow_hidden()
                        .when_some(
                            match ab_fracs {
                                (Some(a), Some(b)) if b > a => Some((a, b - a)),
                                _ => None,
                            },
                            |el, (start, span)| {
                                el.child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .bottom_0()
                                        .left(relative(start))
                                        .w(relative(span))
                                        .bg(rgba(0xfbbf2466)),
                                )
                            },
                        )
                        .child(
                            div()
                                .relative()
                                .h_full()
                                .w(relative(frac))
                                .rounded_full()
                                .bg(theme::accent()),
                        ),
                )
                .when_some(ab_fracs.0, |el, fa| {
                    el.child(
                        div()
                            .absolute()
                            .left(relative(fa))
                            .ml(px(-1.0))
                            .w(px(2.0))
                            .h(px(12.0))
                            .rounded_full()
                            .bg(theme::status_paused()),
                    )
                })
                .when_some(ab_fracs.1, |el, fb| {
                    el.child(
                        div()
                            .absolute()
                            .left(relative(fb))
                            .ml(px(-1.0))
                            .w(px(2.0))
                            .h(px(12.0))
                            .rounded_full()
                            .bg(theme::status_paused()),
                    )
                })
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
                .when_some(hover_tooltip, |el, (hf, label)| {
                    el.child(
                        div()
                            .absolute()
                            .bottom(px(24.0))
                            .left(relative(hf))
                            .ml(px(-24.0))
                            .px_1p5()
                            .py_0p5()
                            .rounded_sm()
                            .bg(theme::bg_elevated())
                            .border_1()
                            .border_color(theme::border_strong())
                            .shadow_md()
                            .font_family(theme::MONO_FONT_FAMILY)
                            .text_xs()
                            .text_color(theme::text_primary())
                            .child(label),
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
                .id("btn-duration-mode")
                .flex_none()
                .w(px(68.0))
                .text_right()
                .font_family(theme::MONO_FONT_FAMILY)
                .text_xs()
                .text_color(theme::text_secondary())
                .hover(|s| s.text_color(theme::text_primary()))
                .cursor_pointer()
                .on_click(cx.listener(|this, _, window, cx| {
                    this.focus_root(window);
                    this.toggle_remaining_time(cx);
                }))
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
    let has_prev = state.playlist.has_prev_with_mode(state.repeat_mode);
    let has_next = state
        .playlist
        .has_next_with_mode(state.repeat_mode, state.shuffle);
    let ab_active = state.ab_loop.point_a.is_some();
    let ab_label = if state.ab_loop.point_a.is_some() && state.ab_loop.point_b.is_none() {
        "A·"
    } else {
        strings.ab_loop_btn
    };
    let repeat_active = state.repeat_mode != RepeatMode::Off;
    let repeat_icon = if state.repeat_mode == RepeatMode::One {
        "↻1"
    } else {
        "↻"
    };

    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1p5()
        .child(
            div()
                .id("btn-prev")
                .px_2()
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
                .px_3p5()
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
                .id("btn-step-frame")
                .px_2()
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
                            this.step_frame_forward(cx);
                        }))
                })
                .when(!has_media, |el| el.text_color(theme::text_muted()))
                .child("▸|"),
        )
        .child(
            div()
                .id("btn-stop")
                .px_2()
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
                .px_2()
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
        .child(
            div()
                .id("btn-ab-loop")
                .px_2()
                .py_1p5()
                .rounded_md()
                .bg(if ab_active {
                    theme::accent_soft()
                } else {
                    theme::bg_elevated()
                })
                .border_1()
                .border_color(if ab_active {
                    theme::status_paused()
                } else {
                    theme::border_subtle()
                })
                .font_family(theme::MONO_FONT_FAMILY)
                .text_xs()
                .when(has_media, |el| {
                    el.text_color(if ab_active {
                        theme::status_paused()
                    } else {
                        theme::text_primary()
                    })
                    .hover(|s| s.bg(theme::bg_hover()))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.focus_root(window);
                        this.cycle_ab_loop(cx);
                    }))
                })
                .when(!has_media, |el| el.text_color(theme::text_muted()))
                .child(ab_label),
        )
        .child(
            div()
                .id("btn-repeat")
                .px_2()
                .py_1p5()
                .rounded_md()
                .bg(if repeat_active {
                    theme::accent_soft()
                } else {
                    theme::bg_elevated()
                })
                .hover(|s| s.bg(theme::bg_hover()))
                .border_1()
                .border_color(if repeat_active {
                    theme::accent()
                } else {
                    theme::border_subtle()
                })
                .text_xs()
                .text_color(if repeat_active {
                    theme::accent()
                } else {
                    theme::text_secondary()
                })
                .cursor_pointer()
                .on_click(cx.listener(|this, _, window, cx| {
                    this.focus_root(window);
                    this.cycle_repeat_mode(cx);
                }))
                .child(repeat_icon),
        )
        .child(
            div()
                .id("btn-shuffle")
                .px_2()
                .py_1p5()
                .rounded_md()
                .bg(if state.shuffle {
                    theme::accent_soft()
                } else {
                    theme::bg_elevated()
                })
                .hover(|s| s.bg(theme::bg_hover()))
                .border_1()
                .border_color(if state.shuffle {
                    theme::accent()
                } else {
                    theme::border_subtle()
                })
                .text_xs()
                .text_color(if state.shuffle {
                    theme::accent()
                } else {
                    theme::text_secondary()
                })
                .cursor_pointer()
                .on_click(cx.listener(|this, _, window, cx| {
                    this.focus_root(window);
                    this.toggle_shuffle(cx);
                }))
                .child("⤮"),
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
    let tools_open = state.open_track_menu == TrackMenu::Tools;

    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
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
                .w(px(84.0))
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
                .w(px(34.0))
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
                    .px_2()
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
                    .px_2()
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
                .px_2()
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
            // Tools popover button
            div()
                .id("btn-tools")
                .px_2()
                .py_1()
                .rounded_md()
                .bg(if tools_open {
                    theme::accent_soft()
                } else {
                    theme::bg_elevated()
                })
                .hover(|s| s.bg(theme::bg_hover()))
                .border_1()
                .border_color(if tools_open {
                    theme::accent()
                } else {
                    theme::border_subtle()
                })
                .text_xs()
                .text_color(if tools_open {
                    theme::accent()
                } else {
                    theme::text_primary()
                })
                .cursor_pointer()
                .on_click(cx.listener(|this, _, window, cx| {
                    this.focus_root(window);
                    this.toggle_track_menu(TrackMenu::Tools, cx);
                }))
                .child(format!("⚙ {} ▾", strings.tools_btn)),
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
        TrackMenu::None | TrackMenu::Tools => return None,
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

/// Render the floating playback, video filter, and subtitle tools popover when open.
pub fn render_tools_popover(
    state: &PlaybackUiState,
    strings: &UiStrings,
    cx: &mut Context<PlayerView>,
) -> Option<AnyElement> {
    if state.open_track_menu != TrackMenu::Tools {
        return None;
    }

    let vt = state.video_transform;
    let rotate_label = if vt.rotation_deg == 0 {
        strings.rotate_btn.to_string()
    } else {
        format!("{} ({}°)", strings.rotate_btn, vt.rotation_deg)
    };
    let aspect_label = format!(
        "{}: {}",
        strings.aspect_mode,
        aspect_mode_label(strings, state.aspect_mode)
    );
    let sub_toggle_label = format!(
        "{}: {}",
        strings.subtitle_toggle_btn,
        if state.subtitles_enabled {
            strings.subtitle_on
        } else {
            strings.subtitle_off
        }
    );
    let sub_size_label = format!(
        "{}: {}",
        strings.subtitle_size_label,
        subtitle_size_label(strings, state.subtitle_size)
    );
    let sub_status = state
        .subtitle_name
        .as_deref()
        .unwrap_or(strings.subtitle_none)
        .to_string();
    let ab_active = state.ab_loop.point_a.is_some();
    let repeat_label = repeat_mode_label(strings, state.repeat_mode);
    let shuffle_label = if state.shuffle {
        strings.shuffle_on
    } else {
        strings.shuffle_off
    };

    Some(
        div()
            .id("tools-menu-popover")
            .occlude()
            .absolute()
            .bottom(px(86.0))
            .right(px(16.0))
            .w(px(376.0))
            .flex()
            .flex_col()
            .gap_2p5()
            .p_3()
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
                    .pb_1p5()
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme::accent())
                            .child(strings.tools_menu_title),
                    )
                    .child(
                        div()
                            .id("close-tools-menu")
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
            // Video section
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .p_2()
                    .rounded_md()
                    .bg(theme::bg_surface())
                    .border_1()
                    .border_color(theme::border_subtle())
                    .child(section_heading(strings.video_section))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .gap_1p5()
                            .child(tool_chip_btn(
                                "tool-aspect-btn",
                                aspect_label,
                                state.aspect_mode != crate::state::AspectMode::Contain,
                                true,
                                cx,
                                |this, _, cx| this.cycle_aspect_mode(cx),
                            ))
                            .child(tool_chip_btn(
                                "tool-rotate-btn",
                                rotate_label,
                                vt.rotation_deg != 0,
                                true,
                                cx,
                                |this, window, cx| this.rotate_video(window, cx),
                            ))
                            .child(tool_chip_btn(
                                "tool-mirror-btn",
                                strings.mirror_btn,
                                vt.mirror_horizontal,
                                true,
                                cx,
                                |this, window, cx| this.toggle_mirror(window, cx),
                            ))
                            .child(tool_chip_btn(
                                "tool-grayscale-btn",
                                strings.grayscale_btn,
                                vt.grayscale,
                                true,
                                cx,
                                |this, window, cx| this.toggle_grayscale(window, cx),
                            )),
                    )
                    .child(stepper_row(
                        "tool-bright",
                        strings.brightness_label,
                        format!("{:+}", vt.brightness),
                        cx,
                        |this, window, cx| this.adjust_brightness(-5, window, cx),
                        |this, window, cx| this.adjust_brightness(5, window, cx),
                    ))
                    .child(stepper_row(
                        "tool-contrast",
                        strings.contrast_label,
                        format!("{}%", vt.contrast_pct),
                        cx,
                        |this, window, cx| this.adjust_contrast(-10, window, cx),
                        |this, window, cx| this.adjust_contrast(10, window, cx),
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_1p5()
                            .child(tool_chip_btn(
                                "tool-video-reset-btn",
                                strings.reset_filters_btn,
                                false,
                                !vt.is_identity(),
                                cx,
                                |this, window, cx| this.reset_video_filters(window, cx),
                            ))
                            .child(tool_chip_btn(
                                "tool-screenshot-btn",
                                strings.screenshot_btn,
                                false,
                                state.has_video_frame,
                                cx,
                                |this, window, cx| this.take_screenshot(window, cx),
                            )),
                    ),
            )
            // Subtitles section
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .p_2()
                    .rounded_md()
                    .bg(theme::bg_surface())
                    .border_1()
                    .border_color(theme::border_subtle())
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(section_heading(strings.subtitle_section))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_right()
                                    .text_xs()
                                    .text_color(theme::text_muted())
                                    .truncate()
                                    .child(sub_status),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .gap_1p5()
                            .child(tool_chip_btn(
                                "tool-sub-load-btn",
                                strings.subtitle_load_btn,
                                false,
                                true,
                                cx,
                                |this, window, cx| this.open_subtitle_dialog(window, cx),
                            ))
                            .child(tool_chip_btn(
                                "tool-sub-toggle-btn",
                                sub_toggle_label,
                                state.subtitles_enabled && state.subtitle_track.is_some(),
                                true,
                                cx,
                                |this, _, cx| this.toggle_subtitles(cx),
                            ))
                            .child(tool_chip_btn(
                                "tool-sub-size-btn",
                                sub_size_label,
                                false,
                                true,
                                cx,
                                |this, _, cx| this.cycle_subtitle_size(cx),
                            )),
                    )
                    .child(stepper_row(
                        "tool-sub-offset",
                        strings.subtitle_offset_label,
                        format!("{:+} ms", state.subtitle_offset_ms),
                        cx,
                        |this, _, cx| this.adjust_subtitle_offset(-250, cx),
                        |this, _, cx| this.adjust_subtitle_offset(250, cx),
                    )),
            )
            // Playback section
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .p_2()
                    .rounded_md()
                    .bg(theme::bg_surface())
                    .border_1()
                    .border_color(theme::border_subtle())
                    .child(section_heading(strings.playback_section))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .gap_1p5()
                            .child(tool_chip_btn(
                                "tool-ab-clear-btn",
                                strings.ab_clear_btn,
                                false,
                                ab_active,
                                cx,
                                |this, _, cx| this.clear_ab_loop(cx),
                            ))
                            .child(tool_chip_btn(
                                "tool-repeat-btn",
                                repeat_label,
                                state.repeat_mode != RepeatMode::Off,
                                true,
                                cx,
                                |this, _, cx| this.cycle_repeat_mode(cx),
                            ))
                            .child(tool_chip_btn(
                                "tool-shuffle-btn",
                                shuffle_label,
                                state.shuffle,
                                true,
                                cx,
                                |this, _, cx| this.toggle_shuffle(cx),
                            ))
                            .child(tool_chip_btn(
                                "tool-speed-reset-btn",
                                strings.speed_reset_btn,
                                false,
                                (state.playback_rate - 1.0).abs() > 0.01,
                                cx,
                                |this, _, cx| this.reset_playback_rate(cx),
                            )),
                    ),
            )
            .into_any_element(),
    )
}

fn section_heading(title: &'static str) -> AnyElement {
    div()
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme::text_secondary())
        .child(title)
        .into_any_element()
}

fn tool_chip_btn(
    id: &'static str,
    label: impl Into<String>,
    active: bool,
    enabled: bool,
    cx: &mut Context<PlayerView>,
    on_click: impl Fn(&mut PlayerView, &mut Window, &mut Context<PlayerView>) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .px_2()
        .py_1()
        .rounded_sm()
        .bg(if active {
            theme::accent_soft()
        } else {
            theme::bg_elevated()
        })
        .border_1()
        .border_color(if active {
            theme::accent()
        } else {
            theme::border_subtle()
        })
        .text_xs()
        .when(enabled, |el| {
            el.text_color(if active {
                theme::accent()
            } else {
                theme::text_primary()
            })
            .hover(|s| s.bg(theme::bg_hover()))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, window, cx| {
                this.focus_root(window);
                on_click(this, window, cx);
            }))
        })
        .when(!enabled, |el| el.text_color(theme::text_muted()))
        .child(label.into())
        .into_any_element()
}

fn stepper_row(
    id_prefix: &'static str,
    label: &'static str,
    value: String,
    cx: &mut Context<PlayerView>,
    on_dec: impl Fn(&mut PlayerView, &mut Window, &mut Context<PlayerView>) + 'static,
    on_inc: impl Fn(&mut PlayerView, &mut Window, &mut Context<PlayerView>) + 'static,
) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_2()
        .child(
            div()
                .text_xs()
                .text_color(theme::text_secondary())
                .child(label),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .id((id_prefix, 0usize))
                        .px_2()
                        .py_0p5()
                        .rounded_sm()
                        .bg(theme::bg_elevated())
                        .hover(|s| s.bg(theme::bg_hover()))
                        .border_1()
                        .border_color(theme::border_subtle())
                        .font_family(theme::MONO_FONT_FAMILY)
                        .text_xs()
                        .text_color(theme::text_primary())
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.focus_root(window);
                            on_dec(this, window, cx);
                        }))
                        .child("-"),
                )
                .child(
                    div()
                        .min_w(px(64.0))
                        .text_center()
                        .font_family(theme::MONO_FONT_FAMILY)
                        .text_xs()
                        .text_color(theme::text_primary())
                        .child(value),
                )
                .child(
                    div()
                        .id((id_prefix, 1usize))
                        .px_2()
                        .py_0p5()
                        .rounded_sm()
                        .bg(theme::bg_elevated())
                        .hover(|s| s.bg(theme::bg_hover()))
                        .border_1()
                        .border_color(theme::border_subtle())
                        .font_family(theme::MONO_FONT_FAMILY)
                        .text_xs()
                        .text_color(theme::text_primary())
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.focus_root(window);
                            on_inc(this, window, cx);
                        }))
                        .child("+"),
                ),
        )
        .into_any_element()
}
