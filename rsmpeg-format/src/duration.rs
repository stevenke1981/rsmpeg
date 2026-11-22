//! Duration conversions between a track's `timescale` and [`std::time::Duration`].
//!
//! Complements [`crate::time_util`] (which works in milliseconds as `f64`).

use std::time::Duration;

/// Convert a sample count at `timescale` to a [`Duration`].
pub fn samples_to_duration(samples: u64, timescale: u64) -> Duration {
    if timescale == 0 {
        return Duration::ZERO;
    }
    let secs = samples / timescale;
    let rem = samples % timescale;
    // Sub-second remainder as nanoseconds. The 128-bit intermediate prevents the
    // overflow that `rem * 1_000_000_000` would hit for large timescales, and the
    // result is always < 1e9 so the `u32` conversion cannot fail — saturate
    // instead of `unwrap()` so a pathological input can never panic.
    let nanos = u32::try_from(rem as u128 * 1_000_000_000 / timescale as u128)
        .unwrap_or(u32::MAX)
        .min(999_999_999);
    Duration::new(secs, nanos)
}

/// Convert a [`Duration`] to the nearest sample index at `timescale`.
pub fn duration_to_samples(d: Duration, timescale: u64) -> u64 {
    if timescale == 0 {
        return 0;
    }
    d.as_secs() * timescale + (d.subsec_nanos() as u64 * timescale / 1_000_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_to_duration_basic() {
        assert_eq!(samples_to_duration(44100, 44100), Duration::from_secs(1));
    }

    #[test]
    fn duration_to_samples_basic() {
        assert_eq!(duration_to_samples(Duration::from_secs(1), 44100), 44100);
    }

    #[test]
    fn round_trip() {
        let d = samples_to_duration(123456, 90000);
        let samples = duration_to_samples(d, 90000);
        assert!((samples as i64 - 123456).abs() <= 1);
    }

    #[test]
    fn zero_timescale_safe() {
        assert_eq!(samples_to_duration(100, 0), Duration::ZERO);
        assert_eq!(duration_to_samples(Duration::from_secs(1), 0), 0);
    }

    #[test]
    fn sub_second_remainder_becomes_nanos() {
        // 22050 samples at 44100 Hz = exactly 0.5 s.
        let d = samples_to_duration(22_050, 44_100);
        assert_eq!(d, Duration::from_millis(500));

        // 1 sample at 3 Hz = 1/3 s ≈ 333.33 ms.
        let d = samples_to_duration(1, 3);
        assert_eq!(d.subsec_nanos(), 333_333_333);
    }

    #[test]
    fn large_timescale_does_not_overflow() {
        // 1e9 timescale with a large remainder: the previous u64 intermediate
        // (`rem * 1_000_000_000`) would have overflowed for rem > ~18.4e9.
        let timescale = u64::MAX / 1000;
        let d = samples_to_duration(timescale, timescale);
        assert_eq!(d, Duration::from_secs(1));
    }

    #[test]
    fn nanos_are_always_valid() {
        // Whatever the input, the nanosecond field must stay below one second so
        // `Duration::new` never has to carry.
        for timescale in [1u64, 7, 1000, 90_000, 44_100, 1_000_000_000] {
            for samples in [0u64, 1, 999, 123_456, timescale, timescale * 3 + 1] {
                let d = samples_to_duration(samples, timescale);
                assert!(
                    d.subsec_nanos() < 1_000_000_000,
                    "timescale={timescale} samples={samples}"
                );
            }
        }
    }

    #[test]
    fn duration_to_samples_rounds_down_sub_sample_time() {
        // 0.9 sample periods at 10 Hz still map to 0 whole samples.
        assert_eq!(duration_to_samples(Duration::from_millis(90), 10), 0);
        // 1.5 sample periods map to 1 (truncating, not rounding).
        assert_eq!(duration_to_samples(Duration::from_millis(150), 10), 1);
        // A large duration must not overflow (uses mul then divide on u64).
        assert_eq!(
            duration_to_samples(Duration::from_secs(u32::MAX as u64), 48_000),
            u32::MAX as u64 * 48_000
        );
    }
}
