//! Media type enumeration.

/// The type of a media stream.
///
/// Analogous to `AVMediaType` in FFmpeg.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaType {
    /// Video stream.
    Video,
    /// Audio stream.
    Audio,
    /// Subtitle stream.
    Subtitle,
    /// Data stream.
    Data,
    /// Attachment stream.
    Attachment,
    /// Unknown stream type.
    Unknown,
}
