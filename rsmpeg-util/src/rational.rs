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

/// Nearest representable value of a reduced wide pair, or `None` when the value
/// is undefined (`den == 0`).
///
/// # Contract
///
/// * If the exact, already-reduced pair fits in `i32`, it is returned verbatim —
///   no rounding happens at all.
/// * Otherwise the returned fraction is the **closest representable value**:
///   there is no `p/q` with `|p| <= i32::MAX, 1 <= q <= i32::MAX` whose value is
///   nearer to the exact rational. Ties prefer the smaller denominator.
///   Closeness is decided by exact integer cross multiplication, never by
///   floating point.
///
/// Callers that cannot tolerate any approximation must use the checked forms
/// ([`Rational::try_mul`], [`Rational::try_add`], [`Rational::try_normalized`]),
/// which report unrepresentability instead of approximating.
fn approximate_wide(num: Wide, den: Wide) -> Option<Rational> {
    let (num, den) = normalize_wide(num, den);
    if den == 0 {
        return None;
    }
    if let Some(exact) = checked_wide(num, den) {
        return Some(exact);
    }
    best_approximation(num, den)
}

/// Closest representable value to `num/den`, found from the continued-fraction
/// expansion.
///
/// The representable set is `{p/q : 0 <= p <= i32::MAX, 1 <= q <= i32::MAX}`
/// (sign handled by the caller). Among the fractions with `q <= max` the closest
/// to `num/den` is always either
///
/// * the last convergent that still fits the bound, or
/// * the **maximal semiconvergent** between it and the first convergent that
///   overflows, i.e. `p0 + k*p1 / (q0 + k*q1)` for the largest feasible `k`.
///
/// This is the classical "best rational approximation with bounded denominator"
/// result. The earlier implementation compared raw `|p*den - num*q|` magnitudes
/// and never formed the semiconvergent, so it settled on `99999/100003` for
/// `(50000/50001)^2` — about six times further from the target than the
/// representable `2147303527/2147389420`.
///
/// Both candidates are compared by exact integer cross multiplication of
/// `|p*den - num*q| / q`, never by floating point, so the choice is the true
/// minimum over the representable set rather than an estimate.
///
/// Callers must pass an already-normalized, non-representable pair.
fn best_approximation(num: Wide, den: Wide) -> Option<Rational> {
    let max = i32::MAX as Wide;
    let negative = num < 0;
    let (num, den) = (num.abs(), den.abs());
    if den == 0 {
        return None;
    }

    // `(|p*den - num*q|, p, q)` for the best candidate so far. Two candidates are
    // ordered by `|p*den - num*q| / q` (the value error times the constant
    // `den`), compared by cross multiplication so the ordering is exact.
    let mut best: Option<(Wide, Wide, Wide)> = None;
    let mut consider = |p: Wide, q: Wide| {
        if q < 1 || q > max || p > max || p < 0 {
            return;
        }
        let error = (p * den - num * q).abs();
        best = match best {
            None => Some((error, p, q)),
            Some((be, bp, bq)) => {
                let left = error * bq;
                let right = be * q;
                // Ties keep the smaller denominator for a canonical result.
                if left < right || (left == right && q < bq) {
                    Some((error, p, q))
                } else {
                    Some((be, bp, bq))
                }
            }
        };
    };

    // Continued-fraction expansion. `(p1, q1)` is the last convergent whose
    // denominator still fits the bound and `(p0, q0)` the convergent before it.
    let (mut p0, mut q0) = (0 as Wide, 1 as Wide);
    let (mut p1, mut q1) = (1 as Wide, 0 as Wide);
    let (mut n, mut d) = (num, den);
    while d != 0 {
        let a = n / d;
        let p2 = p0 + a * p1;
        let q2 = q0 + a * q1;
        // Both components are bounded. `2^31/1` overflows the *numerator* on the
        // very first step while its denominator stays 1, so checking only `q`
        // let an unrepresentable numerator through and the tracked pair ended up
        // describing a different value entirely.
        if q2 > max || p2 > max {
            break;
        }
        (p0, q0, p1, q1) = (p1, q1, p2, q2);
        let r = n - a * d;
        n = d;
        d = r;
    }

    // The optimum is one of the two neighbours of the target on the bounded
    // grid: the last fitting convergent, or the maximal semiconvergent between
    // it and the first one that overflowed.
    let k = (max - q0) / q1.max(1);
    if k > 0 {
        let (p, q) = (p0 + k * p1, q0 + k * q1);
        consider(p, q);
        // The nearest numerator for that denominator can differ from the
        // semiconvergent's own, so probe the rounded one as well.
        consider(round_div(num * q, den).min(max), q);
    }
    consider(p1, q1);
    // The rounded numerator for a boundary denominator can beat both neighbours.
    // `q == 1` matters when the numerator bound bites first: for `2^31/1` the
    // expansion overflows immediately, and only `round(2^31/1) = i32::MAX` at
    // denominator 1 is within one unit of the target.
    for q in [1, q1, q0.max(1), max, max - 1] {
        consider(round_div(num * q, den).min(max), q);
    }

    let (_, p, q) = best?;
    let r = Rational {
        num: p as i32,
        den: q as i32,
    };
    Some(if negative {
        Rational {
            num: -r.num,
            den: r.den,
        }
    } else {
        r
    })
}

/// `a / b` for positive `b`, rounded to nearest (halves away from zero).
fn round_div(a: Wide, b: Wide) -> Wide {
    debug_assert!(b > 0);
    let q = a / b;
    let r = a % b;
    if r.abs() * 2 >= b {
        q + if a < 0 { -1 } else { 1 }
    } else {
        q
    }
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
    // Check for an undefined value *before* normalizing: `normalize_wide` maps a
    // zero denominator onto the canonical zero, which is right for the
    // infallible operators but must never satisfy a checked one. `try_mul` on
    // `1/0` therefore reports `None`, exactly like `try_normalized`.
    if den == 0 || (num == 0 && den != 0 && false) {
        return None;
    }
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
    /// the **closest representable fraction** (see [`approximate_wide`] for the
    /// exact definition) — never a saturated `i32::MAX/1`. Use
    /// [`try_mul`](Self::try_mul) when an exact answer is required. Nothing
    /// panics.
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
        if self.den == 0 || other.den == 0 {
            return None;
        }
        checked_wide(
            self.num as Wide * other.num as Wide,
            self.den as Wide * other.den as Wide,
        )
    }

    /// Checked [`add`](Self::add): `None` when the exact sum is not
    /// representable as an `i32` fraction.
    pub fn try_add(self, other: Rational) -> Option<Self> {
        if self.den == 0 || other.den == 0 {
            return None;
        }
        checked_wide(
            self.num as Wide * other.den as Wide + other.num as Wide * self.den as Wide,
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
        // The clamps are best expressed as "as large as the type allows": the
        // nearest representable fraction to 2^31-1 squared is `i32::MAX/1`,
        // since any denominator above 1 pulls the value back under i32::MAX.
        assert_eq!(huge, Rational::new(i32::MAX, 1));
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
        assert_ne!(squared, Rational::new(i32::MAX, 1));
        // The result must be the closest representable fraction, compared by
        // exact cross multiplication against a rival that a magnitude-only
        // search preferred. `|p*den - num*q| <= 1` was the wrong criterion: for
        // a value near 1 the numerator bound `p <= i32::MAX` bites long before
        // the denominator bound, so the true optimum has a *large* numerator
        // error and a correspondingly small value error.
        let exact_num = 2_500_000_000i128;
        let exact_den = 2_500_100_001i128;
        let err = |r: Rational| (exact_num * r.den as i128 - exact_den * r.num as i128).abs();
        let rival = Rational::new(99_999, 100_003);
        assert!(
            err(squared) * (rival.den as i128) < err(rival) * (squared.den as i128),
            "{squared:?} must be closer than {rival:?}"
        );

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
    fn review_closest_representable_fraction_wins() {
        // Third review round: `1/3 + 1/i32::MAX` returned 715827883/2147483647
        // (error 1.55e-10) while 477218589/1431655765 is 1.08e-19 away. The
        // approximation must be the true nearest, verified by exact cross
        // multiplication (no floating point).
        let sum = Rational::new(1, 3).add(Rational::new(1, i32::MAX));
        let rival = Rational::new(477_218_589, 1_431_655_765);
        let exact_num = i32::MAX as i128 + 3;
        let exact_den = 3 * i32::MAX as i128;
        let err = |r: Rational| (exact_num * r.den as i128 - exact_den * r.num as i128).abs();
        assert!(
            err(sum) * rival.den as i128 <= err(rival) * sum.den as i128,
            "got {sum:?} (err {}) vs {rival:?} (err {})",
            err(sum),
            err(rival)
        );
    }

    #[test]
    fn review_checked_api_rejects_undefined_operands() {
        let invalid = Rational::new(1, 0);
        let valid = Rational::new(1, 2);
        assert_eq!(invalid.try_normalized(), None);
        assert_eq!(valid.try_mul(invalid), None);
        assert_eq!(valid.try_add(invalid), None);
        assert_eq!(invalid.try_add(valid), None);
        assert_eq!(invalid.try_mul(valid), None);
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
        // An undefined operand (zero denominator) is not a value: every checked
        // operation reports None, matching `try_normalized`.
        assert_eq!(Rational::new(1, 2).try_mul(Rational::new(1, 0)), None);
        assert_eq!(Rational::new(1, 2).try_add(Rational::new(1, 0)), None);
        assert_eq!(Rational::new(1, 0).try_mul(Rational::new(1, 2)), None);
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
        // 2^31 approximates to 2^31-1 = i32::MAX (the grid has no 2^31). The
        // expansion's very first convergent overflows here, so the seed pair has
        // to stay in the candidate set or the search falls through to a long
        // denominator and lands on `i32::MAX/(i32::MAX-1)` ≈ 1.0.
        assert_eq!(huge, Rational::new(i32::MAX, 1));
    }

    #[test]
    fn timestamp_to_seconds_matches_manual_conversion() {
        assert!((Rational::timestamp_to_seconds(90_000, Rational::new(1, 90_000)) - 1.0).abs() < 1e-9);
        assert_eq!(Rational::timestamp_to_seconds(5, Rational::new(1, 0)), 0.0);
        assert_eq!(Rational::timestamp_to_seconds(0, Rational::new(1, 25)), 0.0);
    }
}

#[cfg(test)]
mod review_fifth {
    use super::*;

    /// Exact value error, scaled by the denominators so it compares as an
    /// integer: `|p*den - num*q| / q` for the target `num/den`.
    fn scaled_error(num: Wide, den: Wide, r: Rational) -> Wide {
        (num * r.den as Wide - den * r.num as Wide).abs()
    }

    #[test]
    fn squared_value_50000_over_50001_is_the_true_nearest() {
        // Fifth review P2: `(50000/50001)^2` returned `99999/100003`
        // (error 4.0e-15) while `2147303527/2147389420` (error 6.6e-16) is
        // representable and six times closer. The value is just under 1, so the
        // binding constraint is the *numerator* bound, and the optimum is the
        // maximal semiconvergent rather than the largest denominator.
        let exact_num: Wide = 2_500_000_000;
        let exact_den: Wide = 2_500_100_001;
        let got = approximate_wide(exact_num, exact_den).unwrap();
        let witness = Rational::new(2_147_303_527, 2_147_389_420);

        assert_eq!(
            got, witness,
            "the documented nearest fraction is the representable witness"
        );
        assert!(
            scaled_error(exact_num, exact_den, got) * (witness.den as Wide)
                <= scaled_error(exact_num, exact_den, witness) * (got.den as Wide),
            "{got:?} must be at least as close as {witness:?}"
        );
    }

    #[test]
    fn nearest_fraction_beats_every_small_denominator() {
        // The contract is "no representable p/q is closer". Check that
        // exhaustively over a dense box: every `q <= 1200` and enough numerators
        // to bracket the target from both sides.
        const BOX: i128 = 1_200;
        for den in 1..=60i128 {
            for num in 1..=60i128 {
                let got = approximate_wide(num, den).unwrap();
                let got_error = scaled_error(num, den, got);
                for q in 1..=BOX {
                    let p = (num * q * 2 + den) / (2 * den);
                    if p > i32::MAX as i128 {
                        continue;
                    }
                    let rival_error = (num * q - den * p).abs();
                    assert!(
                        rival_error * (got.den as Wide) >= got_error * q,
                        "{num}/{den}: {p}/{q} is closer than the chosen {got:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn numerator_bound_is_respected_for_values_above_one() {
        // `2^31/1` overflows the numerator immediately, while the denominator
        // stays 1 — so a denominator-only bound would let it through and the
        // result collapsed to `i32::MAX/(i32::MAX-1)` (~1.0) for a value of 2^31.
        let got = approximate_wide(i32::MAX as Wide + 1, 1).unwrap();
        assert_eq!(got, Rational::new(i32::MAX, 1));
        // Just inside the box the exact value is returned unchanged.
        assert_eq!(
            approximate_wide(i32::MAX as Wide, 1).unwrap(),
            Rational::new(i32::MAX, 1)
        );
    }

    #[test]
    fn every_approximation_stays_inside_the_representable_box() {
        // Fuzz the shape of the inputs: both components bounded, denominator
        // positive, and a non-zero value (never the canonical 0/1 unless the
        // target really is 0).
        let max = i32::MAX as Wide;
        let samples = [
            (3 as Wide, 7 as Wide),
            (22, 7),
            (1, max),
            (max, 1),
            (max, max),
            (max - 1, max),
            (max * 3, max * 7),
            (1, 2),
            (123_456_789, 987_654_321),
        ];
        for (num, den) in samples {
            let r = approximate_wide(num, den).expect("non-zero denominator");
            assert!(r.den >= 1, "denominator must be positive: {r:?}");
            assert!(r.num >= 0, "numerator must keep the sign: {r:?}");
            assert!(
                (r.den as Wide) <= max && (r.num as Wide) <= max,
                "{r:?} escapes the representable box"
            );
            assert_ne!(r, Rational::new(0, 1), "{num}/{den} collapsed to zero");
        }
    }
}
