use serde::{Deserialize, Serialize};

/// Audio sample format, equivalent to FFmpeg's AVSampleFormat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SampleFormat {
    /// Unsigned 8-bit
    U8,
    /// Signed 16-bit
    S16,
    /// Signed 32-bit
    S32,
    /// 32-bit float
    F32,
    /// 64-bit float
    F64,
    /// Signed 16-bit planar
    S16P,
    /// Signed 32-bit planar
    S32P,
    /// 32-bit float planar
    F32P,
    /// 64-bit float planar
    F64P,
    /// None / unknown
    None,
}

impl SampleFormat {
    pub fn name(self) -> &'static str {
        match self {
            SampleFormat::U8 => "u8",
            SampleFormat::S16 => "s16",
            SampleFormat::S32 => "s32",
            SampleFormat::F32 => "f32",
            SampleFormat::F64 => "f64",
            SampleFormat::S16P => "s16p",
            SampleFormat::S32P => "s32p",
            SampleFormat::F32P => "f32p",
            SampleFormat::F64P => "f64p",
            SampleFormat::None => "none",
        }
    }

    /// Bytes per sample.
    pub fn bytes(self) -> usize {
        match self {
            SampleFormat::U8 => 1,
            SampleFormat::S16 | SampleFormat::S16P => 2,
            SampleFormat::S32 | SampleFormat::F32 | SampleFormat::S32P | SampleFormat::F32P => 4,
            SampleFormat::F64 | SampleFormat::F64P => 8,
            SampleFormat::None => 0,
        }
    }

    /// Whether the format is planar.
    pub fn is_planar(self) -> bool {
        matches!(
            self,
            SampleFormat::S16P | SampleFormat::S32P | SampleFormat::F32P | SampleFormat::F64P
        )
    }
}
