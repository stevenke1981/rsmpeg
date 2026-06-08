//! Rational number type for media timestamps and time bases.

/// A rational number represented as a numerator/denominator pair.
///
/// Analogous to `AVRational` in FFmpeg.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rational {
    /// Numerator.
    pub num: i32,
    /// Denominator.
    pub den: i32,
}

impl Rational {
    /// Create a new rational number.
    pub const fn new(num: i32, den: i32) -> Self {
        Self { num, den }
    }

    /// Return the rational value as a floating-point number.
    pub fn as_f64(&self) -> f64 {
        if self.den == 0 {
            f64::NAN
        } else {
            self.num as f64 / self.den as f64
        }
    }
}

impl From<(i32, i32)> for Rational {
    fn from((num, den): (i32, i32)) -> Self {
        Self::new(num, den)
    }
}
