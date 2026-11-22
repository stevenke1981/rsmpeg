//! Pure keyboard shortcut mapping, independent of GPUI event structs.

/// Seek step in seconds for `←` / `→` shortcuts.
pub const SEEK_STEP_SECS: f64 = 5.0;

/// Volume step (`0.0..=1.0`) for `↑` / `↓` shortcuts.
pub const VOLUME_STEP: f32 = 0.05;

/// Logical action triggered by a keyboard shortcut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortcutAction {
    TogglePlayPause,
    SeekBackward,
    SeekForward,
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
    SpeedDown,
    SpeedUp,
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

    // Ctrl+O is a standard synonym for opening files.
    if input.ctrl {
        return match key_lower.as_str() {
            "o" => Some(ShortcutAction::OpenFile),
            _ => None,
        };
    }

    match key_lower.as_str() {
        "space" | " " => Some(ShortcutAction::TogglePlayPause),
        "left" | "arrowleft" => Some(ShortcutAction::SeekBackward),
        "right" | "arrowright" => Some(ShortcutAction::SeekForward),
        "up" | "arrowup" => Some(ShortcutAction::VolumeUp),
        "down" | "arrowdown" => Some(ShortcutAction::VolumeDown),
        "f" => Some(ShortcutAction::ToggleFullscreen),
        "escape" | "esc" => Some(ShortcutAction::ExitFullscreen),
        "m" => Some(ShortcutAction::ToggleMute),
        "o" => Some(ShortcutAction::OpenFile),
        "s" => Some(ShortcutAction::Stop),
        "n" | "pagedown" => Some(ShortcutAction::NextTrack),
        "p" | "pageup" => Some(ShortcutAction::PrevTrack),
        "i" => Some(ShortcutAction::ToggleInfoPanel),
        "l" => Some(ShortcutAction::TogglePlaylistPanel),
        "[" => Some(ShortcutAction::SpeedDown),
        "]" => Some(ShortcutAction::SpeedUp),
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
            map_key_to_action(KeyInput::plain("[")),
            Some(ShortcutAction::SpeedDown)
        );
        assert_eq!(
            map_key_to_action(KeyInput::plain("]")),
            Some(ShortcutAction::SpeedUp)
        );
        assert_eq!(map_key_to_action(KeyInput::plain("z")), None);
    }
}
