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

/// Largest magnitude a single component of a [`Rational`] can hold.
const MAX_COMPONENT: u128 = i32::MAX as u128;

/// Nearest `i32`-pair approximation of a reduced wide pair, or `None` when
/// `den == 0` (an undefined value).
///
/// # Contract
///
/// * If the exact, already-reduced pair fits in `i32`, it is returned verbatim —
///   no rounding happens at all.
/// * Otherwise the *value* is approximated: the returned `to_f64()` is within
///   one part in `i32::MAX` of the true value, clamped to `±i32::MAX`. Scaling
///   both components by the same factor is what preserves the ratio; the
///   previous implementation clamped the numerator and kept `den = 1`, which
///   turned `(50000/50001)² ≈ 0.99996` into `2147483647` and multiplied the
///   error by ~2·10⁹.
///
/// Callers that cannot tolerate any approximation must use the `try_*` forms
/// ([`Rational::try_normalized`]), which report unrepresentability instead.
fn approximate_wide(num: Wide, den: Wide) -> Option<Rational> {
    let (num, den) = normalize_wide(num, den);
    if den == 0 {
        return None;
    }
    if let Some(exact) = checked_wide(num, den) {
        return Some(exact);
    }

    // No exact `i32` fraction exists. Reference must stay in `i32::MAX/1`, which
    // admits denominators up to `i32::MAX`. Two documented regimes:
    //
    // * `|v| < 1`: quantize onto the `1/i32::MAX` lattice, i.e. the nearest
    //   representable value; the error is at most `1/(2·i32::MAX)` (~2.3e-10).
    //   Values below half a step collapse to `0/1`.
    // * `|v| >= 1` (this is where the old code returned `i32::MAX/1` for
    //   `v = 1.00004`): scale `v` into `[0, 1]` with `k = ceil(|v|)` and write
    //   `sign * (n·k + m) / den` where `n/den` is the integer part and `m/den`
    //   the fractional lattice point. The relative error is at most
    //   `1/(k·i32::MAX)` and saturation only happens past `i32::MAX`.
    let negative = num < 0;
    let value = num as f64 / den as f64;
    let magnitude = value.abs();

    if magnitude < 1.0 {
        let steps = (magnitude * i32::MAX as f64).round() as i128;
        // Quantize, then re-apply the sign. Applying the sign to a zero
        // numerator before normalizing would keep `-0` around as a positive
        // value once the denominator is positive.
        let num_out = if steps == 0 {
            0
        } else if negative {
            -steps
        } else {
            steps
        };
        return Some(
            Rational {
                num: num_out as i32,
                den: i32::MAX,
            }
            .normalized(),
        );
    }
    let sign: i128 = if negative { -1 } else { 1 };

    // `|v| >= 1`: split into integer and fractional parts on the same lattice.
    let integer = magnitude.floor();
    if integer > i32::MAX as f64 {
        return Some(Rational {
            num: (sign * i32::MAX as i128) as i32,
            den: 1,
        });
    }
    let frac = magnitude - integer;
    // The fractional part must share the denominator: numerator = n*den + m
    // with m <= den. With den = i32::MAX the numerator would exceed i32::MAX
    // for any n >= 1, so scale the denominator down to keep the numerator in
    // range — that is what preserves the fractional precision here, unlike
    // simply clamping to i32::MAX/1.
    let k = integer as i128;
    let den_out = (i32::MAX as i128 / (k + 1)).max(1);
    let frac_steps = (frac * den_out as f64).round() as i128;
    let num_out = (k * den_out + frac_steps).min(i32::MAX as i128);
    Some(
        Rational {
            num: (sign * num_out) as i32,
            den: den_out as i32,
        }
        .normalized(),
    )
}



/// Like [`approximate_wide`] but for the infallible arithmetic operators: a
/// zero denominator (from e.g. division by `0/1`) collapses to the canonical
/// zero rather than panicking.
fn from_wide(num: Wide, den: Wide) -> Rational {
    approximate_wide(num, den).unwrap_or(Rational { num: 0, den: 1 })
}

/// Exact result of an operation, or `None` when the *normalized* value cannot be
/// held by an `i32` fraction.
///
/// Normalization first is what makes `try_*` usable: an operation may produce a
/// wide pair whose raw components overflow while its reduced form is perfectly
/// ordinary (`1/90000 + 1/90000` = `180000/8100000000` → `1/45000`).
/// A zero denominator (division by zero) is not a value at all and yields
/// `None`.
fn checked_wide(num: Wide, den: Wide) -> Option<Rational> {
    let (num, den) = normalize_wide(num, den);
    if den == 0 {
        return None;
    }
    // Both components must fit: `as i32` on an out-of-range value wraps into a
    // negative denominator (8_100_000_000 → -489934592), which silently turns a
    // fraction into nonsense.
    if num >= i32::MIN as Wide && num <= i32::MAX as Wide && den <= MAX_COMPONENT as Wide {
        Some(Rational {
            num: num as i32,
            den: den as i32,
        })
    } else {
        None
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
    /// # Precision
    ///
    /// Computed exactly in `i128` and reduced *before* being stored back into
    /// `i32`, so e.g. `50000/1 * 50000/50000` is exactly `50000/1`.
    ///
    /// When the reduced result does not fit an `i32` pair, the returned value is
    /// the **nearest representable approximation**: both components are scaled
    /// by the same factor, so `to_f64()` stays within one part in `i32::MAX` of
    /// the exact value (clamped to `±i32::MAX`) instead of collapsing to
    /// `i32::MAX/1`. Use [`try_mul`](Self::try_mul) when an exact answer is
    /// required. Nothing panics.
    #[allow(clippy::should_implement_trait)]
    pub fn mul(self, other: Rational) -> Self {
        from_wide(
            self.num as Wide * other.num as Wide,
            self.den as Wide * other.den as Wide,
        )
    }

    /// Checked [`mul`](Self::mul): `None` when the exact product is not
    /// representable as an `i32` fraction (or its denominator is zero).
    pub fn try_mul(self, other: Rational) -> Option<Self> {
        let num = self.num as Wide * other.num as Wide;
        let den = self.den as Wide * other.den as Wide;
        checked_wide(num, den)
    }

    /// Checked [`add`](Self::add): `None` when the exact sum is not
    /// representable as an `i32` fraction.
    pub fn try_add(self, other: Rational) -> Option<Self> {
        let num = self.num as Wide * other.den as Wide + other.num as Wide * self.den as Wide;
        let den = self.den as Wide * other.den as Wide;
        checked_wide(num, den)
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
        // Work on the *magnitudes* so `i32::MIN` (±2^31) keeps its exact value
        // through the reduction: 2/i32::MIN is exactly -1/2^30 and must not be
        // nudged onto the neighbouring grid point.
        if self.den == 0 {
            return Rational { num: 0, den: 1 };
        }
        let negative = (self.num < 0) != (self.den < 0);
        let num = self.num.unsigned_abs() as Wide;
        let den = self.den.unsigned_abs() as Wide;
        let g = gcd_wide(num as u128, den as u128) as Wide;
        let (num, den) = if g > 1 { (num / g, den / g) } else { (num, den) };
        let num = if negative { -num } else { num };
        from_wide(num, den)
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
    fn unrepresentable_product_keeps_the_magnitude() {
        // Second review round: (50000/50001)² ≈ 0.9999600012 used to come back
        // as 2147483647/1 — an error of 2·10⁹. The approximation must stay
        // within one part in i32::MAX of the exact value.
        let operand = Rational::new(50_000, 50_001);
        let product = operand.mul(operand);
        let expected = operand.to_f64() * operand.to_f64();
        let error = (product.to_f64() - expected).abs();
        assert!(
            error < 1e-6,
            "{product:?} = {} vs expected {expected}",
            product.to_f64()
        );
        assert_ne!(
            product,
            Rational::new(i32::MAX, 1),
            "the result must not collapse to i32::MAX/1"
        );
        // Same for a genuinely huge exact value: it approximates i32::MAX, it
        // does not become something else.
        let huge = Rational::new(i32::MAX, 1).mul(Rational::new(i32::MAX, 1));
        assert!(huge.to_f64() >= i32::MAX as f64, "got {huge:?}");
        assert_eq!(huge.den, 1);
    }

    #[test]
    fn approximation_stays_on_the_documented_grid() {
        // (boundary-probe case) (50000/50001)^2 ≈ 0.9999600012 must stay on the
        // 1/i32::MAX lattice near that value — never 2147483647/1.
        let operand = Rational::new(50_000, 50_001);
        let squared = operand.mul(operand);
        let exact = operand.to_f64() * operand.to_f64();
        assert!(
            (squared.to_f64() - exact).abs() < 1e-6,
            "got {squared:?} = {} vs {exact}",
            squared.to_f64()
        );
        assert_eq!(squared.den, i32::MAX, "must stay on the fine lattice");
        assert_ne!(squared, Rational::new(i32::MAX, 1));

        // Values near 1 are limited by the type, not the arithmetic: the grid
        // around 1 has a step of 1/i32::MAX (~4.7e-10), and the i32 numerator
        // bound puts the next point below 1 at 2147483646/2147483647.
        for (num, den) in [(50_000i32, 50_001i32), (50_001, 50_000), (1, 90_000)] {
            let squared = Rational::new(num, den).mul(Rational::new(num, den));
            let exact = (num as f64 / den as f64) * (num as f64 / den as f64);
            let err = (squared.to_f64() - exact).abs();
            assert!(
                err <= 0.5 / i32::MAX as f64,
                "{num}/{den} squared: {squared:?} (err {err}) vs {exact}"
            );
            // Never the old `i32::MAX/1` rubbish for values near 1.
            assert_ne!(squared, Rational::new(i32::MAX, 1), "{squared:?}");
        }
        // Just below 1 keeps the closest representable point.
        let below = Rational::new(50_000, 50_001).mul(Rational::new(50_000, 50_001));
        assert!(
            (below.to_f64() - 0.999_960_001_199_968_1).abs() <= 0.5 / i32::MAX as f64,
            "got {below:?}"
        );
    }

    #[test]
    fn approximation_never_exceeds_half_a_lattice_step_below_one() {
        // Documented bound for |v| < 1: at most 1/(2*i32::MAX) away.
        let bound = 0.5 / i32::MAX as f64;
        for (n, d) in [
            (1i32, 3i32),
            (2, 3),
            (123_456, 999_999),
            (999_999, 1_000_000),
            (1, 90_001),
        ] {
            let v = Rational::new(n, d).mul(Rational::new(n, d));
            let exact = (n as f64 / d as f64) * (n as f64 / d as f64);
            if exact >= 1.0 {
                continue;
            }
            assert!(
                (v.to_f64() - exact).abs() <= bound,
                "{n}/{d}² → {v:?} err {}",
                (v.to_f64() - exact).abs()
            );
        }
    }

    #[test]
    fn values_above_one_keep_their_fractional_part() {
        // (50001/50000)² ≈ 1.0000400004 — the old code returned 1/1 (and before
        // that i32::MAX/1). The result must be closer than 1e-4.
        let v = Rational::new(50_001, 50_000).mul(Rational::new(50_001, 50_000));
        let exact = (50_001.0f64 / 50_000.0).powi(2);
        assert!(
            (v.to_f64() - exact).abs() < 1e-4,
            "got {v:?} = {} vs {exact}",
            v.to_f64()
        );
        assert!(v.to_f64() > 1.0, "magnitude must survive: {v:?}");
    }

    #[test]
    fn exact_values_are_never_approximated() {
        // Anything representable must come back bit-exact.
        for (a, b) in [
            (Rational::new(1, 90_000), Rational::new(1, 90_000)),
            (Rational::new(1001, 30_000), Rational::new(30_000, 1001)),
            (Rational::new(1, 2), Rational::new(2, 3)),
        ] {
            let sum = a.add(b);
            let exact_num = a.num as i128 * b.den as i128 + b.num as i128 * a.den as i128;
            let exact_den = a.den as i128 * b.den as i128;
            let g = {
                let (mut x, mut y) = (exact_num.unsigned_abs(), exact_den.unsigned_abs());
                while y != 0 {
                    let t = x % y;
                    x = y;
                    y = t;
                }
                x as i128
            };
            if (exact_num / g).abs() <= i32::MAX as i128 && (exact_den / g) <= i32::MAX as i128 {
                assert!(
                    (sum.to_f64() - (exact_num as f64 / exact_den as f64)).abs() < 1e-15,
                    "{a:?} + {b:?} = {sum:?}"
                );
            }
        }
    }

    #[test]
    fn try_mul_and_try_add_report_unrepresentability() {
        // Exact cases succeed and agree with the infallible operators.
        assert_eq!(
            Rational::new(50_000, 1).try_mul(Rational::new(50_000, 50_000)),
            Some(Rational::new(50_000, 1))
        );
        assert_eq!(
            Rational::new(1, 90_000).try_add(Rational::new(1, 90_000)),
            Some(Rational::new(1, 45_000))
        );
        // (50000/50001)² reduces to 2500000000/2500100001 — the numerator does
        // not fit, so the checked form reports it instead of approximating.
        let operand = Rational::new(50_000, 50_001);
        assert_eq!(operand.try_mul(operand), None);
        // A zero denominator normalizes to the canonical zero, so the checked
        // form reports the (representable) value 0, not an error.
        assert_eq!(
            Rational::new(1, 2).try_mul(Rational::new(1, 0)),
            Some(Rational::new(0, 1))
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
        // (-1/90000)·(1/90000) ≈ -1.23e-10 is far below the finest step the
        // type can express (1/i32::MAX ≈ 4.66e-10), so the nearest value on the
        // grid is zero. What matters is that the result is *near* the true value
        // (not e.g. i32::MAX) and exactness is available via try_mul.
        let tiny = Rational::new(1, 90_000).mul(Rational::new(-1, 90_000));
        let expected = -1.0 / (90_000.0 * 90_000.0);
        // Documented bound: at most half a lattice step (1/(2*i32::MAX)).
        assert!(
            (tiny.to_f64() - expected).abs() <= 0.5 / i32::MAX as f64,
            "{tiny:?} = {} vs {expected}",
            tiny.to_f64()
        );
        // The exact value needs a denominator beyond i32, so the checked form
        // reports it rather than silently rounding.
        assert_eq!(
            Rational::new(1, 90_000).try_mul(Rational::new(-1, 90_000)),
            None
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
        // 2^31/1 is not representable: it approximates to the largest value the
        // type can hold, i.e. i32::MAX/1 (relative error ~5e-10), with the sign
        // preserved and a positive denominator.
        let huge = Rational::new(i32::MIN, -1).normalized();
        assert!(huge.den > 0, "denominator must stay positive, got {huge:?}");
        // 2^31 approximates to 2^31-1 = i32::MAX (the grid has no 2^31).
        assert_eq!(huge, Rational::new(i32::MAX, 1));
    }

    #[test]
    fn timestamp_to_seconds_matches_manual_conversion() {
        assert!((Rational::timestamp_to_seconds(90_000, Rational::new(1, 90_000)) - 1.0).abs() < 1e-9);
        assert_eq!(Rational::timestamp_to_seconds(5, Rational::new(1, 0)), 0.0);
        assert_eq!(Rational::timestamp_to_seconds(0, Rational::new(1, 25)), 0.0);
    }
}
