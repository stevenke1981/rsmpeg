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

    // Resample / reformat path (stub-quality conversion inside Resampler today).
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

fn is_device_ready_s16(frame: &Frame, target_rate: u32, target_channels: u16) -> bool {
    frame.sample_format == SampleFormat::S16
        && !frame.sample_format.is_planar()
        && frame.sample_rate == target_rate
        && frame.channels == target_channels
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
}
