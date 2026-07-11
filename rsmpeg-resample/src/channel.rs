//! Standalone channel-layout conversion helpers (building blocks).
//!
//! These functions are pure, deterministic, and operate on interleaved sample
//! slices. They are not wired into the [`crate::Resampler`] and perform no
//! sample-rate conversion or I/O. They are intended as reusable audio-path
//! building blocks.
//!
//! Interleaved stereo layout is `[L0, R0, L1, R1, ...]`. Odd trailing samples
//! in stereo input are ignored by the downmix helpers.

/// Downmix interleaved stereo `f32` to mono by averaging each L/R pair.
///
/// The output has half the length of the input (rounded down). An odd trailing
/// stereo sample is ignored.
pub fn stereo_to_mono_f32(stereo: &[f32]) -> Vec<f32> {
    let n = stereo.len() / 2;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let l = stereo[2 * i];
        let r = stereo[2 * i + 1];
        out.push((l + r) * 0.5);
    }
    out
}

/// Upmix mono `f32` to interleaved stereo (L = R = mono).
///
/// The output has twice the length of the input: `[M0, M0, M1, M1, ...]`.
pub fn mono_to_stereo_f32(mono: &[f32]) -> Vec<f32> {
    let mut out = Vec::with_capacity(mono.len() * 2);
    for &s in mono {
        out.push(s);
        out.push(s);
    }
    out
}

/// Downmix interleaved stereo `i16` to mono by averaging each L/R pair.
///
/// The output has half the length of the input (rounded down). An odd trailing
/// stereo sample is ignored. Integer math is used to keep the conversion exact
/// and overflow-safe (two `i16` values fit comfortably in `i32`).
pub fn stereo_to_mono_i16(stereo: &[i16]) -> Vec<i16> {
    let n = stereo.len() / 2;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let l = stereo[2 * i] as i32;
        let r = stereo[2 * i + 1] as i32;
        out.push(((l + r) / 2) as i16);
    }
    out
}

/// Upmix mono `i16` to interleaved stereo (L = R = mono).
///
/// The output has twice the length of the input: `[M0, M0, M1, M1, ...]`.
pub fn mono_to_stereo_i16(mono: &[i16]) -> Vec<i16> {
    let mut out = Vec::with_capacity(mono.len() * 2);
    for &s in mono {
        out.push(s);
        out.push(s);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stereo_to_mono_f32_averages() {
        assert_eq!(stereo_to_mono_f32(&[0.0, 1.0, 2.0, 4.0]), vec![0.5, 3.0]);
    }

    #[test]
    fn mono_to_stereo_f32_duplicates() {
        assert_eq!(mono_to_stereo_f32(&[1.0, 2.0]), vec![1.0, 1.0, 2.0, 2.0]);
    }

    #[test]
    fn stereo_to_mono_i16_averages() {
        assert_eq!(stereo_to_mono_i16(&[-2, 2, 0, 4]), vec![0, 2]);
    }

    #[test]
    fn mono_to_stereo_i16_duplicates() {
        assert_eq!(mono_to_stereo_i16(&[5]), vec![5, 5]);
    }

    #[test]
    fn stereo_to_mono_f32_ignores_odd_trailing_sample() {
        // Length 5 (odd) → only the first pair is processed.
        assert_eq!(stereo_to_mono_f32(&[2.0, 4.0, 9.0]), vec![3.0]);
    }

    #[test]
    fn stereo_to_mono_i16_ignores_odd_trailing_sample() {
        assert_eq!(stereo_to_mono_i16(&[-2, 2, 7]), vec![0]);
    }

    #[test]
    fn stereo_to_mono_f32_empty() {
        let out: Vec<f32> = stereo_to_mono_f32(&[]);
        assert!(out.is_empty());
    }

    #[test]
    fn mono_to_stereo_f32_empty() {
        let out: Vec<f32> = mono_to_stereo_f32(&[]);
        assert!(out.is_empty());
    }

    #[test]
    fn roundtrip_f32() {
        // mono → stereo → mono should be identity for the mono signal.
        let mono = [1.5, -2.0, 3.25];
        let stereo = mono_to_stereo_f32(&mono);
        let back = stereo_to_mono_f32(&stereo);
        assert_eq!(mono.to_vec(), back);
    }
}
