use std::collections::HashMap;

/// Metadata dictionary, equivalent to FFmpeg's AVDictionary.
#[derive(Debug, Clone, PartialEq)]
pub struct Dict {
    entries: HashMap<String, String>,
}

impl Dict {
    pub fn new() -> Self {
        Dict {
            entries: HashMap::new(),
        }
    }

    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.entries.insert(key.into(), value.into());
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries.get(key).map(|s| s.as_str())
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for Dict {
    fn default() -> Self {
        Dict::new()
    }
}

impl FromIterator<(String, String)> for Dict {
    fn from_iter<I: IntoIterator<Item = (String, String)>>(iter: I) -> Self {
        Dict {
            entries: iter.into_iter().collect(),
        }
    }
}
