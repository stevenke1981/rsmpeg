//! Audio channel layout.

/// Describes a set of audio channels.
///
/// Analogous to `AVChannelLayout` in FFmpeg.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChannelLayout {
    /// Channel mask bitfield (order is implementation-defined).
    pub mask: u64,
    /// Number of channels.
    pub channels: u32,
}

impl ChannelLayout {
    /// Create a new channel layout with the given mask and channel count.
    pub const fn new(mask: u64, channels: u32) -> Self {
        Self { mask, channels }
    }

    /// Mono layout.
    pub const MONO: Self = Self::new(1, 1);
    /// Stereo (left/right) layout.
    pub const STEREO: Self = Self::new(3, 2);
}
