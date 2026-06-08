use bytes::Bytes;
use rsmpeg_util::Rational;

bitflags::bitflags! {
    /// Packet flags, equivalent to FFmpeg's AV_PKT_FLAG_*.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct PacketFlags: u8 {
        const KEY = 1 << 0;
        const CORRUPT = 1 << 1;
        const DISCARD = 1 << 2;
        const TRUSTED = 1 << 3;
    }
}

/// Compressed media data packet, equivalent to FFmpeg's AVPacket.
#[derive(Debug, Clone)]
pub struct Packet {
    pub data: Bytes,
    pub pts: Option<i64>,
    pub dts: Option<i64>,
    pub duration: i64,
    pub stream_index: usize,
    pub flags: PacketFlags,
    pub pos: i64,
    pub time_base: Rational,
}

impl Packet {
    pub fn new(data: Bytes, stream_index: usize) -> Self {
        Packet {
            data,
            pts: None,
            dts: None,
            duration: 0,
            stream_index,
            flags: PacketFlags::empty(),
            pos: -1,
            time_base: Rational::new(1, 1000),
        }
    }

    pub fn pts_seconds(&self) -> Option<f64> {
        self.pts.map(|pts| pts as f64 * self.time_base.to_f64())
    }

    pub fn duration_seconds(&self) -> f64 {
        self.duration as f64 * self.time_base.to_f64()
    }

    pub fn is_key(&self) -> bool {
        self.flags.contains(PacketFlags::KEY)
    }
}
