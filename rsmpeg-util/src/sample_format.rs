//! Audio sample format definitions.

/// Audio sample format.
///
/// Analogous to `AVSampleFormat` in FFmpeg.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SampleFormat {
    /// None / unknown.
    None,
    /// Unsigned 8-bit.
    U8,
    /// Signed 16-bit planar.
    S16,
    /// Signed 32-bit planar.
    S32,
    /// 32-bit float planar.
    Flt,
    /// 64-bit double planar.
    Dbl,
}

impl SampleFormat {
    /// Return the byte size of a single sample in this format.
    pub fn sample_size(&self) -> usize {
        match self {
            SampleFormat::None => 0,
            SampleFormat::U8 => 1,
            SampleFormat::S16 => 2,
            SampleFormat::S32 => 4,
            SampleFormat::Flt => 4,
            SampleFormat::Dbl => 8,
        }
    }
}
