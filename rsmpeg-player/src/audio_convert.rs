//! Thin helpers that convert decoded audio frames to device-ready PCM.
//!
//! Device path targets interleaved signed 16-bit (`S16`) at a host sample rate
//! and channel count (typically stereo 48 kHz / 44.1 kHz).
//!
//! # Conversion quality
//!
//! - **Identity path**: when the frame is already interleaved [`SampleFormat::S16`]
//!   at the target rate and channel count, bytes are re-read as little-endian `i16`
//!   with no resampling.
//! - **Resample path**: otherwise a [`rsmpeg_resample::Resampler`] is constructed and
//!   invoked. The resampler performs real linear-interpolation sample-rate conversion
//!   with format conversion (`S16`/`F32`) and basic channel remapping, producing
//!   correctly sized, non-silent output.

use rsmpeg_codec::Frame;
use rsmpeg_resample::{Resampler, ResamplerConfig};
use rsmpeg_util::{ChannelLayout, RsError, RsResult, SampleFormat};

/// Convert an audio [`Frame`] to interleaved `i16` at `target_rate` / `target_channels`.
///
/// Output layout is packed little-endian S16, channel order L-R for stereo
/// (`[L0, R0, L1, R1, …]`), or mono (`[M0, M1, …]`).
///
/// # Errors
///
/// - Non-audio / unknown sample format
/// - Zero target rate or channels
/// - Unsupported channel count for layout mapping
/// - Truncated plane data on the identity path
pub fn frame_to_s16_device(
    frame: &Frame,
    target_rate: u32,
    target_channels: u16,
) -> RsResult<Vec<i16>> {
    if target_rate == 0 || target_channels == 0 {
        return Err(RsError::InvalidData(
            "frame_to_s16_device requires non-zero target_rate and target_channels".into(),
        ));
    }
    if frame.sample_format == SampleFormat::None {
        return Err(RsError::InvalidData(
            "frame_to_s16_device requires an audio sample format".into(),
        ));
    }
    if frame.sample_rate == 0 || frame.channels == 0 {
        return Err(RsError::InvalidData(
            "frame_to_s16_device requires non-zero frame sample_rate and channels".into(),
        ));
    }

    // Fast path: already device-ready interleaved S16.
    if is_device_ready_s16(frame, target_rate, target_channels) {
        return s16_plane_to_i16(frame);
    }

    match frame.sample_format {
        SampleFormat::S32 => {
            let plane = validated_plane(frame, SampleFormat::S32)?;
            let samples: Vec<i16> = plane
                .chunks_exact(4)
                .map(|c| (i32::from_le_bytes([c[0], c[1], c[2], c[3]]) >> 16) as i16)
                .collect();
            deliver_converted_s16(frame, samples, target_rate, target_channels)
        }
        SampleFormat::U8 => {
            let plane = validated_plane(frame, SampleFormat::U8)?;
            let samples: Vec<i16> = plane.iter().map(|&b| (i16::from(b) - 128) << 8).collect();
            deliver_converted_s16(frame, samples, target_rate, target_channels)
        }
        _ => resample_to_s16(frame, target_rate, target_channels),
    }
}

fn is_device_ready_s16(frame: &Frame, target_rate: u32, target_channels: u16) -> bool {
    frame.sample_format == SampleFormat::S16
        && !frame.sample_format.is_planar()
        && frame.sample_rate == target_rate
        && frame.channels == target_channels
}

fn validated_plane(frame: &Frame, fmt: SampleFormat) -> RsResult<&[u8]> {
    if frame.samples == 0 {
        return Ok(&[]);
    }
    let expected = frame
        .samples
        .checked_mul(usize::from(frame.channels))
        .and_then(|n| n.checked_mul(fmt.bytes()))
        .ok_or_else(|| {
            RsError::InvalidData("frame_to_s16_device: audio plane size overflow".into())
        })?;
    let plane = frame.data.first().map(|d| d.as_slice()).unwrap_or(&[]);
    if plane.len() < expected {
        return Err(RsError::InvalidData(
            format!(
                "frame_to_s16_device: {} plane too short (have {}, need {})",
                fmt.name(),
                plane.len(),
                expected
            )
            .into(),
        ));
    }
    Ok(&plane[..expected])
}

fn deliver_converted_s16(
    src: &Frame,
    samples: Vec<i16>,
    target_rate: u32,
    target_channels: u16,
) -> RsResult<Vec<i16>> {
    if src.sample_rate == target_rate && src.channels == target_channels {
        return Ok(samples);
    }
    if samples.is_empty() {
        let _ = layout_for_channels(src.channels)?;
        let _ = layout_for_channels(target_channels)?;
        return Ok(Vec::new());
    }
    let mut tmp_bytes = Vec::with_capacity(samples.len() * 2);
    for s in &samples {
        tmp_bytes.extend_from_slice(&s.to_le_bytes());
    }
    let linesize = tmp_bytes.len();
    let tmp = Frame {
        data: vec![tmp_bytes],
        linesize: vec![linesize],
        width: 0,
        height: 0,
        pixel_format: rsmpeg_util::PixelFormat::None,
        sample_format: SampleFormat::S16,
        sample_rate: src.sample_rate,
        channels: src.channels,
        samples: src.samples,
        pts: src.pts,
        duration: src.duration,
        time_base: src.time_base,
        key_frame: src.key_frame,
        pict_type: src.pict_type,
    };
    resample_to_s16(&tmp, target_rate, target_channels)
}

fn resample_to_s16(frame: &Frame, target_rate: u32, target_channels: u16) -> RsResult<Vec<i16>> {
    let src_layout = layout_for_channels(frame.channels)?;
    let dst_layout = layout_for_channels(target_channels)?;

    let config = ResamplerConfig::new(
        frame.sample_rate,
        target_rate,
        frame.sample_format,
        SampleFormat::S16,
    )
    .with_channel_layouts(src_layout, dst_layout);

    let resampler = Resampler::new(config)?;
    let out = resampler.resample(frame)?;
    s16_plane_to_i16(&out)
}

fn layout_for_channels(channels: u16) -> RsResult<ChannelLayout> {
    match channels {
        1 => Ok(ChannelLayout::MONO),
        2 => Ok(ChannelLayout::STEREO),
        6 => Ok(ChannelLayout::_5POINT1),
        8 => Ok(ChannelLayout::_7POINT1),
        n => Err(RsError::Unsupported(
            format!("frame_to_s16_device: unsupported channel count {n}").into(),
        )),
    }
}

/// Read interleaved little-endian S16 plane bytes into `Vec<i16>`.
fn s16_plane_to_i16(frame: &Frame) -> RsResult<Vec<i16>> {
    let plane = frame.data.first().map(|d| d.as_slice()).unwrap_or(&[]);
    let expected = frame
        .samples
        .saturating_mul(frame.channels as usize)
        .saturating_mul(SampleFormat::S16.bytes());

    if frame.samples == 0 {
        return Ok(Vec::new());
    }
    if plane.len() < expected {
        return Err(RsError::InvalidData(
            format!(
                "frame_to_s16_device: S16 plane too short (have {}, need {})",
                plane.len(),
                expected
            )
            .into(),
        ));
    }

    Ok(plane[..expected]
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsmpeg_codec::PictureType;
    use rsmpeg_util::Rational;

    fn s16_stereo_frame(rate: u32, samples: &[i16]) -> Frame {
        // samples is interleaved L/R pairs; nb_samples = total_i16 / channels
        assert_eq!(samples.len() % 2, 0);
        let nb = samples.len() / 2;
        let mut data = Vec::with_capacity(samples.len() * 2);
        for s in samples {
            data.extend_from_slice(&s.to_le_bytes());
        }
        Frame {
            data: vec![data],
            linesize: vec![samples.len() * 2],
            width: 0,
            height: 0,
            pixel_format: rsmpeg_util::PixelFormat::None,
            sample_format: SampleFormat::S16,
            sample_rate: rate,
            channels: 2,
            samples: nb,
            pts: Some(0),
            duration: nb as i64,
            time_base: Rational::new(1, rate as i32),
            key_frame: true,
            pict_type: PictureType::I,
        }
    }

    #[test]
    fn identity_s16_stereo_same_rate() {
        let pcm: Vec<i16> = vec![0, 100, -100, 200, 300, -300];
        let frame = s16_stereo_frame(48_000, &pcm);
        let out = frame_to_s16_device(&frame, 48_000, 2).unwrap();
        assert_eq!(out, pcm);
    }

    #[test]
    fn identity_empty_samples() {
        let frame = Frame::new_audio(SampleFormat::S16, 44_100, 2, 0);
        let out = frame_to_s16_device(&frame, 44_100, 2).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn rejects_non_audio() {
        let mut frame = Frame::new_audio(SampleFormat::S16, 48_000, 2, 4);
        frame.sample_format = SampleFormat::None;
        assert!(frame_to_s16_device(&frame, 48_000, 2).is_err());
    }

    #[test]
    fn rejects_zero_target() {
        let frame = Frame::new_audio(SampleFormat::S16, 48_000, 2, 4);
        assert!(frame_to_s16_device(&frame, 0, 2).is_err());
        assert!(frame_to_s16_device(&frame, 48_000, 0).is_err());
    }

    #[test]
    fn rate_change_uses_resampler_length() {
        // Real resampler must size output via estimate and be non-silent.
        // 441 input samples @ 44100 → 480 @ 48000.
        let nb = 441;
        let mut interleaved = Vec::with_capacity(nb * 2);
        for i in 0..nb {
            interleaved.push((i as i16).wrapping_mul(3));
            interleaved.push(-(i as i16).wrapping_mul(3));
        }
        let frame = s16_stereo_frame(44_100, &interleaved);
        let out = frame_to_s16_device(&frame, 48_000, 2).unwrap();
        // Real resampler: correctly sized and non-silent.
        assert_eq!(out.len(), 480 * 2);
        assert!(
            out.iter().any(|&s| s != 0),
            "resampler must produce non-silent output"
        );
    }

    #[test]
    fn mono_identity() {
        let samples: Vec<i16> = vec![1, -2, 3, -4];
        let mut data = Vec::new();
        for s in &samples {
            data.extend_from_slice(&s.to_le_bytes());
        }
        let frame = Frame {
            data: vec![data],
            linesize: vec![samples.len() * 2],
            width: 0,
            height: 0,
            pixel_format: rsmpeg_util::PixelFormat::None,
            sample_format: SampleFormat::S16,
            sample_rate: 48_000,
            channels: 1,
            samples: samples.len(),
            pts: Some(0),
            duration: samples.len() as i64,
            time_base: Rational::new(1, 48_000),
            key_frame: true,
            pict_type: PictureType::I,
        };
        let out = frame_to_s16_device(&frame, 48_000, 1).unwrap();
        assert_eq!(out, samples);
    }

    #[test]
    fn extreme_samples_survive_the_identity_path() {
        // i16::MIN/MAX must round-trip bit-exactly through the LE byte view.
        let pcm: Vec<i16> = vec![i16::MIN, i16::MAX, -1, 0, 1, -32767];
        let frame = s16_stereo_frame(48_000, &pcm);
        let out = frame_to_s16_device(&frame, 48_000, 2).unwrap();
        assert_eq!(out, pcm);
    }

    #[test]
    fn truncated_s16_plane_is_an_error() {
        // Declared 4 samples x 2 ch x 2 bytes = 16 bytes, but only 6 present.
        let mut frame = Frame::new_audio(SampleFormat::S16, 48_000, 2, 4);
        frame.data[0].truncate(6);
        assert!(frame_to_s16_device(&frame, 48_000, 2).is_err());
    }

    #[test]
    fn unsupported_channel_count_is_rejected() {
        // 3 channels has no ChannelLayout mapping for the resample path.
        let mut frame = Frame::new_audio(SampleFormat::S16, 44_100, 3, 8);
        frame.data[0].fill(1);
        let err = frame_to_s16_device(&frame, 48_000, 3).unwrap_err();
        assert!(
            matches!(err, RsError::Unsupported(_)),
            "expected Unsupported, got {err:?}"
        );
    }

    #[test]
    fn mono_to_stereo_upmix_produces_two_channels() {
        // 100 mono samples at 48 kHz → stereo at 48 kHz keeps 100 frames.
        let nb = 100usize;
        let mut data = Vec::new();
        for i in 0..nb {
            data.extend_from_slice(&((i as i16) * 10).to_le_bytes());
        }
        let frame = Frame {
            data: vec![data],
            linesize: vec![nb * 2],
            width: 0,
            height: 0,
            pixel_format: rsmpeg_util::PixelFormat::None,
            sample_format: SampleFormat::S16,
            sample_rate: 48_000,
            channels: 1,
            samples: nb,
            pts: Some(0),
            duration: nb as i64,
            time_base: Rational::new(1, 48_000),
            key_frame: true,
            pict_type: PictureType::I,
        };
        let out = frame_to_s16_device(&frame, 48_000, 2).unwrap();
        assert_eq!(out.len(), nb * 2, "upmix must yield interleaved stereo");
        assert!(out.iter().any(|&s| s != 0), "upmix must not be silent");
    }

    #[test]
    fn resample_downsample_halves_the_sample_count() {
        // 48 kHz → 24 kHz: 480 input samples become 240 output samples.
        let nb = 480usize;
        let mut interleaved = Vec::with_capacity(nb * 2);
        for i in 0..nb {
            let v = ((i as i16) % 1000) * 30;
            interleaved.push(v);
            interleaved.push(-v);
        }
        let frame = s16_stereo_frame(48_000, &interleaved);
        let out = frame_to_s16_device(&frame, 24_000, 2).unwrap();
        assert_eq!(out.len(), 240 * 2);
        assert!(out.iter().any(|&s| s != 0));
    }

    fn s32_stereo_frame(rate: u32, samples: &[i32]) -> Frame {
        assert_eq!(samples.len() % 2, 0);
        let nb = samples.len() / 2;
        let mut data = Vec::with_capacity(samples.len() * 4);
        for s in samples {
            data.extend_from_slice(&s.to_le_bytes());
        }
        Frame {
            data: vec![data],
            linesize: vec![samples.len() * 4],
            width: 0,
            height: 0,
            pixel_format: rsmpeg_util::PixelFormat::None,
            sample_format: SampleFormat::S32,
            sample_rate: rate,
            channels: 2,
            samples: nb,
            pts: Some(0),
            duration: nb as i64,
            time_base: Rational::new(1, rate as i32),
            key_frame: true,
            pict_type: PictureType::I,
        }
    }

    fn u8_stereo_frame(rate: u32, samples: &[u8]) -> Frame {
        assert_eq!(samples.len() % 2, 0);
        let nb = samples.len() / 2;
        Frame {
            data: vec![samples.to_vec()],
            linesize: vec![samples.len()],
            width: 0,
            height: 0,
            pixel_format: rsmpeg_util::PixelFormat::None,
            sample_format: SampleFormat::U8,
            sample_rate: rate,
            channels: 2,
            samples: nb,
            pts: Some(0),
            duration: nb as i64,
            time_base: Rational::new(1, rate as i32),
            key_frame: true,
            pict_type: PictureType::I,
        }
    }

    #[test]
    fn s32_24bit_shifted_same_rate_and_resampled() {
        // 24-bit signed values shifted left by 8 into S32 (matching FlacDecoder output).
        let raw_24: [i32; 6] = [
            0x00_123400,
            -0x00_234500,
            0x00_7FFF00,
            -0x00_800000,
            1000,
            -1000,
        ];
        let s32_vals: Vec<i32> = raw_24.iter().map(|&v| v << 8).collect();
        let expected: Vec<i16> = s32_vals.iter().map(|&v| (v >> 16) as i16).collect();
        let frame = s32_stereo_frame(48_000, &s32_vals);

        let out_direct = frame_to_s16_device(&frame, 48_000, 2).unwrap();
        assert_eq!(out_direct, expected);

        // Resampled path (44.1 kHz -> 48 kHz)
        let nb = 441;
        let mut s32_44k = Vec::with_capacity(nb * 2);
        for i in 0..nb {
            let v = (((i as i32 % 100) + 1) * 500) << 16;
            s32_44k.push(v);
            s32_44k.push(-v);
        }
        let frame_44k = s32_stereo_frame(44_100, &s32_44k);
        let out_resampled = frame_to_s16_device(&frame_44k, 48_000, 2).unwrap();
        assert_eq!(out_resampled.len(), 480 * 2);
        assert!(out_resampled.iter().any(|&s| s != 0));
    }

    #[test]
    fn u8_same_rate_and_resampled() {
        let u8_vals = [0u8, 128, 255, 64, 192, 128];
        let expected: Vec<i16> = u8_vals.iter().map(|&b| (i16::from(b) - 128) << 8).collect();
        let frame = u8_stereo_frame(48_000, &u8_vals);

        let out_direct = frame_to_s16_device(&frame, 48_000, 2).unwrap();
        assert_eq!(out_direct, expected);

        // Resampled path (44.1 kHz -> 48 kHz)
        let nb = 441;
        let mut u8_44k = Vec::with_capacity(nb * 2);
        for i in 0..nb {
            let v = if (i / 4) % 2 == 0 { 220 } else { 36 };
            u8_44k.push(v);
            u8_44k.push(255 - v);
        }
        let frame_44k = u8_stereo_frame(44_100, &u8_44k);
        let out_resampled = frame_to_s16_device(&frame_44k, 48_000, 2).unwrap();
        assert_eq!(out_resampled.len(), 480 * 2);
        assert!(out_resampled.iter().any(|&s| s != 0));
    }

    #[test]
    fn truncated_s32_and_u8_planes_return_err() {
        let mut s32 = Frame::new_audio(SampleFormat::S32, 48_000, 2, 4);
        s32.data[0].truncate(10); // needs 32
        assert!(frame_to_s16_device(&s32, 48_000, 2).is_err());

        let mut u8f = Frame::new_audio(SampleFormat::U8, 48_000, 2, 4);
        u8f.data[0].truncate(5); // needs 8
        assert!(frame_to_s16_device(&u8f, 48_000, 2).is_err());
    }
}
