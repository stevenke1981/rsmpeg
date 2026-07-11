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
    // sub-second remainder as nanoseconds (always < 1_000_000_000, fits u32)
    let nanos = u32::try_from((rem as u128 * 1_000_000_000 / timescale as u128) as u64).unwrap();
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
}
