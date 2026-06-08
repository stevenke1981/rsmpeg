#![forbid(unsafe_code)]

pub mod scaler;
pub mod colorspace;

pub use scaler::{Scaler, ScalerConfig, ScalerFlags, InterpolationMethod};
pub use colorspace::{ColorSpace, ColorRange, ColorConversion};
