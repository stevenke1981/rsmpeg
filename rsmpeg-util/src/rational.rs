//! Rational number type for media timestamps and time bases.

use std::fmt;

/// Rational number (numerator/denominator), equivalent to FFmpeg's AVRational.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rational {
    pub num: i32,
    pub den: i32,
}

impl Rational {
    /// Create a new rational number.
    pub const fn new(num: i32, den: i32) -> Self {
        Rational { num, den }
    }

    /// Create from a floating-point value with continued fraction approximation.
    pub fn from_f64(value: f64) -> Self {
        const MAX_DEN: i32 = 1_000_000;
        if value.is_nan() || value.is_infinite() {
            return Rational { num: 0, den: 1 };
        }
        let sign = if value < 0.0 { -1 } else { 1 };
        let value = value.abs();
        let a0 = value.floor() as i64;
        let mut frac = value - value.floor();
        // p_{-1}=1, p_0=a0; q_{-1}=0, q_0=1  (C0 = a0/1)
        let mut num_prev = 1i64;
        let mut num_curr = a0;
        let mut den_prev = 0i64;
        let mut den_curr = 1i64;

        for _ in 0..20 {
            if frac < 1e-12 {
                break;
            }
            let inv = 1.0 / frac;
            let a = inv.floor() as i64;
            frac = inv - inv.floor();
            let num_next = a * num_curr + num_prev;
            let den_next = a * den_curr + den_prev;
            if den_next > MAX_DEN as i64 {
                break;
            }
            num_prev = num_curr;
            num_curr = num_next;
            den_prev = den_curr;
            den_curr = den_next;
        }

        let num = num_curr * sign as i64;
        Rational {
            num: num as i32,
            den: den_curr as i32,
        }
    }

    /// Return the rational value as a floating-point number.
    pub fn to_f64(self) -> f64 {
        if self.den == 0 {
            0.0
        } else {
            self.num as f64 / self.den as f64
        }
    }

    /// Reduce the rational to its lowest terms.
    pub fn reduced(self) -> Self {
        let g = gcd(self.num.abs(), self.den.abs());
        if g > 1 {
            Rational {
                num: self.num / g,
                den: self.den / g,
            }
        } else {
            self
        }
    }

    /// Multiply by another rational.
    #[allow(clippy::should_implement_trait)]
    pub fn mul(self, other: Rational) -> Self {
        Rational::new(self.num * other.num, self.den * other.den).reduced()
    }

    /// Divide by another rational.
    #[allow(clippy::should_implement_trait)]
    pub fn div(self, other: Rational) -> Self {
        Rational::new(self.num * other.den, self.den * other.num).reduced()
    }

    /// Add another rational.
    #[allow(clippy::should_implement_trait)]
    pub fn add(self, other: Rational) -> Self {
        Rational::new(
            self.num * other.den + other.num * self.den,
            self.den * other.den,
        )
        .reduced()
    }

    /// Check if the rational is zero.
    pub fn is_zero(self) -> bool {
        self.num == 0 || self.den == 0
    }
}

fn gcd(a: i32, b: i32) -> i32 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

impl fmt::Display for Rational {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.num, self.den)
    }
}

impl From<(i32, i32)> for Rational {
    fn from((num, den): (i32, i32)) -> Self {
        Rational::new(num, den)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rational_basic() {
        let r = Rational::new(1, 2);
        assert_eq!(r.to_f64(), 0.5);
    }

    #[test]
    fn test_rational_from_f64() {
        let r = Rational::from_f64(0.33333);
        assert!((r.to_f64() - 1.0 / 3.0).abs() < 0.001);
    }

    #[test]
    fn test_rational_reduce() {
        let r = Rational::new(4, 8).reduced();
        assert_eq!(r.num, 1);
        assert_eq!(r.den, 2);
    }

    #[test]
    fn test_rational_mul() {
        let a = Rational::new(1, 2);
        let b = Rational::new(2, 3);
        assert!((a.mul(b).to_f64() - 1.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn test_rational_add() {
        let a = Rational::new(1, 3);
        let b = Rational::new(1, 6);
        assert!((a.add(b).to_f64() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn test_rational_display() {
        assert_eq!(Rational::new(3, 4).to_string(), "3/4");
    }

    #[test]
    fn test_rational_is_zero() {
        assert!(Rational::new(0, 1).is_zero());
        assert!(Rational::new(5, 0).is_zero());
        assert!(!Rational::new(5, 1).is_zero());
    }

    #[test]
    fn to_f64_handles_zero_denominator() {
        // A zero denominator must not produce NaN/inf or panic.
        assert_eq!(Rational::new(7, 0).to_f64(), 0.0);
        assert_eq!(Rational::new(0, 0).to_f64(), 0.0);
    }

    #[test]
    fn from_f64_handles_non_finite_and_negative() {
        // NaN/inf collapse to 0/1 instead of producing garbage integers.
        assert_eq!(Rational::from_f64(f64::NAN), Rational::new(0, 1));
        assert_eq!(Rational::from_f64(f64::INFINITY), Rational::new(0, 1));
        assert_eq!(Rational::from_f64(f64::NEG_INFINITY), Rational::new(0, 1));

        let neg = Rational::from_f64(-0.5);
        assert_eq!(neg.num, -1);
        assert_eq!(neg.den, 2);
    }

    #[test]
    fn from_f64_exact_ntsc_frame_rate() {
        // 29.97 fps must land on a small-denominator approximation.
        let r = Rational::from_f64(30_000.0 / 1001.0);
        assert!((r.to_f64() - 30_000.0 / 1001.0).abs() < 1e-6);
        assert!(r.den <= 1_000_000, "denominator must stay bounded: {r}");
    }

    #[test]
    fn from_f64_denominator_is_bounded() {
        // A hard-to-approximate value must not blow the denominator past MAX_DEN.
        for v in [1.0 / 3.0, 0.1, std::f64::consts::PI, 1.234_567_891] {
            let r = Rational::from_f64(v);
            assert!(r.den <= 1_000_000, "{v} produced denominator {}", r.den);
            assert!((r.to_f64() - v).abs() < 1e-4, "{v} -> {r} = {}", r.to_f64());
        }
    }

    #[test]
    fn div_by_zero_numerator_is_safe() {
        // Dividing by 0/1 yields an infinite rational represented as den == 0.
        let q = Rational::new(1, 2).div(Rational::new(0, 1));
        assert!(q.is_zero(), "den==0 must be reported as zero: {q}");
    }

    #[test]
    fn reduced_handles_zero_denominator_and_negatives() {
        // gcd(0, 0) must not divide by zero.
        assert_eq!(Rational::new(0, 0).reduced(), Rational::new(0, 0));
        // Negative numerator is reduced by magnitude.
        let r = Rational::new(-6, 8).reduced();
        assert_eq!((r.num, r.den), (-3, 4));
    }

    #[test]
    fn mul_and_add_match_float_reference() {
        let a = Rational::new(2, 3);
        let b = Rational::new(3, 5);
        assert!((a.mul(b).to_f64() - 0.4).abs() < 1e-9);
        assert!((a.add(b).to_f64() - (2.0 / 3.0 + 0.6)).abs() < 1e-9);
    }

    #[test]
    fn from_tuple_and_copy_semantics() {
        let r: Rational = (30000, 1001).into();
        let copied = r; // Copy, so `r` is still usable below.
        assert_eq!(copied, r);
        assert!((copied.to_f64() - 29.970_029_97).abs() < 1e-6);
    }
}
