//! Media type enumeration.

use serde::{Deserialize, Serialize};

/// Media stream type, equivalent to FFmpeg's AVMediaType.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MediaType {
    Video,
    Audio,
    Subtitle,
    Data,
    Attachment,
}

impl MediaType {
    /// Return the string name of this media type.
    pub fn name(self) -> &'static str {
        match self {
            MediaType::Video => "video",
            MediaType::Audio => "audio",
            MediaType::Subtitle => "subtitle",
            MediaType::Data => "data",
            MediaType::Attachment => "attachment",
        }
    }

    /// Parse a media type from its string name.
    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "video" => Some(MediaType::Video),
            "audio" => Some(MediaType::Audio),
            "subtitle" => Some(MediaType::Subtitle),
            "data" => Some(MediaType::Data),
            "attachment" => Some(MediaType::Attachment),
            _ => None,
        }
    }
}
