//! Built-in demuxer implementations for common container formats.
//!
//! Each demuxer parses the container header and discovers streams
//! (codec type, sample rate, channels, dimensions, etc.).

mod avi_demuxer;
mod flac_demuxer;
mod mkv_demuxer;
mod mp4_demuxer;
mod raw_demuxer;
mod wav_demuxer;

pub use avi_demuxer::AVIDemuxer;
pub use flac_demuxer::FLACDemuxer;
pub use mkv_demuxer::MKVDemuxer;
pub use mp4_demuxer::MP4Demuxer;
pub(crate) use raw_demuxer::y4m_frame_bytes;
pub use raw_demuxer::{RawVideoDemuxer, Y4mDemuxer};
pub use wav_demuxer::WAVDemuxer;

/// Canonical PCM packet normalisation shared by the WAV, AVI and Matroska
/// demuxers.
///
/// `PCMAudioDecoder` (and every downstream consumer) expects interleaved
/// little-endian samples whose width matches the stream's [`SampleFormat`]:
/// `U8` = 1 byte, `S16` = 2, `S32`/`F32` = 4, `F64` = 8. Containers also carry
/// packed 24-bit, big-endian and G.711 companded audio, which must be
/// rewritten into one of those layouts before a packet leaves the demuxer —
/// otherwise e.g. 3-byte `s24le` data labelled `S32` makes the decoder count
/// samples wrongly and play noise.
///
/// [`SampleFormat`]: rsmpeg_util::SampleFormat
pub(crate) mod pcm {
    use rsmpeg_util::SampleFormat;

    /// How to turn container PCM bytes into canonical little-endian samples.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub(crate) enum PcmLayout {
        /// Already canonical (`u8`, `s16le`, `s32le`, `f32le`, `f64le`).
        #[default]
        Passthrough,
        /// Packed 24-bit LE → 32-bit LE, value `<< 8` (`[b0,b1,b2]` →
        /// `[0,b0,b1,b2]`), matching `FlacDecoder`'s 24-bit `S32` output.
        S24LeToS32,
        /// 16-bit big-endian → 16-bit little-endian.
        S16BeToLe,
        /// Packed 24-bit BE → 32-bit LE, value `<< 8`.
        S24BeToS32,
        /// 32-bit big-endian → 32-bit little-endian.
        S32BeToLe,
        /// ITU-T G.711 A-law (8-bit) → `s16le`.
        ALaw,
        /// ITU-T G.711 µ-law (8-bit) → `s16le`.
        MuLaw,
    }

    impl PcmLayout {
        /// Bytes per sample in the container.
        pub(crate) fn src_bytes(self) -> usize {
            match self {
                PcmLayout::Passthrough => 1,
                PcmLayout::S24LeToS32 | PcmLayout::S24BeToS32 => 3,
                PcmLayout::S16BeToLe => 2,
                PcmLayout::S32BeToLe => 4,
                PcmLayout::ALaw | PcmLayout::MuLaw => 1,
            }
        }

        /// Rewrite `src` into canonical bytes. A trailing partial sample is
        /// dropped. `Passthrough` returns the input unchanged (no copy).
        pub(crate) fn apply(self, src: Vec<u8>) -> Vec<u8> {
            match self {
                PcmLayout::Passthrough => src,
                _ => self.convert(&src),
            }
        }

        /// [`apply`](Self::apply) for reference-counted payloads.
        pub(crate) fn apply_bytes(self, src: bytes::Bytes) -> bytes::Bytes {
            match self {
                PcmLayout::Passthrough => src,
                _ => bytes::Bytes::from(self.convert(&src)),
            }
        }

        fn convert(self, src: &[u8]) -> Vec<u8> {
            let n = src.len() / self.src_bytes();
            match self {
                PcmLayout::Passthrough => src.to_vec(),
                PcmLayout::S24LeToS32 => {
                    let mut out = Vec::with_capacity(n * 4);
                    for s in src.chunks_exact(3) {
                        out.extend_from_slice(&[0, s[0], s[1], s[2]]);
                    }
                    out
                }
                PcmLayout::S24BeToS32 => {
                    let mut out = Vec::with_capacity(n * 4);
                    for s in src.chunks_exact(3) {
                        out.extend_from_slice(&[0, s[2], s[1], s[0]]);
                    }
                    out
                }
                PcmLayout::S16BeToLe => {
                    let mut out = Vec::with_capacity(n * 2);
                    for s in src.chunks_exact(2) {
                        out.extend_from_slice(&[s[1], s[0]]);
                    }
                    out
                }
                PcmLayout::S32BeToLe => {
                    let mut out = Vec::with_capacity(n * 4);
                    for s in src.chunks_exact(4) {
                        out.extend_from_slice(&[s[3], s[2], s[1], s[0]]);
                    }
                    out
                }
                PcmLayout::ALaw => {
                    let mut out = Vec::with_capacity(n * 2);
                    for &b in src {
                        out.extend_from_slice(&ALAW_TO_S16[usize::from(b)].to_le_bytes());
                    }
                    out
                }
                PcmLayout::MuLaw => {
                    let mut out = Vec::with_capacity(n * 2);
                    for &b in src {
                        out.extend_from_slice(&MULAW_TO_S16[usize::from(b)].to_le_bytes());
                    }
                    out
                }
            }
        }
    }

    /// ITU-T G.711 A-law expansion of one code word to linear 16-bit.
    pub(crate) const fn alaw_to_linear(a: u8) -> i16 {
        let a = a ^ 0x55;
        let mut t = ((a & 0x0F) as i32) << 4;
        let seg = ((a & 0x70) >> 4) as i32;
        match seg {
            0 => t += 8,
            1 => t += 0x108,
            _ => {
                t += 0x108;
                t <<= seg - 1;
            }
        }
        if a & 0x80 != 0 {
            t as i16
        } else {
            -t as i16
        }
    }

    /// ITU-T G.711 µ-law expansion of one code word to linear 16-bit.
    pub(crate) const fn mulaw_to_linear(u: u8) -> i16 {
        const BIAS: i32 = 0x84;
        let u = !u;
        let mut t = (((u & 0x0F) as i32) << 3) + BIAS;
        t <<= ((u & 0x70) >> 4) as i32;
        if u & 0x80 != 0 {
            (BIAS - t) as i16
        } else {
            (t - BIAS) as i16
        }
    }

    const fn build_table(alaw: bool) -> [i16; 256] {
        let mut table = [0i16; 256];
        let mut i = 0;
        while i < 256 {
            table[i] = if alaw {
                alaw_to_linear(i as u8)
            } else {
                mulaw_to_linear(i as u8)
            };
            i += 1;
        }
        table
    }

    static ALAW_TO_S16: [i16; 256] = build_table(true);
    static MULAW_TO_S16: [i16; 256] = build_table(false);

    /// Resolve a `WAVEFORMATEX.wFormatTag` / bit depth into the canonical
    /// sample format plus the conversion needed to reach it.
    ///
    /// Returns `None` for tags/bit depths that are not plain PCM.
    pub(crate) fn wave_pcm_layout(tag: u16, bits: u16) -> Option<(SampleFormat, PcmLayout)> {
        match (tag, bits) {
            // Integer PCM: the container width is the bit depth rounded up to
            // whole bytes (e.g. 20-bit audio is stored in 3 bytes).
            (0x0001, 1..=8) => Some((SampleFormat::U8, PcmLayout::Passthrough)),
            (0x0001, 9..=16) => Some((SampleFormat::S16, PcmLayout::Passthrough)),
            (0x0001, 17..=24) => Some((SampleFormat::S32, PcmLayout::S24LeToS32)),
            (0x0001, 25..=32) => Some((SampleFormat::S32, PcmLayout::Passthrough)),
            (0x0003, 32) => Some((SampleFormat::F32, PcmLayout::Passthrough)),
            (0x0003, 64) => Some((SampleFormat::F64, PcmLayout::Passthrough)),
            (0x0006, 8) => Some((SampleFormat::S16, PcmLayout::ALaw)),
            (0x0007, 8) => Some((SampleFormat::S16, PcmLayout::MuLaw)),
            _ => None,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn g711_reference_values() {
            // A-law: 0xD5 / 0x55 are the smallest positive / negative steps.
            assert_eq!(alaw_to_linear(0xD5), 8);
            assert_eq!(alaw_to_linear(0x55), -8);
            assert_eq!(alaw_to_linear(0xAA), 32_256);
            assert_eq!(alaw_to_linear(0x2A), -32_256);
            // µ-law: 0xFF / 0x7F are both zero, 0x80 / 0x00 the extremes.
            assert_eq!(mulaw_to_linear(0xFF), 0);
            assert_eq!(mulaw_to_linear(0x7F), 0);
            assert_eq!(mulaw_to_linear(0x80), 32_124);
            assert_eq!(mulaw_to_linear(0x00), -32_124);
            // Tables agree with the functions and are odd-symmetric.
            for i in 0..=255u8 {
                assert_eq!(ALAW_TO_S16[usize::from(i)], alaw_to_linear(i));
                assert_eq!(MULAW_TO_S16[usize::from(i)], mulaw_to_linear(i));
                assert_eq!(alaw_to_linear(i), -alaw_to_linear(i ^ 0x80));
                assert_eq!(mulaw_to_linear(i), -mulaw_to_linear(i ^ 0x80));
            }
            // µ-law is monotonic within the positive half (0xFF→0x80).
            for i in 0x81..=0xFFu8 {
                assert!(mulaw_to_linear(i) < mulaw_to_linear(i - 1));
            }
        }

        #[test]
        fn layouts_rewrite_bytes_and_drop_partial_samples() {
            assert_eq!(
                PcmLayout::S24LeToS32.apply(vec![1, 2, 3, 4, 5, 6, 7]),
                vec![0, 1, 2, 3, 0, 4, 5, 6]
            );
            assert_eq!(
                PcmLayout::S24BeToS32.apply(vec![0x12, 0x34, 0x56]),
                vec![0, 0x56, 0x34, 0x12]
            );
            assert_eq!(
                PcmLayout::S16BeToLe.apply(vec![0x12, 0x34, 0xAB]),
                vec![0x34, 0x12]
            );
            assert_eq!(
                PcmLayout::S32BeToLe.apply(vec![1, 2, 3, 4]),
                vec![4, 3, 2, 1]
            );
            assert_eq!(
                PcmLayout::MuLaw.apply(vec![0xFF, 0x80]),
                [0i16.to_le_bytes(), 32_124i16.to_le_bytes()].concat()
            );
            assert_eq!(
                PcmLayout::ALaw.apply_bytes(bytes::Bytes::from_static(&[0xD5])),
                bytes::Bytes::from(8i16.to_le_bytes().to_vec())
            );
            let raw = vec![9u8, 8, 7];
            assert_eq!(PcmLayout::Passthrough.apply(raw.clone()), raw);
            // -1 as s24le widened is -256 as s32le (value << 8).
            let w = PcmLayout::S24LeToS32.apply(vec![0xFF, 0xFF, 0xFF]);
            assert_eq!(i32::from_le_bytes([w[0], w[1], w[2], w[3]]), -256);
        }

        #[test]
        fn wave_tags_map_to_canonical_formats() {
            use PcmLayout::{ALaw, MuLaw, Passthrough, S24LeToS32};
            use SampleFormat::{F32, F64, S16, S32, U8};
            assert_eq!(wave_pcm_layout(1, 8), Some((U8, Passthrough)));
            assert_eq!(wave_pcm_layout(1, 16), Some((S16, Passthrough)));
            assert_eq!(wave_pcm_layout(1, 20), Some((S32, S24LeToS32)));
            assert_eq!(wave_pcm_layout(1, 24), Some((S32, S24LeToS32)));
            assert_eq!(wave_pcm_layout(1, 32), Some((S32, Passthrough)));
            assert_eq!(wave_pcm_layout(3, 32), Some((F32, Passthrough)));
            assert_eq!(wave_pcm_layout(3, 64), Some((F64, Passthrough)));
            assert_eq!(wave_pcm_layout(6, 8), Some((S16, ALaw)));
            assert_eq!(wave_pcm_layout(7, 8), Some((S16, MuLaw)));
            assert_eq!(wave_pcm_layout(1, 0), None);
            assert_eq!(wave_pcm_layout(1, 48), None);
            assert_eq!(wave_pcm_layout(3, 16), None);
            assert_eq!(wave_pcm_layout(0x55, 0), None);
        }
    }
}
