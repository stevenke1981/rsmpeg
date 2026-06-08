/// Standard video color spaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSpace {
    BT601,
    BT709,
    BT2020,
    FCC,
    SMPTE240M,
    RGB,
    YCgCo,
}

impl ColorSpace {
    pub fn name(&self) -> &'static str {
        match self {
            ColorSpace::BT601 => "bt601",
            ColorSpace::BT709 => "bt709",
            ColorSpace::BT2020 => "bt2020",
            ColorSpace::FCC => "fcc",
            ColorSpace::SMPTE240M => "smpte240m",
            ColorSpace::RGB => "rgb",
            ColorSpace::YCgCo => "ycgco",
        }
    }

    /// Compute BT.601/BT.709/BT.2020 luminance coefficients.
    pub fn luminance_coefficients(&self) -> (f32, f32, f32) {
        match self {
            ColorSpace::BT709 | ColorSpace::YCgCo => (0.2126, 0.7152, 0.0722),
            ColorSpace::BT2020 => (0.2627, 0.6780, 0.0593),
            ColorSpace::RGB => (0.3333, 0.3333, 0.3333),
            _ => (0.299, 0.587, 0.114), // BT.601
        }
    }
}

/// YUV color range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorRange {
    Limited, // 16-235 (TV)
    Full,    // 0-255 (PC)
}

impl ColorRange {
    pub fn name(&self) -> &'static str {
        match self {
            ColorRange::Limited => "limited",
            ColorRange::Full => "full",
        }
    }
}

/// Color conversion specification.
#[derive(Debug, Clone)]
pub struct ColorConversion {
    pub src_space: ColorSpace,
    pub dst_space: ColorSpace,
    pub src_range: ColorRange,
    pub dst_range: ColorRange,
}

impl ColorConversion {
    pub fn new(
        src_space: ColorSpace,
        dst_space: ColorSpace,
        src_range: ColorRange,
        dst_range: ColorRange,
    ) -> Self {
        ColorConversion {
            src_space,
            dst_space,
            src_range,
            dst_range,
        }
    }
}
