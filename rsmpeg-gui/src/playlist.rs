//! Pure playlist state and media-extension filtering.

use std::path::{Path, PathBuf};

/// Supported file extensions offered in file dialogs and drag-and-drop prioritization.
pub const MEDIA_EXTENSIONS: &[&str] = &[
    "mp4", "m4v", "mov", "mkv", "webm", "avi", "wav", "flac", "mp3", "m4a", "ogg", "opus",
];

/// Return `true` when `path` has a recognized audio/video file extension.
pub fn is_supported_media_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            MEDIA_EXTENSIONS
                .iter()
                .any(|allowed| ext.eq_ignore_ascii_case(allowed))
        })
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

    /// Move to the previous item if one exists.
    pub fn step_prev(&mut self) -> Option<&PlaylistItem> {
        let prev_idx = match self.current {
            Some(i) if i > 0 => i - 1,
            _ => return None,
        };
        self.current = Some(prev_idx);
        self.items.get(prev_idx)
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
}
