//! Pure timestamp conversions between a track's `timescale` and milliseconds.
//!
//! These are building blocks for demux seek; no media parsing required.

/// Convert a sample count at `timescale` to milliseconds.
pub fn samples_to_ms(samples: u64, timescale: u32) -> f64 {
    if timescale == 0 {
        return 0.0;
    }
    samples as f64 * 1000.0 / timescale as f64
}

/// Convert milliseconds to the nearest sample index at `timescale`.
pub fn ms_to_samples(ms: f64, timescale: u32) -> u64 {
    if timescale == 0 {
        return 0;
    }
    (ms * timescale as f64 / 1000.0).round() as u64
}

/// Convert a sample count at `timescale` to seconds.
pub fn samples_to_secs(samples: u64, timescale: u32) -> f64 {
    if timescale == 0 {
        return 0.0;
    }
    samples as f64 / timescale as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_to_ms_basic() {
        assert_eq!(samples_to_ms(44100, 44100), 1000.0);
    }

    #[test]
    fn ms_to_samples_basic() {
        assert_eq!(ms_to_samples(1000.0, 44100), 44100);
    }

    #[test]
    fn round_trip() {
        let samples = ms_to_samples(samples_to_ms(12345, 90000), 90000);
        assert!((samples as i64 - 12345).abs() <= 1);
    }

    #[test]
    fn zero_timescale_safe() {
        assert_eq!(samples_to_ms(100, 0), 0.0);
        assert_eq!(ms_to_samples(100.0, 0), 0);
    }
}
