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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_get_roundtrip() {
        let mut d = Dict::new();
        d.insert("title", "Example");
        assert_eq!(d.get("title"), Some("Example"));
        assert_eq!(d.get("missing"), None);
    }

    #[test]
    fn insert_overwrites_existing_key() {
        let mut d = Dict::new();
        d.insert("k", "v1");
        d.insert("k", "v2");
        assert_eq!(d.get("k"), Some("v2"));
        assert_eq!(d.len(), 1);
    }

    #[test]
    fn len_and_is_empty_track_entries() {
        let mut d = Dict::new();
        assert!(d.is_empty());
        assert_eq!(d.len(), 0);
        d.insert("a", "1");
        assert!(!d.is_empty());
        assert_eq!(d.len(), 1);
    }

    #[test]
    fn iter_yields_all_pairs_in_any_order() {
        let mut d = Dict::new();
        d.insert("a", "1");
        d.insert("b", "2");
        let mut pairs: Vec<(&str, &str)> = d.iter().collect();
        pairs.sort_unstable();
        assert_eq!(pairs, vec![("a", "1"), ("b", "2")]);
    }

    #[test]
    fn from_iterator_collects_entries() {
        let d: Dict = vec![
            ("lang".to_string(), "zh-TW".to_string()),
            ("encoder".to_string(), "rsmpeg".to_string()),
        ]
        .into_iter()
        .collect();
        assert_eq!(d.len(), 2);
        assert_eq!(d.get("lang"), Some("zh-TW"));
    }

    #[test]
    fn empty_dict_iterates_zero_times() {
        assert_eq!(Dict::new().iter().count(), 0);
        assert_eq!(Dict::default(), Dict::new());
    }
}
