//! Symphonia packet-in audio decoder backend.
//!
//! Implements [`rsmpeg_codec::Decoder`] by feeding elementary-stream packets
//! into a Symphonia codec decoder.  There is **no** FormatReader / file demux —
//! callers already demux via rsmpeg-format (or equivalent) and pass packets.

use std::collections::VecDeque;

use rsmpeg_codec::{CodecId, CodecParameters, DecodeStatus, Decoder, Frame, Packet, PictureType};
use rsmpeg_util::{PixelFormat, Rational, RsError, RsResult, SampleFormat};
use symphonia::core::audio::{AudioBufferRef, Channels, SampleBuffer, SignalSpec};
use symphonia::core::codecs::{
    DecoderOptions, CODEC_TYPE_AAC, CODEC_TYPE_MP3, CODEC_TYPE_PCM_F32LE, CODEC_TYPE_PCM_S16LE,
    CODEC_TYPE_PCM_S32LE, CODEC_TYPE_PCM_U8,
};
use symphonia::core::formats::Packet as SymPacket;
use symphonia::core::units::TimeBase;

/// Packet-in Symphonia audio decoder implementing the rsmpeg send/receive model.
pub struct SymphoniaAudioDecoder {
    codec_id: CodecId,
    params: CodecParameters,
    decoder: Box<dyn symphonia::core::codecs::Decoder>,
    pending: VecDeque<Frame>,
    eof: bool,
    sample_rate: u32,
    channels: u16,
}

impl SymphoniaAudioDecoder {
    /// Construct from rsmpeg [`CodecParameters`].
    ///
    /// Supported: [`CodecId::Aac`], [`CodecId::Pcm`], [`CodecId::Mp3`].
    ///
    /// For AAC, `params.extradata` may be a raw AudioSpecificConfig or an MPEG-4
    /// `esds` payload — ASC is extracted automatically when present.
    pub fn try_new(params: &CodecParameters) -> RsResult<Self> {
        if !Self::supported(params.codec_id) {
            return Err(RsError::Unsupported(std::borrow::Cow::Owned(format!(
                "symphonia backend does not support codec '{}'",
                params.codec_id.name()
            ))));
        }

        let sample_rate = params.sample_rate.unwrap_or(44_100);
        let channels = params.channels.unwrap_or(2);

        let mut sym_params = symphonia::core::codecs::CodecParameters::new();
        match params.codec_id {
            CodecId::Aac => {
                sym_params.for_codec(CODEC_TYPE_AAC);
                if let Some(ref extra) = params.extradata {
                    let asc = extract_aac_asc(extra).unwrap_or_else(|| extra.clone());
                    sym_params.with_extra_data(asc.into_boxed_slice());
                }
            }
            CodecId::Mp3 => {
                sym_params.for_codec(CODEC_TYPE_MP3);
            }
            CodecId::Pcm => {
                let fmt = params.sample_format.unwrap_or(SampleFormat::S16);
                let (codec_type, bits) = match fmt {
                    SampleFormat::U8 => (CODEC_TYPE_PCM_U8, 8u32),
                    SampleFormat::S32 | SampleFormat::S32P => (CODEC_TYPE_PCM_S32LE, 32),
                    SampleFormat::F32 | SampleFormat::F32P => (CODEC_TYPE_PCM_F32LE, 32),
                    _ => (CODEC_TYPE_PCM_S16LE, 16),
                };
                sym_params.for_codec(codec_type);
                // Symphonia PCM requires max frames/packet and bits-per-sample.
                sym_params.with_max_frames_per_packet(65_536);
                sym_params.with_bits_per_sample(bits);
                sym_params.with_bits_per_coded_sample(bits);
            }
            other => {
                return Err(RsError::Unsupported(std::borrow::Cow::Owned(format!(
                    "symphonia backend does not support codec '{}'",
                    other.name()
                ))));
            }
        }

        sym_params.with_sample_rate(sample_rate);
        sym_params.with_channels(channels_mask(channels));
        // Default 1/sample_rate time base when not known from the container.
        sym_params.with_time_base(TimeBase::new(1, sample_rate.max(1)));

        let decoder = symphonia::default::get_codecs()
            .make(&sym_params, &DecoderOptions::default())
            .map_err(|e| {
                RsError::Codec(std::borrow::Cow::Owned(format!(
                    "failed to open Symphonia decoder for '{}': {e}",
                    params.codec_id.name()
                )))
            })?;

        let mut stored = params.clone();
        if stored.sample_rate.is_none() {
            stored.sample_rate = Some(sample_rate);
        }
        if stored.channels.is_none() {
            stored.channels = Some(channels);
        }
        if stored.sample_format.is_none() {
            stored.sample_format = Some(SampleFormat::S16);
        }

        Ok(Self {
            codec_id: params.codec_id,
            params: stored,
            decoder,
            pending: VecDeque::new(),
            eof: false,
            sample_rate,
            channels,
        })
    }

    /// Whether this backend can decode the given codec id.
    pub fn supported(codec_id: CodecId) -> bool {
        matches!(codec_id, CodecId::Aac | CodecId::Pcm | CodecId::Mp3)
    }

    fn packet_to_frame(&mut self, packet: &Packet) -> RsResult<Option<Frame>> {
        let ts = packet.pts.or(packet.dts).unwrap_or(0).max(0) as u64;
        let dur = packet.duration.max(0) as u64;
        let sym = SymPacket::new_from_slice(packet.stream_index as u32, ts, dur, &packet.data);

        match self.decoder.decode(&sym) {
            Ok(audio) => Ok(Some(audio_buffer_to_frame(
                audio,
                self.sample_rate,
                self.channels,
                packet,
            ))),
            // Soft errors: skip undecodeable packet (same as native_pipeline).
            Err(symphonia::core::errors::Error::DecodeError(_))
            | Err(symphonia::core::errors::Error::IoError(_)) => Ok(None),
            Err(symphonia::core::errors::Error::ResetRequired) => {
                self.decoder.reset();
                Ok(None)
            }
            Err(e) => Err(RsError::Codec(std::borrow::Cow::Owned(format!(
                "symphonia decode error: {e}"
            )))),
        }
    }
}

impl Decoder for SymphoniaAudioDecoder {
    fn codec_id(&self) -> CodecId {
        self.codec_id
    }

    fn send_packet(&mut self, packet: Option<&Packet>) -> RsResult<()> {
        match packet {
            Some(pkt) => {
                if self.eof {
                    return Err(RsError::Codec(std::borrow::Cow::Borrowed(
                        "cannot send packet after end-of-stream; call reset() first",
                    )));
                }
                if let Some(frame) = self.packet_to_frame(pkt)? {
                    // Prefer live channel/rate from decoded output.
                    self.sample_rate = frame.sample_rate;
                    self.channels = frame.channels;
                    self.params.sample_rate = Some(frame.sample_rate);
                    self.params.channels = Some(frame.channels);
                    self.params.sample_format = Some(frame.sample_format);
                    self.pending.push_back(frame);
                }
                Ok(())
            }
            None => {
                self.eof = true;
                Ok(())
            }
        }
    }

    fn receive_frame(&mut self) -> RsResult<DecodeStatus> {
        if let Some(frame) = self.pending.pop_front() {
            Ok(DecodeStatus::Frame(frame))
        } else if self.eof {
            Ok(DecodeStatus::EndOfStream)
        } else {
            Ok(DecodeStatus::NeedMoreInput)
        }
    }

    fn reset(&mut self) -> RsResult<()> {
        self.pending.clear();
        self.eof = false;
        self.decoder.reset();
        Ok(())
    }

    fn get_parameters(&self) -> CodecParameters {
        self.params.clone()
    }
}

fn channels_mask(channels: u16) -> Channels {
    match channels {
        0 | 1 => Channels::FRONT_LEFT,
        2 => Channels::FRONT_LEFT | Channels::FRONT_RIGHT,
        3 => Channels::FRONT_LEFT | Channels::FRONT_RIGHT | Channels::FRONT_CENTRE,
        4 => {
            Channels::FRONT_LEFT
                | Channels::FRONT_RIGHT
                | Channels::REAR_LEFT
                | Channels::REAR_RIGHT
        }
        5 => {
            Channels::FRONT_LEFT
                | Channels::FRONT_RIGHT
                | Channels::FRONT_CENTRE
                | Channels::REAR_LEFT
                | Channels::REAR_RIGHT
        }
        _ => {
            // 5.1 and above: stereo + centre + LFE + rears as a reasonable default.
            Channels::FRONT_LEFT
                | Channels::FRONT_RIGHT
                | Channels::FRONT_CENTRE
                | Channels::LFE1
                | Channels::REAR_LEFT
                | Channels::REAR_RIGHT
        }
    }
}

/// Convert a Symphonia audio buffer into an interleaved S16 [`Frame`].
fn audio_buffer_to_frame(
    audio: AudioBufferRef<'_>,
    fallback_rate: u32,
    fallback_ch: u16,
    packet: &Packet,
) -> Frame {
    let spec: SignalSpec = *audio.spec();
    let sample_rate = if spec.rate > 0 {
        spec.rate
    } else {
        fallback_rate
    };
    let channels = {
        let n = spec.channels.count() as u16;
        if n > 0 {
            n
        } else {
            fallback_ch
        }
    };

    let n_frames = audio.frames();
    let mut sb = SampleBuffer::<i16>::new(audio.capacity() as u64, spec);
    sb.copy_interleaved_ref(audio);
    let samples_i16 = sb.samples();

    // Interleaved S16 LE bytes.
    let mut data = Vec::with_capacity(samples_i16.len() * 2);
    for s in samples_i16 {
        data.extend_from_slice(&s.to_le_bytes());
    }

    let duration = if packet.duration > 0 {
        packet.duration
    } else if sample_rate > 0 {
        n_frames as i64
    } else {
        0
    };

    let time_base = if packet.time_base.den != 0 {
        packet.time_base
    } else {
        Rational::new(1, sample_rate.max(1) as i32)
    };

    Frame {
        data: vec![data.clone()],
        linesize: vec![data.len()],
        width: 0,
        height: 0,
        pixel_format: PixelFormat::None,
        sample_format: SampleFormat::S16,
        sample_rate,
        channels,
        samples: n_frames,
        pts: packet.pts.or(packet.dts),
        duration,
        time_base,
        key_frame: true,
        pict_type: PictureType::I,
    }
}

/// Pull AudioSpecificConfig from an MPEG-4 `esds` box payload (or return raw).
///
/// Mirrors the helper used by the native pipeline so AAC packet-in works with
/// either raw ASC or full esds extradata.
fn extract_aac_asc(esds: &[u8]) -> Option<Vec<u8>> {
    let mut i = 0usize;
    while i < esds.len() {
        let tag = esds[i];
        i += 1;
        if i >= esds.len() {
            return None;
        }
        // Expandable MPEG-4 length
        let mut len = 0usize;
        for _ in 0..4 {
            if i >= esds.len() {
                return None;
            }
            let b = esds[i];
            i += 1;
            len = (len << 7) | (b & 0x7f) as usize;
            if b & 0x80 == 0 {
                break;
            }
        }
        if tag == 0x05 {
            if i + len <= esds.len() && len > 0 {
                return Some(esds[i..i + len].to_vec());
            }
            return None;
        }
        // Nested descriptors (ES / DecoderConfig) — continue scanning inside.
        if tag == 0x03 || tag == 0x04 {
            continue;
        }
        i = i.saturating_add(len);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm_params() -> CodecParameters {
        let mut p = CodecParameters::new(CodecId::Pcm);
        p.sample_rate = Some(44_100);
        p.channels = Some(2);
        p.sample_format = Some(SampleFormat::S16);
        p
    }

    #[test]
    fn need_more_input_without_packets() {
        let mut dec = SymphoniaAudioDecoder::try_new(&pcm_params()).expect("construct pcm");
        match dec.receive_frame().expect("receive") {
            DecodeStatus::NeedMoreInput => {}
            other => panic!("expected NeedMoreInput, got {other:?}"),
        }
    }

    #[test]
    fn construct_pcm_decoder() {
        let dec = SymphoniaAudioDecoder::try_new(&pcm_params()).expect("pcm decoder");
        assert_eq!(dec.codec_id(), CodecId::Pcm);
        let params = dec.get_parameters();
        assert_eq!(params.sample_rate, Some(44_100));
        assert_eq!(params.channels, Some(2));
    }

    #[test]
    fn pcm_packet_roundtrip() {
        let mut dec = SymphoniaAudioDecoder::try_new(&pcm_params()).expect("pcm");
        // 4 stereo S16 samples = 16 bytes
        let raw: Vec<u8> = (0i16..8).flat_map(|v| v.to_le_bytes()).collect();
        let mut pkt = Packet::new(raw.into(), 0);
        pkt.pts = Some(100);
        pkt.duration = 4;
        pkt.time_base = Rational::new(1, 44_100);

        dec.send_packet(Some(&pkt)).expect("send");
        match dec.receive_frame().expect("recv") {
            DecodeStatus::Frame(f) => {
                assert_eq!(f.sample_format, SampleFormat::S16);
                assert_eq!(f.sample_rate, 44_100);
                assert_eq!(f.channels, 2);
                assert_eq!(f.samples, 4);
                assert_eq!(f.pts, Some(100));
                assert!(!f.data[0].is_empty());
            }
            other => panic!("expected Frame, got {other:?}"),
        }
        assert!(matches!(
            dec.receive_frame().unwrap(),
            DecodeStatus::NeedMoreInput
        ));

        dec.send_packet(None).unwrap();
        assert!(matches!(
            dec.receive_frame().unwrap(),
            DecodeStatus::EndOfStream
        ));
    }

    #[test]
    fn reset_clears_eof() {
        let mut dec = SymphoniaAudioDecoder::try_new(&pcm_params()).expect("pcm");
        dec.send_packet(None).unwrap();
        assert!(matches!(
            dec.receive_frame().unwrap(),
            DecodeStatus::EndOfStream
        ));
        dec.reset().unwrap();
        assert!(matches!(
            dec.receive_frame().unwrap(),
            DecodeStatus::NeedMoreInput
        ));
    }

    #[test]
    fn unsupported_codec_rejected() {
        let p = CodecParameters::new(CodecId::H264);
        assert!(SymphoniaAudioDecoder::try_new(&p).is_err());
    }

    #[test]
    fn extract_asc_from_minimal_esds_like() {
        let data = [0x05u8, 0x02, 0x11, 0x90];
        let asc = extract_aac_asc(&data).expect("asc");
        assert_eq!(asc, vec![0x11, 0x90]);
    }

    #[test]
    fn construct_aac_with_raw_asc() {
        // AAC-LC, 48 kHz, stereo ASC: 0x11 0x90
        let mut p = CodecParameters::new(CodecId::Aac);
        p.sample_rate = Some(48_000);
        p.channels = Some(2);
        p.extradata = Some(vec![0x11, 0x90]);
        let dec = SymphoniaAudioDecoder::try_new(&p).expect("aac decoder");
        assert_eq!(dec.codec_id(), CodecId::Aac);
    }

    #[test]
    fn construct_mp3() {
        let mut p = CodecParameters::new(CodecId::Mp3);
        p.sample_rate = Some(44_100);
        p.channels = Some(2);
        let dec = SymphoniaAudioDecoder::try_new(&p).expect("mp3 decoder");
        assert_eq!(dec.codec_id(), CodecId::Mp3);
    }
}
