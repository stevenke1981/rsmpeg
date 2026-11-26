//! Pure keyboard shortcut mapping, independent of GPUI event structs.

/// Seek step in seconds for `←` / `→` shortcuts.
pub const SEEK_STEP_SECS: f64 = 5.0;

/// Long seek step in seconds for `Shift+←` / `Shift+→` shortcuts.
pub const SEEK_LONG_STEP_SECS: f64 = 30.0;

/// Fine seek step in seconds for `Ctrl+←` / `Ctrl+→` shortcuts.
pub const SEEK_FINE_STEP_SECS: f64 = 1.0;

/// Volume step (`0.0..=1.0`) for `↑` / `↓` shortcuts.
pub const VOLUME_STEP: f32 = 0.05;

/// Logical action triggered by a keyboard shortcut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortcutAction {
    TogglePlayPause,
    SeekBackward,
    SeekForward,
    SeekBackwardLong,
    SeekForwardLong,
    SeekBackwardFine,
    SeekForwardFine,
    JumpToPercent(u8),
    VolumeUp,
    VolumeDown,
    ToggleFullscreen,
    ExitFullscreen,
    ToggleMute,
    OpenFile,
    Stop,
    NextTrack,
    PrevTrack,
    ToggleInfoPanel,
    TogglePlaylistPanel,
    ToggleVideoTrackMenu,
    ToggleAudioTrackMenu,
    ToggleLocale,
    SpeedDown,
    SpeedUp,
    ResetSpeed,
    CycleRepeatMode,
    RotateVideo,
    ToggleShuffle,
    CycleAbLoop,
    StepFrameForward,
    ToggleSubtitles,
    LoadSubtitle,
    CycleAspectMode,
    ToggleToolsMenu,
    TakeScreenshot,
    ToggleShortcutsHelp,
}

/// Platform-agnostic key press snapshot for shortcut matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyInput<'a> {
    pub key: &'a str,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
}

impl<'a> KeyInput<'a> {
    pub const fn plain(key: &'a str) -> Self {
        Self {
            key,
            ctrl: false,
            alt: false,
            shift: false,
            meta: false,
        }
    }
}

/// Map a normalized keystroke to a [`ShortcutAction`].
pub fn map_key_to_action(input: KeyInput<'_>) -> Option<ShortcutAction> {
    if input.alt || input.meta {
        return None;
    }

    let key_lower = input.key.to_ascii_lowercase();

    // Ctrl shortcuts.
    if input.ctrl {
        if input.shift {
            return None;
        }
        return match key_lower.as_str() {
            "o" => Some(ShortcutAction::OpenFile),
            "left" | "arrowleft" => Some(ShortcutAction::SeekBackwardFine),
            "right" | "arrowright" => Some(ShortcutAction::SeekForwardFine),
            _ => None,
        };
    }

    match key_lower.as_str() {
        "space" | " " => Some(ShortcutAction::TogglePlayPause),
        "left" | "arrowleft" => Some(if input.shift {
            ShortcutAction::SeekBackwardLong
        } else {
            ShortcutAction::SeekBackward
        }),
        "right" | "arrowright" => Some(if input.shift {
            ShortcutAction::SeekForwardLong
        } else {
            ShortcutAction::SeekForward
        }),
        "up" | "arrowup" => Some(ShortcutAction::VolumeUp),
        "down" | "arrowdown" => Some(ShortcutAction::VolumeDown),
        "f" => Some(ShortcutAction::ToggleFullscreen),
        "escape" | "esc" => Some(ShortcutAction::ExitFullscreen),
        "m" => Some(ShortcutAction::ToggleMute),
        "o" => Some(ShortcutAction::OpenFile),
        "s" => Some(if input.shift {
            ShortcutAction::TakeScreenshot
        } else {
            ShortcutAction::Stop
        }),
        "r" => Some(if input.shift {
            ShortcutAction::RotateVideo
        } else {
            ShortcutAction::CycleRepeatMode
        }),
        "c" => Some(if input.shift {
            ShortcutAction::LoadSubtitle
        } else {
            ShortcutAction::ToggleSubtitles
        }),
        "n" | "pagedown" => Some(ShortcutAction::NextTrack),
        "p" | "pageup" => Some(ShortcutAction::PrevTrack),
        "i" => Some(ShortcutAction::ToggleInfoPanel),
        "l" => Some(ShortcutAction::TogglePlaylistPanel),
        "v" => Some(ShortcutAction::ToggleVideoTrackMenu),
        "a" => Some(ShortcutAction::ToggleAudioTrackMenu),
        "g" => Some(ShortcutAction::ToggleLocale),
        "[" => Some(ShortcutAction::SpeedDown),
        "]" => Some(ShortcutAction::SpeedUp),
        "backspace" => Some(ShortcutAction::ResetSpeed),
        "h" => Some(ShortcutAction::ToggleShuffle),
        "b" => Some(ShortcutAction::CycleAbLoop),
        "." => Some(ShortcutAction::StepFrameForward),
        "d" => Some(ShortcutAction::CycleAspectMode),
        "t" => Some(ShortcutAction::ToggleToolsMenu),
        "?" | "f1" => Some(ShortcutAction::ToggleShortcutsHelp),
        "/" if input.shift => Some(ShortcutAction::ToggleShortcutsHelp),
        "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" => Some(
            ShortcutAction::JumpToPercent(key_lower.as_bytes()[0] - b'0'),
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_all_required_playback_shortcuts() {
        assert_eq!(
            map_key_to_action(KeyInput::plain("space")),
            Some(ShortcutAction::TogglePlayPause)
        );
        assert_eq!(
            map_key_to_action(KeyInput::plain("left")),
            Some(ShortcutAction::SeekBackward)
        );
        assert_eq!(
            map_key_to_action(KeyInput::plain("right")),
            Some(ShortcutAction::SeekForward)
        );
        assert_eq!(
            map_key_to_action(KeyInput::plain("up")),
            Some(ShortcutAction::VolumeUp)
        );
        assert_eq!(
            map_key_to_action(KeyInput::plain("down")),
            Some(ShortcutAction::VolumeDown)
        );
        assert_eq!(
            map_key_to_action(KeyInput::plain("f")),
            Some(ShortcutAction::ToggleFullscreen)
        );
        assert_eq!(
            map_key_to_action(KeyInput::plain("F")),
            Some(ShortcutAction::ToggleFullscreen)
        );
        assert_eq!(
            map_key_to_action(KeyInput::plain("m")),
            Some(ShortcutAction::ToggleMute)
        );
        assert_eq!(
            map_key_to_action(KeyInput::plain("o")),
            Some(ShortcutAction::OpenFile)
        );
        assert_eq!(
            map_key_to_action(KeyInput::plain("escape")),
            Some(ShortcutAction::ExitFullscreen)
        );
    }

    #[test]
    fn maps_ctrl_o_and_ignores_unrelated_modifier_chords() {
        assert_eq!(
            map_key_to_action(KeyInput {
                key: "o",
                ctrl: true,
                alt: false,
                shift: false,
                meta: false,
            }),
            Some(ShortcutAction::OpenFile)
        );
        assert_eq!(
            map_key_to_action(KeyInput {
                key: "f",
                ctrl: true,
                alt: false,
                shift: false,
                meta: false,
            }),
            None
        );
        assert_eq!(
            map_key_to_action(KeyInput {
                key: "space",
                ctrl: false,
                alt: true,
                shift: false,
                meta: false,
            }),
            None
        );
    }

    #[test]
    fn maps_playlist_info_and_speed_shortcuts() {
        assert_eq!(
            map_key_to_action(KeyInput::plain("n")),
            Some(ShortcutAction::NextTrack)
        );
        assert_eq!(
            map_key_to_action(KeyInput::plain("p")),
            Some(ShortcutAction::PrevTrack)
        );
        assert_eq!(
            map_key_to_action(KeyInput::plain("i")),
            Some(ShortcutAction::ToggleInfoPanel)
        );
        assert_eq!(
            map_key_to_action(KeyInput::plain("l")),
            Some(ShortcutAction::TogglePlaylistPanel)
        );
        assert_eq!(
            map_key_to_action(KeyInput::plain("v")),
            Some(ShortcutAction::ToggleVideoTrackMenu)
        );
        assert_eq!(
            map_key_to_action(KeyInput::plain("a")),
            Some(ShortcutAction::ToggleAudioTrackMenu)
        );
        assert_eq!(
            map_key_to_action(KeyInput::plain("g")),
            Some(ShortcutAction::ToggleLocale)
        );
        assert_eq!(
            map_key_to_action(KeyInput::plain("[")),
            Some(ShortcutAction::SpeedDown)
        );
        assert_eq!(
            map_key_to_action(KeyInput::plain("]")),
            Some(ShortcutAction::SpeedUp)
        );
        assert_eq!(map_key_to_action(KeyInput::plain("z")), None);
    }

    #[test]
    fn maps_extended_playback_video_subtitle_and_help_shortcuts() {
        let shifted = |key| KeyInput {
            key,
            ctrl: false,
            alt: false,
            shift: true,
            meta: false,
        };
        let ctrled = |key| KeyInput {
            key,
            ctrl: true,
            alt: false,
            shift: false,
            meta: false,
        };

        assert_eq!(
            map_key_to_action(shifted("left")),
            Some(ShortcutAction::SeekBackwardLong)
        );
        assert_eq!(
            map_key_to_action(shifted("right")),
            Some(ShortcutAction::SeekForwardLong)
        );
        assert_eq!(
            map_key_to_action(ctrled("left")),
            Some(ShortcutAction::SeekBackwardFine)
        );
        assert_eq!(
            map_key_to_action(ctrled("right")),
            Some(ShortcutAction::SeekForwardFine)
        );

        for digit in 0..=9u8 {
            let s = digit.to_string();
            assert_eq!(
                map_key_to_action(KeyInput::plain(&s)),
                Some(ShortcutAction::JumpToPercent(digit))
            );
        }

        assert_eq!(
            map_key_to_action(KeyInput::plain("backspace")),
            Some(ShortcutAction::ResetSpeed)
        );
        assert_eq!(
            map_key_to_action(KeyInput::plain("r")),
            Some(ShortcutAction::CycleRepeatMode)
        );
        assert_eq!(
            map_key_to_action(KeyInput::plain("R")),
            Some(ShortcutAction::CycleRepeatMode)
        );
        assert_eq!(
            map_key_to_action(shifted("r")),
            Some(ShortcutAction::RotateVideo)
        );
        assert_eq!(
            map_key_to_action(KeyInput::plain("h")),
            Some(ShortcutAction::ToggleShuffle)
        );
        assert_eq!(
            map_key_to_action(KeyInput::plain("b")),
            Some(ShortcutAction::CycleAbLoop)
        );
        assert_eq!(
            map_key_to_action(KeyInput::plain(".")),
            Some(ShortcutAction::StepFrameForward)
        );
        assert_eq!(
            map_key_to_action(KeyInput::plain("c")),
            Some(ShortcutAction::ToggleSubtitles)
        );
        assert_eq!(
            map_key_to_action(KeyInput::plain("C")),
            Some(ShortcutAction::ToggleSubtitles)
        );
        assert_eq!(
            map_key_to_action(shifted("c")),
            Some(ShortcutAction::LoadSubtitle)
        );
        assert_eq!(
            map_key_to_action(KeyInput::plain("d")),
            Some(ShortcutAction::CycleAspectMode)
        );
        assert_eq!(
            map_key_to_action(KeyInput::plain("t")),
            Some(ShortcutAction::ToggleToolsMenu)
        );
        assert_eq!(
            map_key_to_action(KeyInput::plain("s")),
            Some(ShortcutAction::Stop)
        );
        assert_eq!(
            map_key_to_action(shifted("s")),
            Some(ShortcutAction::TakeScreenshot)
        );
        assert_eq!(
            map_key_to_action(KeyInput::plain("?")),
            Some(ShortcutAction::ToggleShortcutsHelp)
        );
        assert_eq!(
            map_key_to_action(shifted("/")),
            Some(ShortcutAction::ToggleShortcutsHelp)
        );
        assert_eq!(map_key_to_action(KeyInput::plain("/")), None);
        assert_eq!(
            map_key_to_action(KeyInput::plain("f1")),
            Some(ShortcutAction::ToggleShortcutsHelp)
        );
    }
}
