use serde::{Deserialize, Serialize};

bitflags::bitflags! {
    /// Audio channel layout, equivalent to FFmpeg's AVChannelLayout.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub struct ChannelLayout: u64 {
        const FRONT_LEFT      = 1 << 0;
        const FRONT_RIGHT     = 1 << 1;
        const FRONT_CENTER    = 1 << 2;
        const LOW_FREQUENCY   = 1 << 3;
        const BACK_LEFT       = 1 << 4;
        const BACK_RIGHT      = 1 << 5;
        const FRONT_LEFT_OF_CENTER  = 1 << 6;
        const FRONT_RIGHT_OF_CENTER = 1 << 7;
        const BACK_CENTER     = 1 << 8;
        const SIDE_LEFT       = 1 << 9;
        const SIDE_RIGHT      = 1 << 10;

        const MONO            = Self::FRONT_CENTER.bits();
        const STEREO          = Self::FRONT_LEFT.bits() | Self::FRONT_RIGHT.bits();
        const SURROUND        = Self::STEREO.bits() | Self::FRONT_CENTER.bits();
        const _5POINT1        = Self::SURROUND.bits() | Self::BACK_LEFT.bits()
                              | Self::BACK_RIGHT.bits() | Self::LOW_FREQUENCY.bits();
        const _7POINT1        = Self::_5POINT1.bits() | Self::SIDE_LEFT.bits()
                              | Self::SIDE_RIGHT.bits();
    }
}

impl ChannelLayout {
    pub fn name(self) -> &'static str {
        match self {
            x if x == ChannelLayout::MONO => "mono",
            x if x == ChannelLayout::STEREO => "stereo",
            x if x == ChannelLayout::SURROUND => "surround",
            x if x == ChannelLayout::_5POINT1 => "5.1",
            x if x == ChannelLayout::_7POINT1 => "7.1",
            _ => "unknown",
        }
    }

    /// Number of channels.
    pub fn channels(self) -> usize {
        self.bits().count_ones() as usize
    }
}
