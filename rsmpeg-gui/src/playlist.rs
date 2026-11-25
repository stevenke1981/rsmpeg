//! Pure playlist state and media-extension filtering.

use std::path::{Path, PathBuf};

/// Supported file extensions offered in file dialogs and drag-and-drop prioritization.
pub const MEDIA_EXTENSIONS: &[&str] = &[
    "mp4", "m4v", "mov", "mkv", "webm", "avi", "wav", "flac", "mp3", "m4a", "ogg", "opus",
];

/// Supported subtitle extensions for sidecar detection, file dialogs, and drag-and-drop.
pub const SUBTITLE_EXTENSIONS: &[&str] = &["srt", "vtt"];

fn has_extension_in(path: &Path, allowed_exts: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            allowed_exts
                .iter()
                .any(|allowed| ext.eq_ignore_ascii_case(allowed))
        })
}

/// Return `true` when `path` has a recognized audio/video file extension.
pub fn is_supported_media_extension(path: &Path) -> bool {
    has_extension_in(path, MEDIA_EXTENSIONS)
}

/// Return `true` when `path` has a supported subtitle extension (`.srt` / `.vtt`).
pub fn is_subtitle_extension(path: &Path) -> bool {
    has_extension_in(path, SUBTITLE_EXTENSIONS)
}

/// Repeat behavior when advancing through or finishing a playlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RepeatMode {
    #[default]
    Off,
    All,
    One,
}

impl RepeatMode {
    /// Cycle through `Off -> All -> One -> Off`.
    pub const fn cycle(self) -> Self {
        match self {
            Self::Off => Self::All,
            Self::All => Self::One,
            Self::One => Self::Off,
        }
    }
}

/// One entry in the playback queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistItem {
    pub path: PathBuf,
    pub display_name: String,
}

impl PlaylistItem {
    pub fn from_path(path: PathBuf) -> Self {
        let display_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        Self { path, display_name }
    }
}

/// Outcome of removing an item from [`Playlist`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoveOutcome {
    /// Removing the entry did not change the active track.
    ActiveUnchanged,
    /// The active track was removed; contains the replacement path if any remains.
    ActiveReplaced(Option<PathBuf>),
}

/// Ordered playlist with an optional active index.
#[derive(Debug, Clone, Default)]
pub struct Playlist {
    items: Vec<PlaylistItem>,
    current: Option<usize>,
}

impl Playlist {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn items(&self) -> &[PlaylistItem] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn current_index(&self) -> Option<usize> {
        self.current
    }

    pub fn current_item(&self) -> Option<&PlaylistItem> {
        self.current.and_then(|i| self.items.get(i))
    }

    /// Append paths (deduplicating exact path matches) and return the index of
    /// the first path in `paths` so the caller can immediately start playing it.
    pub fn add_paths(&mut self, paths: impl IntoIterator<Item = PathBuf>) -> Option<usize> {
        let mut first_index = None;
        for path in paths {
            let idx = if let Some(existing) = self.items.iter().position(|it| it.path == path) {
                existing
            } else {
                let idx = self.items.len();
                self.items.push(PlaylistItem::from_path(path));
                idx
            };
            if first_index.is_none() {
                first_index = Some(idx);
            }
        }
        first_index
    }

    /// Select an entry by index and return it if in bounds.
    pub fn select(&mut self, index: usize) -> Option<&PlaylistItem> {
        if index < self.items.len() {
            self.current = Some(index);
            self.items.get(index)
        } else {
            None
        }
    }

    pub fn has_next(&self) -> bool {
        match self.current {
            Some(i) => i + 1 < self.items.len(),
            None => !self.items.is_empty(),
        }
    }

    pub fn has_prev(&self) -> bool {
        matches!(self.current, Some(i) if i > 0)
    }

    /// Whether advancing with the given `repeat` and `shuffle` modes yields a track.
    pub fn has_next_with_mode(&self, repeat: RepeatMode, shuffle: bool) -> bool {
        if self.items.is_empty() {
            return false;
        }
        match repeat {
            RepeatMode::All => true,
            RepeatMode::One => self.current.is_some(),
            RepeatMode::Off => {
                if shuffle && self.items.len() > 1 {
                    true
                } else {
                    self.has_next()
                }
            }
        }
    }

    /// Whether stepping backward with the given `repeat` mode yields a track.
    pub fn has_prev_with_mode(&self, repeat: RepeatMode) -> bool {
        if self.items.is_empty() {
            return false;
        }
        match repeat {
            RepeatMode::All => true,
            RepeatMode::Off | RepeatMode::One => self.has_prev(),
        }
    }

    /// Advance to the next item if one exists.
    pub fn advance_next(&mut self) -> Option<&PlaylistItem> {
        let next_idx = match self.current {
            Some(i) if i + 1 < self.items.len() => i + 1,
            None if !self.items.is_empty() => 0,
            _ => return None,
        };
        self.current = Some(next_idx);
        self.items.get(next_idx)
    }

    /// Advance deterministically respecting `repeat` mode, `shuffle`, and `seed`.
    pub fn advance_with_mode(
        &mut self,
        repeat: RepeatMode,
        shuffle: bool,
        seed: u64,
    ) -> Option<&PlaylistItem> {
        let len = self.items.len();
        if len == 0 {
            return None;
        }
        if repeat == RepeatMode::One {
            return self.current.and_then(|cur| self.items.get(cur));
        }
        if shuffle && len > 1 {
            let next_idx = if let Some(cur) = self.current {
                let mixed = seed ^ ((cur as u64 + 1).wrapping_mul(6364136223846793005));
                let offset = 1 + ((mixed as usize) % (len - 1));
                (cur + offset) % len
            } else {
                let mixed = seed ^ 6364136223846793005;
                (mixed as usize) % len
            };
            self.current = Some(next_idx);
            return self.items.get(next_idx);
        }
        let next_idx = match (repeat, self.current) {
            (RepeatMode::All, Some(i)) => (i + 1) % len,
            (RepeatMode::All, None) => 0,
            (RepeatMode::Off, Some(i)) if i + 1 < len => i + 1,
            (RepeatMode::Off, None) => 0,
            _ => return None,
        };
        self.current = Some(next_idx);
        self.items.get(next_idx)
    }

    /// Move to the previous item if one exists.
    pub fn step_prev(&mut self) -> Option<&PlaylistItem> {
        let prev_idx = match self.current {
            Some(i) if i > 0 => i - 1,
            _ => return None,
        };
        self.current = Some(prev_idx);
        self.items.get(prev_idx)
    }

    /// Step backward respecting `repeat` mode (`RepeatMode::All` wraps around).
    pub fn step_prev_with_mode(&mut self, repeat: RepeatMode) -> Option<&PlaylistItem> {
        let len = self.items.len();
        if len == 0 {
            return None;
        }
        let prev_idx = match (repeat, self.current) {
            (RepeatMode::All, Some(i)) => (i + len - 1) % len,
            (RepeatMode::All, None) => len - 1,
            (_, Some(i)) if i > 0 => i - 1,
            _ => return None,
        };
        self.current = Some(prev_idx);
        self.items.get(prev_idx)
    }

    /// Swap item `index` with `index - 1`, preserving `current` tracking.
    pub fn move_up(&mut self, index: usize) -> bool {
        if index == 0 || index >= self.items.len() {
            return false;
        }
        self.items.swap(index - 1, index);
        if self.current == Some(index) {
            self.current = Some(index - 1);
        } else if self.current == Some(index - 1) {
            self.current = Some(index);
        }
        true
    }

    /// Swap item `index` with `index + 1`, preserving `current` tracking.
    pub fn move_down(&mut self, index: usize) -> bool {
        if index >= self.items.len() || index + 1 >= self.items.len() {
            return false;
        }
        self.items.swap(index, index + 1);
        if self.current == Some(index) {
            self.current = Some(index + 1);
        } else if self.current == Some(index + 1) {
            self.current = Some(index);
        }
        true
    }

    /// Remove an entry by index and report whether the active track changed.
    pub fn remove(&mut self, index: usize) -> RemoveOutcome {
        if index >= self.items.len() {
            return RemoveOutcome::ActiveUnchanged;
        }
        self.items.remove(index);
        match self.current {
            Some(cur) if cur == index => {
                if self.items.is_empty() {
                    self.current = None;
                    RemoveOutcome::ActiveReplaced(None)
                } else {
                    let next_cur = index.min(self.items.len() - 1);
                    self.current = Some(next_cur);
                    RemoveOutcome::ActiveReplaced(Some(self.items[next_cur].path.clone()))
                }
            }
            Some(cur) if index < cur => {
                self.current = Some(cur - 1);
                RemoveOutcome::ActiveUnchanged
            }
            _ => RemoveOutcome::ActiveUnchanged,
        }
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.current = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_media_extensions_case_insensitively() {
        assert!(is_supported_media_extension(Path::new("clip.MP4")));
        assert!(is_supported_media_extension(Path::new("song.wav")));
        assert!(is_supported_media_extension(Path::new("movie.mkv")));
        assert!(!is_supported_media_extension(Path::new("readme.txt")));
        assert!(!is_supported_media_extension(Path::new("no_extension")));
    }

    #[test]
    fn add_deduplicates_and_navigates_next_prev() {
        let mut pl = Playlist::new();
        let first = pl.add_paths([PathBuf::from("a.mp4"), PathBuf::from("b.wav")]);
        assert_eq!(first, Some(0));
        assert_eq!(pl.len(), 2);

        // Re-adding an existing item returns its existing index without duplication.
        let dup = pl.add_paths([PathBuf::from("b.wav"), PathBuf::from("c.flac")]);
        assert_eq!(dup, Some(1));
        assert_eq!(pl.len(), 3);

        pl.select(0);
        assert!(!pl.has_prev());
        assert!(pl.has_next());
        assert_eq!(pl.advance_next().unwrap().display_name, "b.wav");
        assert!(pl.has_prev());
        assert!(pl.has_next());
        assert_eq!(pl.advance_next().unwrap().display_name, "c.flac");
        assert!(!pl.has_next());
        assert!(pl.advance_next().is_none());

        assert_eq!(pl.step_prev().unwrap().display_name, "b.wav");
        assert_eq!(pl.step_prev().unwrap().display_name, "a.mp4");
        assert!(pl.step_prev().is_none());
    }

    #[test]
    fn remove_updates_current_index_correctly() {
        let mut pl = Playlist::new();
        pl.add_paths([
            PathBuf::from("0.mp4"),
            PathBuf::from("1.mp4"),
            PathBuf::from("2.mp4"),
        ]);
        pl.select(1);

        // Removing an entry before current shifts current left without changing active file.
        assert_eq!(pl.remove(0), RemoveOutcome::ActiveUnchanged);
        assert_eq!(pl.current_index(), Some(0));
        assert_eq!(pl.current_item().unwrap().display_name, "1.mp4");

        // Removing the active item advances to the next remaining item.
        assert_eq!(
            pl.remove(0),
            RemoveOutcome::ActiveReplaced(Some(PathBuf::from("2.mp4")))
        );
        assert_eq!(pl.current_index(), Some(0));

        // Removing the last remaining item leaves playlist empty.
        assert_eq!(pl.remove(0), RemoveOutcome::ActiveReplaced(None));
        assert!(pl.is_empty());
        assert_eq!(pl.current_index(), None);
    }

    #[test]
    fn repeat_mode_cycles_and_subtitle_extensions_match() {
        assert_eq!(RepeatMode::default(), RepeatMode::Off);
        assert_eq!(RepeatMode::Off.cycle(), RepeatMode::All);
        assert_eq!(RepeatMode::All.cycle(), RepeatMode::One);
        assert_eq!(RepeatMode::One.cycle(), RepeatMode::Off);

        assert!(is_subtitle_extension(Path::new("movie.srt")));
        assert!(is_subtitle_extension(Path::new("movie.VTT")));
        assert!(!is_subtitle_extension(Path::new("movie.mp4")));
        assert!(!is_subtitle_extension(Path::new("no_ext")));
    }

    #[test]
    fn move_up_and_move_down_preserve_current_item_tracking() {
        let mut pl = Playlist::new();
        pl.add_paths([
            PathBuf::from("a.mp4"),
            PathBuf::from("b.mp4"),
            PathBuf::from("c.mp4"),
        ]);
        pl.select(1); // "b.mp4"

        assert!(!pl.move_up(0));
        assert!(!pl.move_down(2));
        assert!(!pl.move_up(99));

        // Moving the current item up updates current_index to follow it.
        assert!(pl.move_up(1));
        assert_eq!(pl.current_index(), Some(0));
        assert_eq!(pl.current_item().unwrap().display_name, "b.mp4");

        // Moving a neighbor across the current item keeps current pointing to "b.mp4".
        assert!(pl.move_up(1)); // "a.mp4" moves from 1 -> 0, "b.mp4" shifts to 1
        assert_eq!(pl.current_index(), Some(1));
        assert_eq!(pl.current_item().unwrap().display_name, "b.mp4");

        // Moving the current item down updates current_index to 2.
        assert!(pl.move_down(1));
        assert_eq!(pl.current_index(), Some(2));
        assert_eq!(pl.current_item().unwrap().display_name, "b.mp4");
        assert_eq!(pl.items()[0].display_name, "a.mp4");
        assert_eq!(pl.items()[1].display_name, "c.mp4");
    }

    #[test]
    fn advance_and_step_prev_with_mode_cover_off_all_one_and_shuffle() {
        let mut pl = Playlist::new();
        assert!(!pl.has_next_with_mode(RepeatMode::All, false));
        assert!(!pl.has_prev_with_mode(RepeatMode::All));
        assert!(pl.advance_with_mode(RepeatMode::All, false, 1).is_none());
        assert!(pl.step_prev_with_mode(RepeatMode::All).is_none());

        pl.add_paths([
            PathBuf::from("0.mp4"),
            PathBuf::from("1.mp4"),
            PathBuf::from("2.mp4"),
        ]);
        pl.select(2);

        // Repeat::Off stops at end when not shuffling.
        assert!(!pl.has_next_with_mode(RepeatMode::Off, false));
        assert!(pl.advance_with_mode(RepeatMode::Off, false, 7).is_none());

        // Repeat::All wraps forward and backward.
        assert!(pl.has_next_with_mode(RepeatMode::All, false));
        assert_eq!(
            pl.advance_with_mode(RepeatMode::All, false, 7)
                .unwrap()
                .display_name,
            "0.mp4"
        );
        assert!(pl.has_prev_with_mode(RepeatMode::All));
        assert_eq!(
            pl.step_prev_with_mode(RepeatMode::All)
                .unwrap()
                .display_name,
            "2.mp4"
        );

        // Repeat::One stays on current.
        assert!(pl.has_next_with_mode(RepeatMode::One, true));
        assert_eq!(
            pl.advance_with_mode(RepeatMode::One, true, 7)
                .unwrap()
                .display_name,
            "2.mp4"
        );
        assert_eq!(pl.current_index(), Some(2));

        // Shuffle picks a deterministic index != current.
        assert!(pl.has_next_with_mode(RepeatMode::Off, true));
        let picked = pl
            .advance_with_mode(RepeatMode::Off, true, 42)
            .unwrap()
            .display_name
            .clone();
        assert_ne!(picked, "2.mp4");
        let idx1 = pl.current_index().unwrap();
        // Reset back to 2 with same seed -> identical deterministic choice.
        pl.select(2);
        pl.advance_with_mode(RepeatMode::Off, true, 42);
        assert_eq!(pl.current_index(), Some(idx1));
    }
}
