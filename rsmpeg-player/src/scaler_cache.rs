//! Per-resolution [`Scaler`] cache to avoid rebuilding a `Scaler` every frame.
//!
//! Working on a single worker thread, a thread-local cache keeps the hottest
//! path lock-free while still reusing the same `Scaler` for every frame that
//! shares `(width, height)`. This addresses the Phase 6.1 perf TODO:
//! "解析度不變時不得每 frame 重建 scaler".
use std::cell::RefCell;
use std::collections::HashMap;

use rsmpeg_codec::Frame;
use rsmpeg_scale::{Scaler, ScalerConfig};
use rsmpeg_util::{PixelFormat, RsError, RsResult};

thread_local! {
    static CACHE: RefCell<HashMap<(u32, u32), Scaler>> = RefCell::new(HashMap::new());
}

/// Scale a YUV420P [`Frame`] to tightly-packed RGBA (`w * h * 4`), reusing a
/// cached [`Scaler`] keyed by `(width, height)`. Returns the first (RGBA) data
/// plane. Output is byte-identical to [`crate::video_convert::yuv420p_frame_to_rgba`].
pub fn yuv420p_to_rgba_cached(frame: &Frame) -> RsResult<Vec<u8>> {
    if frame.pixel_format != PixelFormat::Yuv420P {
        return Err(RsError::InvalidData(
            "yuv420p_to_rgba_cached requires PixelFormat::Yuv420P".into(),
        ));
    }
    if frame.width == 0 || frame.height == 0 {
        return Err(RsError::InvalidData(
            "yuv420p_to_rgba_cached requires non-zero dimensions".into(),
        ));
    }

    let key = (frame.width as u32, frame.height as u32);

    // Build (or reuse) the cached Scaler, then scale. `Scaler` is not `Clone`,
    // so we keep it in the map and call `scale(&self, ..)` via an immutable
    // borrow — `scale` returns an owned `Frame`, so no borrow conflict arises.
    let out = CACHE.with(|c| -> RsResult<Frame> {
        let mut guard = c.borrow_mut();
        if !guard.contains_key(&key) {
            let config = ScalerConfig::new(
                frame.width as u32,
                frame.height as u32,
                PixelFormat::Yuv420P,
                frame.width as u32,
                frame.height as u32,
                PixelFormat::Rgba,
            );
            let scaler = Scaler::new(config)?;
            guard.insert(key, scaler);
        }
        let scaler = guard.get(&key).expect("scaler just inserted for key");
        scaler.scale(frame)
    })?;

    out.data
        .into_iter()
        .next()
        .ok_or_else(|| RsError::Bug("scaler produced empty RGBA plane list".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsmpeg_codec::PictureType;
    use rsmpeg_util::Rational;

    fn solid_yuv420p(w: usize, h: usize, y: u8, u: u8, v: u8) -> Frame {
        let cw = (w + 1) / 2;
        let ch = (h + 1) / 2;
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

    #[test]
    fn cached_reuses_scaler_same_dims() {
        let frame = solid_yuv420p(16, 10, 16, 128, 128);
        let a = yuv420p_to_rgba_cached(&frame).unwrap();
        assert_eq!(a.len(), 16 * 10 * 4);
        // Second call must reuse the cached Scaler (same dims) and still be valid.
        let b = yuv420p_to_rgba_cached(&frame).unwrap();
        assert_eq!(b.len(), 16 * 10 * 4);
    }

    #[test]
    fn cached_rejects_non_yuv() {
        let mut frame = solid_yuv420p(4, 4, 16, 128, 128);
        frame.pixel_format = PixelFormat::Rgb24;
        assert!(yuv420p_to_rgba_cached(&frame).is_err());
    }
}
