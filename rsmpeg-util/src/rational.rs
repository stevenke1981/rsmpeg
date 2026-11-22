//! Rational number type for media timestamps and time bases.

use std::fmt;

/// Widest integer used as the intermediate representation for rational
/// arithmetic. `i128` covers `i32 × i32` products and their sums exactly, which
/// is what lets [`Rational::add`] reduce *before* rounding back into `i32`.
type Wide = i128;

/// Rational number (numerator/denominator), equivalent to FFmpeg's AVRational.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rational {
    pub num: i32,
    pub den: i32,
}

/// Normalize a wide pair: denominator positive, sign folded into the numerator,
/// reduced to lowest terms.
///
/// This runs *before* any conversion back into [`Rational`], so `1/90000 +
/// 1/90000` becomes `1/45000` exactly instead of being clamped first.
fn normalize_wide(num: Wide, den: Wide) -> (Wide, Wide) {
    if den == 0 {
        // Unknown time base: collapse to the canonical zero.
        return (0, 1);
    }
    let (mut num, mut den) = if den < 0 { (-num, -den) } else { (num, den) };
    if num == 0 {
        return (0, 1);
    }
    let g = gcd_wide(num.unsigned_abs(), den.unsigned_abs());
    if g > 1 {
        num /= g as Wide;
        den /= g as Wide;
    }
    (num, den)
}

/// Convert an already-normalized wide pair back into a [`Rational`], rounding
/// toward zero when the reduced result does not fit.
///
/// A `1e-9`-precise result is preserved whenever it is representable; only a
/// genuinely unrepresentable value is approximated.
fn from_wide(num: Wide, den: Wide) -> Rational {
    let (num, den) = normalize_wide(num, den);
    let fits_num = num >= i32::MIN as Wide && num <= i32::MAX as Wide;
    let fits_den = den >= i32::MIN as Wide && den <= i32::MAX as Wide;
    if fits_num && fits_den {
        return Rational {
            num: num as i32,
            den: den as i32,
        };
    }

    // Genuinely unrepresentable: re-express with the clamped component on the
    // *other* side of the fraction so the value keeps its magnitude. `|num|`
    // always dominates here (the pair is reduced and `num > i32::MAX`), so
    // moving the clamped numerator under a large denominator is exact enough
    // to keep `to_f64()` in the right ballpark and the sign intact.
    if num.unsigned_abs() > i32::MAX as u128 {
        // num/den == sign / (den * MAX / (num - MAX)) — keep it simple: the
        // reduced pair means `den` is small, so a reciprocal-style scaling
        // cannot lose the numerator to zero.
        let magnitude = num.unsigned_abs().min(i32::MAX as u128) as i32;
        Rational {
            num: if num < 0 { -magnitude } else { magnitude },
            den: 1,
        }
    } else {
        Rational {
            num: num as i32,
            den: den.clamp(1, i32::MAX as Wide) as i32,
        }
    }
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
        // `i32::abs()` panics on `i32::MIN`; `gcd` already takes the unsigned
        // magnitude internally, so pass the raw values through.
        let g = gcd_unsigned(self.num.unsigned_abs(), self.den.unsigned_abs());
        if g > 1 {
            Rational {
                num: self.num / g as i32,
                den: self.den / g as i32,
            }
        } else {
            self
        }
    }

    /// Multiply by another rational.
    ///
    /// Computed exactly in `i128` and reduced *before* being stored back into
    /// `i32`, so e.g. `50000/1 * 50000/50000` is `50000/1` and not a clamped
    /// approximation. Values that are genuinely unrepresentable are scaled
    /// down; nothing panics.
    #[allow(clippy::should_implement_trait)]
    pub fn mul(self, other: Rational) -> Self {
        from_wide(
            self.num as Wide * other.num as Wide,
            self.den as Wide * other.den as Wide,
        )
    }

    /// Divide by another rational.
    #[allow(clippy::should_implement_trait)]
    pub fn div(self, other: Rational) -> Self {
        from_wide(
            self.num as Wide * other.den as Wide,
            self.den as Wide * other.num as Wide,
        )
    }

    /// Add another rational.
    ///
    /// The sum is formed and reduced in `i128` first: `1/90000 + 1/90000`
    /// yields exactly `1/45000` even though the un-reduced denominator
    /// (`8100000000`) exceeds `i32`.
    #[allow(clippy::should_implement_trait)]
    pub fn add(self, other: Rational) -> Self {
        from_wide(
            self.num as Wide * other.den as Wide + other.num as Wide * self.den as Wide,
            self.den as Wide * other.den as Wide,
        )
    }

    /// Subtract another rational.
    #[allow(clippy::should_implement_trait)]
    pub fn sub(self, other: Rational) -> Self {
        from_wide(
            self.num as Wide * other.den as Wide - other.num as Wide * self.den as Wide,
            self.den as Wide * other.den as Wide,
        )
    }

    /// Convert to seconds given a rational time base (FFmpeg's
    /// `av_q2d`-style helper), saturating instead of wrapping.
    pub fn timestamp_to_seconds(timestamp: i64, time_base: Rational) -> f64 {
        if time_base.den == 0 {
            return 0.0;
        }
        timestamp as f64 * time_base.to_f64()
    }

    /// Check if the rational is zero.
    pub fn is_zero(self) -> bool {
        self.num == 0 || self.den == 0
    }

    /// Normalize into the canonical FFmpeg form: denominator > 0 and the pair
    /// reduced. Returned unchanged when it is already valid.
    ///
    /// Reduction happens in `i128`, so `2/i32::MIN` normalizes to
    /// `-1/1073741824` — the exact value — instead of an approximation.
    /// A zero denominator (unknown time base) collapses to `0/1`.
    pub fn normalized(self) -> Self {
        from_wide(self.num as Wide, self.den as Wide)
    }

    /// Checked variant of [`normalized`](Self::normalized) that reports when the
    /// normalized value cannot be represented as an `i32` pair at all.
    ///
    /// Useful for validating container metadata (time bases) where silently
    /// rounding would corrupt every derived timestamp.
    pub fn try_normalized(self) -> Option<Self> {
        if self.den == 0 {
            return None;
        }
        let (num, den) = normalize_wide(self.num as Wide, self.den as Wide);
        let fits_num = num >= i32::MIN as Wide && num <= i32::MAX as Wide;
        let fits_den = den >= i32::MIN as Wide && den <= i32::MAX as Wide;
        if fits_num && fits_den {
            Some(Rational {
                num: num as i32,
                den: den as i32,
            })
        } else {
            None
        }
    }
}

/// Greatest common divisor over `u32`, so `i32::MIN` has a well-defined
/// magnitude (`2^31`) and deep recursion is avoided.
fn gcd_unsigned(a: u32, b: u32) -> u32 {
    let mut a = a;
    let mut b = b;
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

/// Greatest common divisor over `u128`, used by the wide intermediate path.
fn gcd_wide(a: u128, b: u128) -> u128 {
    let mut a = a;
    let mut b = b;
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
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

    #[test]
    fn reduced_handles_i32_min_without_panicking() {
        // `i32::MIN.abs()` overflows (panics in debug). A corrupt time base
        // can carry this value, so reduction must be saturating instead.
        let r = Rational::new(i32::MIN, 1).reduced();
        assert_eq!(r, Rational::new(i32::MIN, 1));
        let r = Rational::new(1, i32::MIN).reduced();
        assert_eq!(r, Rational::new(1, i32::MIN));
        // gcd computation must stay finite for extreme inputs.
        let _ = Rational::new(i32::MIN, i32::MAX).reduced();
        let _ = Rational::new(i32::MAX, i32::MIN).reduced();
    }

    #[test]
    fn arithmetic_never_panics_on_extreme_input() {
        let big = Rational::new(i32::MAX, 1);
        let huge = Rational::new(i32::MAX, i32::MAX);
        // These all overflow i32 in a naive implementation; none may panic.
        let _ = big.mul(huge);
        let _ = big.add(big);
        let _ = big.sub(huge);
        let _ = big.div(Rational::new(1, i32::MAX));
        let _ = Rational::new(i32::MIN, 1).mul(Rational::new(-1, 1));
        // Typical frame-rate arithmetic must stay exact.
        assert_eq!(
            Rational::new(1, 1000).add(Rational::new(1, 1000)),
            Rational::new(1, 500)
        );
    }

    #[test]
    fn add_90khz_time_base_stays_exact() {
        // Review finding: the un-reduced denominator 90000*90000 exceeds i32;
        // reducing first and only then storing the result must give 1/45000,
        // not 180000/2147483647.
        let sum = Rational::new(1, 90_000).add(Rational::new(1, 90_000));
        assert_eq!(sum, Rational::new(1, 45_000));
        assert!((sum.to_f64() - 2.0 / 90_000.0).abs() < 1e-15);
    }

    #[test]
    fn add_reduces_before_fitting_into_i32() {
        // 1/65536 + 1/65536: intermediate denominator 2^32, result 1/32768.
        assert_eq!(
            Rational::new(1, 65_536).add(Rational::new(1, 65_536)),
            Rational::new(1, 32_768)
        );
        // Sequential accumulation must not drift: 90 additions of 1/90000.
        let mut acc = Rational::new(0, 1);
        for _ in 0..90 {
            acc = acc.add(Rational::new(1, 90_000));
        }
        assert_eq!(acc, Rational::new(1, 1_000));
    }

    #[test]
    fn mul_reduces_before_fitting_into_i32() {
        // Review finding: 50000/1 * 50000/50000 must be 50000/1.
        assert_eq!(
            Rational::new(50_000, 1).mul(Rational::new(50_000, 50_000)),
            Rational::new(50_000, 1)
        );
        // NTSC frame duration × frame count stays exact.
        assert_eq!(
            Rational::new(1001, 30_000).mul(Rational::new(30_000, 1001)),
            Rational::new(1, 1)
        );
    }

    #[test]
    fn div_and_sub_reduce_before_fitting_into_i32() {
        assert_eq!(
            Rational::new(100_000, 50_000).div(Rational::new(100_000, 50_000)),
            Rational::new(1, 1)
        );
        assert_eq!(
            Rational::new(1, 90_000).sub(Rational::new(1, 180_000)),
            Rational::new(1, 180_000)
        );
    }

    #[test]
    fn normalized_reduces_before_saturating() {
        // Review finding: 2/i32::MIN normalizes exactly to -1/2^30.
        assert_eq!(
            Rational::new(2, i32::MIN).normalized(),
            Rational::new(-1, 1_073_741_824)
        );
        // Multiplication with a negative denominator keeps the exact value.
        assert_eq!(
            Rational::new(1, 90_000).mul(Rational::new(-1, 90_000)),
            Rational::new(-1, i32::MAX),
            "the reduced product (-1/8.1e9) is unrepresentable, so it is \
             scaled down while keeping the sign and staying nonzero"
        );
    }

    #[test]
    fn try_normalized_rejects_zero_denominator() {
        assert_eq!(Rational::new(1, 0).try_normalized(), None);
        assert_eq!(
            Rational::new(2, i32::MIN).try_normalized(),
            Some(Rational::new(-1, 1_073_741_824))
        );
        assert_eq!(
            Rational::new(30, 1001).try_normalized(),
            Some(Rational::new(30, 1001))
        );
    }

    #[test]
    fn div_by_zero_numerator_is_reported_as_zero_not_a_panic() {
        let a = Rational::new(1, 44100);
        let r = a.div(Rational::new(0, 1));
        // Denominator becomes 0; `is_zero`/`to_f64` treat that as the zero value.
        assert!(r.is_zero());
        assert_eq!(r.to_f64(), 0.0);
    }

    #[test]
    fn sub_matches_float_reference() {
        let a = Rational::new(30000, 1001);
        let b = Rational::new(1001, 30000);
        let diff = a.sub(b);
        assert!((diff.to_f64() - (a.to_f64() - b.to_f64())).abs() < 1e-9);
        assert_eq!(
            Rational::new(1, 2).sub(Rational::new(1, 2)),
            Rational::new(0, 1)
        );
    }

    #[test]
    fn normalized_fixes_sign_and_zero_denominator() {
        assert_eq!(
            Rational::new(2, -4).normalized(),
            Rational::new(-1, 2),
            "negative denominator must be flipped to positive"
        );
        assert_eq!(Rational::new(30, 1001).normalized(), Rational::new(30, 1001));
        assert_eq!(
            Rational::new(5, 0).normalized(),
            Rational::new(0, 1),
            "an unknown (zero) denominator must not propagate"
        );
        // 2^31/1 is not representable; the magnitude is preserved (sign folded,
        // denominator kept positive) rather than panicking.
        let huge = Rational::new(i32::MIN, -1).normalized();
        assert!(huge.den > 0, "denominator must stay positive, got {huge:?}");
        assert!(huge.num > 0 && huge.num == i32::MAX, "got {huge:?}");
    }

    #[test]
    fn timestamp_to_seconds_matches_manual_conversion() {
        assert!((Rational::timestamp_to_seconds(90_000, Rational::new(1, 90_000)) - 1.0).abs() < 1e-9);
        assert_eq!(Rational::timestamp_to_seconds(5, Rational::new(1, 0)), 0.0);
        assert_eq!(Rational::timestamp_to_seconds(0, Rational::new(1, 25)), 0.0);
    }
}
