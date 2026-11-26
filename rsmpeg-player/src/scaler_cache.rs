//! [`Scaler`] cache to avoid rebuilding a `Scaler` every frame.
//!
//! Working on a single worker thread, a thread-local cache keeps the hottest
//! path lock-free while still reusing the same `Scaler` for every frame that
//! shares `(pixel format, width, height)`. This addresses the Phase 6.1 perf
//! TODO: "解析度不變時不得每 frame 重建 scaler".
//!
//! The cache is bounded by [`MAX_CACHED_SCALERS`], so a stream that keeps
//! changing resolution cannot grow it without limit.
use std::cell::RefCell;
use std::collections::hash_map::Entry;
use std::collections::HashMap;

use rsmpeg_codec::Frame;
use rsmpeg_scale::{Scaler, ScalerConfig};
use rsmpeg_util::{PixelFormat, RsError, RsResult};

/// Most scalers one thread keeps; the cache is emptied when a new key would
/// exceed this.
const MAX_CACHED_SCALERS: usize = 16;

type ScalerKey = (PixelFormat, u32, u32);

thread_local! {
    static CACHE: RefCell<HashMap<ScalerKey, Scaler>> = RefCell::new(HashMap::new());
}

/// Convert a `Yuv420P`, `Yuv422P`, `Yuv444P` or `Nv12` [`Frame`] to
/// tightly-packed RGBA (`w * h * 4`), reusing a cached [`Scaler`] keyed by
/// `(pixel format, width, height)`. Returns the first (RGBA) data plane.
pub fn yuv_to_rgba_cached(frame: &Frame) -> RsResult<Vec<u8>> {
    if !matches!(
        frame.pixel_format,
        PixelFormat::Yuv420P | PixelFormat::Yuv422P | PixelFormat::Yuv444P | PixelFormat::Nv12
    ) {
        return Err(RsError::InvalidData(
            format!(
                "yuv_to_rgba_cached does not support {:?}",
                frame.pixel_format
            )
            .into(),
        ));
    }
    if frame.width == 0 || frame.height == 0 {
        return Err(RsError::InvalidData(
            "yuv_to_rgba_cached requires non-zero dimensions".into(),
        ));
    }
    let (Ok(width), Ok(height)) = (u32::try_from(frame.width), u32::try_from(frame.height)) else {
        return Err(RsError::InvalidData(
            "yuv_to_rgba_cached: dimensions exceed u32".into(),
        ));
    };

    let key = (frame.pixel_format, width, height);

    // Build (or reuse) the cached Scaler, then scale. `Scaler` is not `Clone`,
    // so we keep it in the map and call `scale(&self, ..)` via an immutable
    // borrow — `scale` returns an owned `Frame`, so no borrow conflict arises.
    let out = CACHE.with(|c| -> RsResult<Frame> {
        let mut guard = c.borrow_mut();
        if guard.len() >= MAX_CACHED_SCALERS && !guard.contains_key(&key) {
            guard.clear();
        }
        // Build once per key, then reuse. `Scaler::new` can fail, so we cannot
        // use `or_insert_with`; the entry is created only on the vacant path and
        // the borrow below never needs an `expect`/`unwrap`.
        match guard.entry(key) {
            Entry::Occupied(e) => e.into_mut().scale(frame),
            Entry::Vacant(e) => {
                let config = ScalerConfig::new(
                    width,
                    height,
                    frame.pixel_format,
                    width,
                    height,
                    PixelFormat::Rgba,
                );
                let scaler = Scaler::new(config)?;
                e.insert(scaler).scale(frame)
            }
        }
    })?;

    out.data
        .into_iter()
        .next()
        .ok_or_else(|| RsError::Bug("scaler produced empty RGBA plane list".into()))
}

/// [`yuv_to_rgba_cached`] restricted to `Yuv420P` input. Output is
/// byte-identical to [`crate::video_convert::yuv420p_frame_to_rgba`].
pub fn yuv420p_to_rgba_cached(frame: &Frame) -> RsResult<Vec<u8>> {
    if frame.pixel_format != PixelFormat::Yuv420P {
        return Err(RsError::InvalidData(
            "yuv420p_to_rgba_cached requires PixelFormat::Yuv420P".into(),
        ));
    }
    yuv_to_rgba_cached(frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsmpeg_codec::PictureType;
    use rsmpeg_util::Rational;

    fn solid_yuv420p(w: usize, h: usize, y: u8, u: u8, v: u8) -> Frame {
        let cw = w.div_ceil(2);
        let ch = h.div_ceil(2);
        Frame {
            data: vec![vec![y; w * h], vec![u; cw * ch], vec![v; cw * ch]],
            linesize: vec![w, cw, cw],
            width: w,
            height: h,
            pixel_format: PixelFormat::Yuv420P,
            sample_format: rsmpeg_util::SampleFormat::None,
            sample_rate: 0,
            channels: 0,
            samples: 0,
            pts: Some(0),
            duration: 1,
            time_base: Rational::new(1, 30),
            key_frame: true,
            pict_type: PictureType::I,
        }
    }

    /// Uniform-colour frame in any of the cached YUV layouts.
    fn solid_yuv(format: PixelFormat, w: usize, h: usize, y: u8, u: u8, v: u8) -> Frame {
        let mut frame = solid_yuv420p(w, h, y, u, v);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        frame.pixel_format = format;
        match format {
            PixelFormat::Yuv420P => {}
            PixelFormat::Yuv422P => {
                frame.data = vec![vec![y; w * h], vec![u; cw * h], vec![v; cw * h]];
            }
            PixelFormat::Yuv444P => {
                frame.data = vec![vec![y; w * h], vec![u; w * h], vec![v; w * h]];
                frame.linesize = vec![w, w, w];
            }
            PixelFormat::Nv12 => {
                frame.data = vec![vec![y; w * h], [u, v].repeat(cw * ch)];
                frame.linesize = vec![w, cw * 2];
            }
            other => panic!("unsupported test format {other:?}"),
        }
        frame
    }

    fn cached_scaler_count() -> usize {
        CACHE.with(|c| c.borrow().len())
    }

    #[test]
    fn cached_reuses_scaler_same_dims() {
        let frame = solid_yuv420p(16, 10, 16, 128, 128);
        let a = yuv420p_to_rgba_cached(&frame).unwrap();
        assert_eq!(a.len(), 16 * 10 * 4);
        // Second call must reuse the cached Scaler (same dims) and still be valid.
        let b = yuv420p_to_rgba_cached(&frame).unwrap();
        assert_eq!(b.len(), 16 * 10 * 4);
        assert_eq!(cached_scaler_count(), 1);
    }

    #[test]
    fn cached_rejects_non_yuv() {
        let mut frame = solid_yuv420p(4, 4, 16, 128, 128);
        frame.pixel_format = PixelFormat::Rgb24;
        assert!(yuv420p_to_rgba_cached(&frame).is_err());
        assert!(yuv_to_rgba_cached(&frame).is_err());
        // The 4:2:0-only entry point still refuses the other YUV layouts.
        let frame = solid_yuv(PixelFormat::Yuv422P, 4, 4, 16, 128, 128);
        assert!(yuv420p_to_rgba_cached(&frame).is_err());
    }

    #[test]
    fn cached_rejects_zero_dimensions() {
        let frame = Frame::new_video(0, 0, PixelFormat::Yuv420P);
        assert!(yuv420p_to_rgba_cached(&frame).is_err());
        let mut frame = solid_yuv420p(4, 4, 16, 128, 128);
        frame.width = 0;
        assert!(yuv420p_to_rgba_cached(&frame).is_err());
        assert!(yuv_to_rgba_cached(&frame).is_err());
    }

    #[test]
    fn cached_output_matches_uncached_conversion() {
        // The cache must never change pixels, only reuse the scaler.
        let frame = solid_yuv420p(16, 10, 81, 90, 240);
        let cached = yuv420p_to_rgba_cached(&frame).unwrap();
        let direct = crate::video_convert::yuv420p_frame_to_rgba(&frame).unwrap();
        assert_eq!(cached, direct);
        assert_eq!(yuv_to_rgba_cached(&frame).unwrap(), direct);
    }

    #[test]
    fn cache_handles_multiple_resolutions_and_switches_back() {
        let small = solid_yuv420p(8, 8, 16, 128, 128);
        let large = solid_yuv420p(32, 24, 235, 128, 128);

        let s1 = yuv420p_to_rgba_cached(&small).unwrap();
        let l1 = yuv420p_to_rgba_cached(&large).unwrap();
        // Switching back must hit the first cached scaler and still be correct.
        let s2 = yuv420p_to_rgba_cached(&small).unwrap();

        assert_eq!(s1.len(), 8 * 8 * 4);
        assert_eq!(l1.len(), 32 * 24 * 4);
        assert_eq!(s1, s2, "returning to a cached resolution must be stable");
        // Sanity: the small frame is limited-range black, the large one white.
        assert!(s1[0] <= 2);
        assert!(l1[0] >= 250);
    }

    #[test]
    fn cache_keys_on_pixel_format_as_well_as_size() {
        // Same size, different layouts: a scaler built for one format must never
        // be handed a frame of another.
        let formats = [
            PixelFormat::Yuv420P,
            PixelFormat::Yuv422P,
            PixelFormat::Yuv444P,
            PixelFormat::Nv12,
        ];
        let expected =
            yuv420p_to_rgba_cached(&solid_yuv420p(8, 6, 81, 90, 240)).expect("reference");
        for _ in 0..2 {
            for format in formats {
                let frame = solid_yuv(format, 8, 6, 81, 90, 240);
                assert_eq!(yuv_to_rgba_cached(&frame).unwrap(), expected, "{format:?}");
            }
        }
        assert_eq!(cached_scaler_count(), formats.len());
    }

    #[test]
    fn cache_stays_bounded_under_resolution_churn() {
        for width in 1..=3 * MAX_CACHED_SCALERS + 1 {
            let frame = solid_yuv420p(width, 2, 235, 128, 128);
            let rgba = yuv_to_rgba_cached(&frame).unwrap();
            assert_eq!(rgba.len(), width * 2 * 4);
            assert!(cached_scaler_count() <= MAX_CACHED_SCALERS);
        }
    }

    #[test]
    fn full_cache_is_kept_when_the_key_is_already_present() {
        for width in 1..=MAX_CACHED_SCALERS {
            yuv_to_rgba_cached(&solid_yuv420p(width, 2, 16, 128, 128)).unwrap();
        }
        assert_eq!(cached_scaler_count(), MAX_CACHED_SCALERS);
        yuv_to_rgba_cached(&solid_yuv420p(1, 2, 16, 128, 128)).unwrap();
        assert_eq!(cached_scaler_count(), MAX_CACHED_SCALERS);
        // A new key empties the cache before it is inserted.
        yuv_to_rgba_cached(&solid_yuv420p(64, 2, 16, 128, 128)).unwrap();
        assert_eq!(cached_scaler_count(), 1);
    }
}
