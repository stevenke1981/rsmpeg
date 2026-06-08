//! Simple string-keyed dictionary for metadata.

use std::collections::HashMap;

/// A string-keyed dictionary for passing metadata.
///
/// Analogous to `AVDictionary` in FFmpeg.
#[derive(Debug, Clone, Default)]
pub struct Dict {
    entries: HashMap<String, String>,
}

impl Dict {
    /// Create an empty dictionary.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or update a key-value pair.
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.entries.insert(key.into(), value.into());
    }

    /// Get the value for a key, if present.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries.get(key).map(|s| s.as_str())
    }

    /// Remove an entry by key.
    pub fn remove(&mut self, key: &str) -> Option<String> {
        self.entries.remove(key)
    }

    /// Number of entries in this dictionary.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` if the dictionary contains no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over all key-value pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}
