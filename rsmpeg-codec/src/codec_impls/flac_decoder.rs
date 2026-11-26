//! Pure-Rust RFC 9639 FLAC lossless audio frame decoder.

use std::collections::VecDeque;

use crate::codec::{Codec, CodecCapabilities, DecodeStatus, Decoder, Encoder};
use crate::codec_id::CodecId;
use crate::codec_parameters::CodecParameters;
use crate::frame::Frame;
use crate::packet::Packet;
use crate::picture_type::PictureType;
use rsmpeg_util::{MediaType, PixelFormat, Rational, RsError, RsResult, SampleFormat};

const CRC8_TABLE: [u8; 256] = {
    let mut table = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u8;
        let mut j = 0;
        while j < 8 {
            if crc & 0x80 != 0 {
                crc = (crc << 1) ^ 0x07;
            } else {
                crc <<= 1;
            }
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

const CRC16_TABLE: [u16; 256] = {
    let mut table = [0u16; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = (i as u16) << 8;
        let mut j = 0;
        while j < 8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ 0x8005;
            } else {
                crc <<= 1;
            }
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

fn crc8(data: &[u8]) -> u8 {
    let mut crc: u8 = 0;
    for &b in data {
        crc = CRC8_TABLE[(crc ^ b) as usize];
    }
    crc
}

fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &b in data {
        crc = (crc << 8) ^ CRC16_TABLE[(((crc >> 8) as u8) ^ b) as usize];
    }
    crc
}

/// MSB-first bitstream reader backed by a 64-bit bit reservoir.
///
/// `bit_buf` holds the next `bit_count` unread bits left-aligned (bit 63 is
/// the next bit); every bit below that window is kept zero so `read_unary`
/// can use `leading_zeros()` directly. Only whole bytes are ever loaded into
/// the reservoir, so the consumed-bit position — and with it
/// [`byte_position`](Self::byte_position), which the CRC-8 / CRC-16 checks
/// depend on — stays exact.
struct BitReader<'a> {
    data: &'a [u8],
    /// Index of the next byte to load into the reservoir.
    raw_pos: usize,
    bit_buf: u64,
    bit_count: u8,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            raw_pos: 0,
            bit_buf: 0,
            bit_count: 0,
        }
    }

    /// Byte offset of the next unread bit (exact when byte-aligned, which is
    /// the only place the decoder asks for it; otherwise the byte containing
    /// the next bit).
    fn byte_position(&self) -> usize {
        (self.raw_pos * 8 - usize::from(self.bit_count)) / 8
    }

    /// Discard the bits remaining in the current partially-consumed byte.
    fn align_to_byte(&mut self) {
        let drop = self.bit_count % 8;
        if drop != 0 {
            self.bit_buf <<= drop;
            self.bit_count -= drop;
        }
    }

    /// Top up the reservoir with as many whole bytes as fit.
    #[inline]
    fn refill(&mut self) {
        let room_bytes = usize::from((64 - self.bit_count) / 8);
        if room_bytes == 0 {
            return;
        }
        if let Some(chunk) = self.data.get(self.raw_pos..self.raw_pos + 8) {
            let mut word = [0u8; 8];
            word.copy_from_slice(chunk);
            let new_count = self.bit_count + (room_bytes as u8) * 8;
            // `bit_count <= 56` here because `room_bytes >= 1`.
            let mut add = u64::from_be_bytes(word) >> self.bit_count;
            if new_count < 64 {
                add &= !(u64::MAX >> new_count);
            }
            self.bit_buf |= add;
            self.bit_count = new_count;
            self.raw_pos += room_bytes;
        } else {
            while self.bit_count <= 56 && self.raw_pos < self.data.len() {
                self.bit_buf |= u64::from(self.data[self.raw_pos]) << (56 - self.bit_count);
                self.bit_count += 8;
                self.raw_pos += 1;
            }
        }
    }

    #[inline]
    fn consume(&mut self, n: u8) {
        self.bit_buf = self.bit_buf.checked_shl(u32::from(n)).unwrap_or(0);
        self.bit_count -= n;
    }

    fn eof_err() -> RsError {
        RsError::InvalidData("FLAC: unexpected end of bitstream".into())
    }

    fn read_bit(&mut self) -> RsResult<u8> {
        Ok(self.read_bits(1)? as u8)
    }

    fn read_bits(&mut self, n: u8) -> RsResult<u64> {
        if n == 0 {
            return Ok(0);
        }
        if n > 64 {
            return Err(RsError::InvalidData(
                "FLAC: cannot read more than 64 bits at once".into(),
            ));
        }
        if n > 56 {
            // A byte-granular refill only guarantees 57 available bits.
            let hi = self.read_bits(n - 32)?;
            let lo = self.read_bits(32)?;
            return Ok((hi << 32) | lo);
        }
        if self.bit_count < n {
            self.refill();
            if self.bit_count < n {
                return Err(Self::eof_err());
            }
        }
        let val = self.bit_buf >> (64 - u32::from(n));
        self.consume(n);
        Ok(val)
    }

    fn read_signed_bits(&mut self, n: u8) -> RsResult<i64> {
        if n == 0 {
            return Ok(0);
        }
        let raw = self.read_bits(n)?;
        if n == 64 {
            Ok(raw as i64)
        } else {
            let shift = 64 - n;
            Ok(((raw << shift) as i64) >> shift)
        }
    }

    /// Count zero bits up to (and consume) the terminating one bit.
    fn read_unary(&mut self) -> RsResult<u64> {
        let mut count: u64 = 0;
        loop {
            if self.bit_count == 0 {
                self.refill();
                if self.bit_count == 0 {
                    return Err(RsError::InvalidData(
                        "FLAC: unexpected end of bitstream in unary code".into(),
                    ));
                }
            }
            if self.bit_buf != 0 {
                // Bits below the valid window are zero, so the first set bit
                // is always inside it.
                let lz = self.bit_buf.leading_zeros() as u8;
                count = count
                    .checked_add(u64::from(lz))
                    .ok_or_else(|| RsError::InvalidData("FLAC: unary quotient overflow".into()))?;
                self.consume(lz + 1);
                return Ok(count);
            }
            count = count
                .checked_add(u64::from(self.bit_count))
                .ok_or_else(|| RsError::InvalidData("FLAC: unary quotient overflow".into()))?;
            self.bit_buf = 0;
            self.bit_count = 0;
        }
    }

    fn read_utf8_u64(&mut self) -> RsResult<u64> {
        let b0 = self.read_bits(8)? as u8;
        if b0 & 0x80 == 0 {
            return Ok(u64::from(b0));
        }
        let ones = (!b0).leading_zeros() as usize;
        if ones == 1 || ones > 7 {
            return Err(RsError::InvalidData(
                "FLAC: invalid UTF-8 coded frame/sample number prefix".into(),
            ));
        }
        let mut val = if ones < 7 {
            u64::from(b0 & ((1u8 << (7 - ones)) - 1))
        } else {
            0
        };
        for _ in 1..ones {
            let cb = self.read_bits(8)? as u8;
            if cb & 0xC0 != 0x80 {
                return Err(RsError::InvalidData(
                    "FLAC: invalid UTF-8 continuation byte".into(),
                ));
            }
            val = (val << 6) | u64::from(cb & 0x3F);
        }
        Ok(val)
    }
}

/// Parsed FLAC STREAMINFO metadata fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
struct StreamInfo {
    min_blocksize: u16,
    max_blocksize: u16,
    sample_rate: u32,
    channels: u16,
    bits_per_sample: u16,
    total_samples: u64,
}

fn parse_streaminfo_extradata(bytes: &[u8]) -> RsResult<Option<StreamInfo>> {
    if bytes.is_empty() {
        return Ok(None);
    }
    let info = if bytes.starts_with(b"fLaC") {
        if bytes.len() < 42 {
            return Err(RsError::InvalidData(
                "FLAC: fLaC extradata truncated before STREAMINFO".into(),
            ));
        }
        let block_type = bytes[4] & 0x7F;
        let block_size =
            (u32::from(bytes[5]) << 16) | (u32::from(bytes[6]) << 8) | u32::from(bytes[7]);
        if block_type != 0 || block_size < 34 {
            return Err(RsError::InvalidData(
                "FLAC: fLaC extradata missing valid STREAMINFO block".into(),
            ));
        }
        &bytes[8..42]
    } else if bytes.len() >= 38
        && (bytes[0] & 0x7F) == 0
        && bytes[1] == 0
        && bytes[2] == 0
        && bytes[3] == 34
    {
        &bytes[4..38]
    } else if bytes.len() >= 34 {
        &bytes[..34]
    } else {
        return Err(RsError::InvalidData(
            "FLAC: extradata shorter than 34-byte STREAMINFO".into(),
        ));
    };

    let min_blocksize = u16::from_be_bytes([info[0], info[1]]);
    let max_blocksize = u16::from_be_bytes([info[2], info[3]]);
    let sample_rate =
        (u32::from(info[10]) << 12) | (u32::from(info[11]) << 4) | (u32::from(info[12]) >> 4);
    let channels = u16::from((info[12] >> 1) & 0x07) + 1;
    let bps_raw = ((u32::from(info[12]) & 0x01) << 4) | ((u32::from(info[13]) >> 4) & 0x0F);
    let bits_per_sample = (bps_raw + 1) as u16;
    let total_samples = ((u64::from(info[13]) & 0x0F) << 32)
        | u64::from(u32::from_be_bytes([info[14], info[15], info[16], info[17]]));

    Ok(Some(StreamInfo {
        min_blocksize,
        max_blocksize,
        sample_rate,
        channels,
        bits_per_sample,
        total_samples,
    }))
}

/// Pure-Rust FLAC codec descriptor.
pub struct FlacCodec;

impl Codec for FlacCodec {
    fn id(&self) -> CodecId {
        CodecId::Flac
    }

    fn media_type(&self) -> MediaType {
        MediaType::Audio
    }

    fn name(&self) -> &'static str {
        "flac"
    }

    fn long_name(&self) -> &'static str {
        "Pure-Rust FLAC lossless audio decoder"
    }

    fn capabilities(&self) -> CodecCapabilities {
        CodecCapabilities {
            can_decode: true,
            can_encode: false,
            lossless: true,
            intra_only: true,
        }
    }

    fn create_decoder(&self) -> RsResult<Box<dyn Decoder>> {
        Ok(Box::new(FlacDecoder::new()))
    }

    fn create_encoder(&self) -> RsResult<Box<dyn Encoder>> {
        Err(RsError::Unsupported("FLAC encoding not implemented".into()))
    }
}

/// Pure-Rust RFC 9639 FLAC lossless audio frame decoder.
pub struct FlacDecoder {
    params: CodecParameters,
    streaminfo: Option<StreamInfo>,
    pending: VecDeque<Frame>,
    eof: bool,
    /// Per-channel subframe sample buffers, reused across frames so steady
    /// state decoding does not allocate per subframe.
    channel_buffers: Vec<Vec<i64>>,
}

impl FlacDecoder {
    pub fn new() -> Self {
        Self {
            params: CodecParameters::new(CodecId::Flac),
            streaminfo: None,
            pending: VecDeque::new(),
            eof: false,
            channel_buffers: Vec::new(),
        }
    }

    /// Create a [`FlacDecoder`] configured from [`CodecParameters`].
    ///
    /// Parses STREAMINFO from `params.extradata` when provided (either raw
    /// 34-byte STREAMINFO or `fLaC` + metadata header + 34-byte STREAMINFO).
    pub fn from_params(params: &CodecParameters) -> RsResult<Self> {
        if params.codec_id != CodecId::Flac && params.codec_id != CodecId::Unknown {
            return Err(RsError::Unsupported(
                format!("FlacDecoder does not support codec {:?}", params.codec_id).into(),
            ));
        }

        let mut dec = Self::new();
        dec.params = params.clone();
        dec.params.codec_id = CodecId::Flac;
        dec.params.media_type = MediaType::Audio;

        if let Some(ref extra) = params.extradata {
            if let Some(si) = parse_streaminfo_extradata(extra)? {
                dec.streaminfo = Some(si);
                if si.sample_rate > 0 {
                    dec.params.sample_rate = Some(si.sample_rate);
                }
                dec.params.channels = Some(si.channels);
                dec.params.sample_format = Some(if si.bits_per_sample <= 16 {
                    SampleFormat::S16
                } else {
                    SampleFormat::S32
                });
            }
        }

        Ok(dec)
    }

    fn decode_single_frame(
        &mut self,
        data: &[u8],
        pts: Option<i64>,
        pkt_time_base: Rational,
    ) -> RsResult<(Frame, usize)> {
        let mut reader = BitReader::new(data);

        let sync = reader.read_bits(14)?;
        if sync != 0x3FFE {
            return Err(RsError::InvalidData(
                "FLAC: missing frame sync 0x3FFE".into(),
            ));
        }
        let reserved0 = reader.read_bit()?;
        if reserved0 != 0 {
            return Err(RsError::InvalidData(
                "FLAC: non-zero sync reserved bit".into(),
            ));
        }
        let _blocking_strategy = reader.read_bit()?;

        let bs_code = reader.read_bits(4)? as u8;
        let sr_code = reader.read_bits(4)? as u8;
        let ch_code = reader.read_bits(4)? as u8;
        let bps_code = reader.read_bits(3)? as u8;
        let reserved1 = reader.read_bit()?;
        if reserved1 != 0 {
            return Err(RsError::InvalidData(
                "FLAC: non-zero frame header reserved bit".into(),
            ));
        }

        let _coded_number = reader.read_utf8_u64()?;

        let blocksize: usize = match bs_code {
            0 => {
                return Err(RsError::InvalidData(
                    "FLAC: reserved blocksize code 0".into(),
                ))
            }
            1 => 192,
            2..=5 => 576 << (bs_code - 2),
            6 => (reader.read_bits(8)? as usize) + 1,
            7 => (reader.read_bits(16)? as usize) + 1,
            8..=15 => 256 << (bs_code - 8),
            _ => unreachable!(),
        };

        let sample_rate: u32 = match sr_code {
            0 => self
                .streaminfo
                .map(|si| si.sample_rate)
                .or(self.params.sample_rate)
                .filter(|&sr| sr > 0)
                .ok_or_else(|| {
                    RsError::InvalidData("FLAC: sample rate code 0 without STREAMINFO".into())
                })?,
            1 => 88_200,
            2 => 176_400,
            3 => 192_000,
            4 => 8_000,
            5 => 16_000,
            6 => 22_050,
            7 => 24_000,
            8 => 32_000,
            9 => 44_100,
            10 => 48_000,
            11 => 96_000,
            12 => (reader.read_bits(8)? as u32) * 1000,
            13 => reader.read_bits(16)? as u32,
            14 => (reader.read_bits(16)? as u32) * 10,
            15 => {
                return Err(RsError::InvalidData(
                    "FLAC: forbidden sample rate code 15".into(),
                ))
            }
            _ => unreachable!(),
        };
        if sample_rate == 0 {
            return Err(RsError::InvalidData(
                "FLAC: sample rate cannot be zero".into(),
            ));
        }

        let channels: u16 = match ch_code {
            0..=7 => u16::from(ch_code) + 1,
            8..=10 => 2,
            _ => {
                return Err(RsError::InvalidData(
                    format!("FLAC: reserved channel assignment {ch_code}").into(),
                ))
            }
        };

        let bps: u16 = match bps_code {
            0 => self
                .streaminfo
                .map(|si| si.bits_per_sample)
                .filter(|&b| b > 0)
                .ok_or_else(|| {
                    RsError::InvalidData("FLAC: sample size code 0 without STREAMINFO".into())
                })?,
            1 => 8,
            2 => 12,
            3 => {
                return Err(RsError::InvalidData(
                    "FLAC: reserved sample size code 3".into(),
                ))
            }
            4 => 16,
            5 => 20,
            6 => 24,
            7 => 32,
            _ => unreachable!(),
        };
        if !(4..=32).contains(&bps) {
            return Err(RsError::InvalidData(
                format!("FLAC: invalid bits per sample {bps}").into(),
            ));
        }

        let hdr_end = reader.byte_position();
        let expected_crc8 = reader.read_bits(8)? as u8;
        let actual_crc8 = crc8(&data[..hdr_end]);
        if actual_crc8 != expected_crc8 {
            return Err(RsError::InvalidData(
                format!(
                    "FLAC: frame header CRC-8 mismatch (expected {expected_crc8:#04x}, got {actual_crc8:#04x})"
                )
                .into(),
            ));
        }

        // Borrow the reusable channel buffers for the duration of the frame and
        // always hand them back, even when decoding fails.
        let mut buffers = std::mem::take(&mut self.channel_buffers);
        let body = decode_frame_body(
            &mut reader,
            data,
            ch_code,
            usize::from(channels),
            bps,
            blocksize,
            &mut buffers,
        );
        self.channel_buffers = buffers;
        let (sample_format, pcm_bytes, consumed) = body?;

        self.params.sample_rate = Some(sample_rate);
        self.params.channels = Some(channels);
        self.params.sample_format = Some(sample_format);

        let time_base = if pkt_time_base.num > 0 && pkt_time_base.den > 0 {
            pkt_time_base
        } else {
            Rational::new(1, sample_rate.min(i32::MAX as u32) as i32)
        };

        let pcm_len = pcm_bytes.len();
        let frame = Frame {
            data: vec![pcm_bytes],
            linesize: vec![pcm_len],
            width: 0,
            height: 0,
            pixel_format: PixelFormat::None,
            sample_format,
            sample_rate,
            channels,
            samples: blocksize,
            pts,
            duration: blocksize as i64,
            time_base,
            key_frame: true,
            pict_type: PictureType::I,
        };

        Ok((frame, consumed))
    }
}

impl Default for FlacDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder for FlacDecoder {
    fn codec_id(&self) -> CodecId {
        CodecId::Flac
    }

    fn send_packet(&mut self, packet: Option<&Packet>) -> RsResult<()> {
        match packet {
            Some(pkt) => {
                if self.eof {
                    return Err(RsError::Codec(
                        "cannot send packet after end-of-stream; call reset() first".into(),
                    ));
                }
                let mut offset = 0usize;
                let mut sample_offset = 0usize;
                let mut decoded = Vec::new();
                while offset < pkt.data.len() {
                    let frame_pts = pkt.pts.map(|base| {
                        if sample_offset == 0 {
                            base
                        } else if pkt.time_base.num > 0 && pkt.time_base.den > 0 {
                            let sr = self.params.sample_rate.unwrap_or(1).max(1);
                            let delta = (sample_offset as i128 * i128::from(pkt.time_base.den))
                                / (i128::from(sr) * i128::from(pkt.time_base.num));
                            base.saturating_add(
                                delta.clamp(i64::MIN as i128, i64::MAX as i128) as i64
                            )
                        } else {
                            base.saturating_add(sample_offset as i64)
                        }
                    });

                    let (frame, consumed) =
                        self.decode_single_frame(&pkt.data[offset..], frame_pts, pkt.time_base)?;
                    if consumed == 0 {
                        return Err(RsError::Bug(
                            "FLAC decoder made zero forward progress".into(),
                        ));
                    }
                    sample_offset = sample_offset.saturating_add(frame.samples);
                    offset += consumed;
                    decoded.push(frame);
                }
                self.pending.extend(decoded);
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
        self.channel_buffers.clear();
        Ok(())
    }

    fn get_parameters(&self) -> CodecParameters {
        self.params.clone()
    }
}

/// Decode all subframes of one frame into `buffers`, undo inter-channel
/// decorrelation, verify the CRC-16 footer and pack interleaved PCM.
///
/// Returns `(sample_format, pcm_bytes, consumed_bytes)`.
fn decode_frame_body(
    reader: &mut BitReader<'_>,
    data: &[u8],
    ch_code: u8,
    channels: usize,
    bps: u16,
    blocksize: usize,
    buffers: &mut Vec<Vec<i64>>,
) -> RsResult<(SampleFormat, Vec<u8>, usize)> {
    if buffers.len() < channels {
        buffers.resize_with(channels, Vec::new);
    }
    let chans = &mut buffers[..channels];
    for (ch_idx, buf) in chans.iter_mut().enumerate() {
        let sub_bps: u8 = match (ch_code, ch_idx) {
            (8, 1) | (9, 0) | (10, 1) => (bps + 1) as u8,
            _ => bps as u8,
        };
        decode_subframe(reader, blocksize, sub_bps, buf)?;
    }

    decorrelate_channels(ch_code, blocksize, chans);

    reader.align_to_byte();
    let crc16_pos = reader.byte_position();
    let expected_crc16 = reader.read_bits(16)? as u16;
    let actual_crc16 = crc16(&data[..crc16_pos]);
    if actual_crc16 != expected_crc16 {
        return Err(RsError::InvalidData(
            format!(
                "FLAC: frame CRC-16 mismatch (expected {expected_crc16:#06x}, got {actual_crc16:#06x})"
            )
            .into(),
        ));
    }
    let consumed = reader.byte_position();

    let (sample_format, pcm_bytes) = pack_interleaved_pcm(bps, blocksize, chans);
    Ok((sample_format, pcm_bytes, consumed))
}

/// Decode one subframe into `out` (cleared first and reused across frames, so
/// steady-state decoding performs no per-frame heap allocation).
fn decode_subframe(
    reader: &mut BitReader<'_>,
    blocksize: usize,
    sub_bps: u8,
    out: &mut Vec<i64>,
) -> RsResult<()> {
    out.clear();
    let zero = reader.read_bit()?;
    if zero != 0 {
        return Err(RsError::InvalidData(
            "FLAC: non-zero subframe header padding bit".into(),
        ));
    }
    let sub_type = reader.read_bits(6)? as u8;
    let has_wasted = reader.read_bit()?;
    let wasted: u8 = if has_wasted == 1 {
        let k = 1u64
            .checked_add(reader.read_unary()?)
            .ok_or_else(|| RsError::InvalidData("FLAC: wasted bits overflow".into()))?;
        if k >= u64::from(sub_bps) {
            return Err(RsError::InvalidData(
                "FLAC: wasted bits exceed subframe sample bit depth".into(),
            ));
        }
        k as u8
    } else {
        0
    };

    let eff_bps = sub_bps - wasted;
    match sub_type {
        0 => {
            let val = reader.read_signed_bits(eff_bps)?;
            out.resize(blocksize, val);
        }
        1 => {
            out.reserve(blocksize);
            for _ in 0..blocksize {
                out.push(reader.read_signed_bits(eff_bps)?);
            }
        }
        8..=12 => {
            let order = usize::from(sub_type - 8);
            decode_fixed_subframe(reader, blocksize, eff_bps, order, out)?;
        }
        32..=63 => {
            let order = usize::from(sub_type - 31);
            decode_lpc_subframe(reader, blocksize, eff_bps, order, out)?;
        }
        other => {
            return Err(RsError::InvalidData(
                format!("FLAC: reserved subframe type {other}").into(),
            ))
        }
    }

    if wasted > 0 {
        for s in out.iter_mut() {
            *s = s.wrapping_shl(u32::from(wasted));
        }
    }

    Ok(())
}

/// FIXED predictor: warm-up samples, then residuals decoded straight into
/// `out` and reconstructed in place.
fn decode_fixed_subframe(
    reader: &mut BitReader<'_>,
    blocksize: usize,
    eff_bps: u8,
    order: usize,
    out: &mut Vec<i64>,
) -> RsResult<()> {
    if order > blocksize {
        return Err(RsError::InvalidData(
            "FLAC: FIXED predictor order exceeds blocksize".into(),
        ));
    }
    out.reserve(blocksize);
    for _ in 0..order {
        out.push(reader.read_signed_bits(eff_bps)?);
    }
    decode_residual(reader, blocksize, order, out)?;
    let s = out.as_mut_slice();
    match order {
        0 => {}
        1 => {
            for i in 1..s.len() {
                s[i] = s[i].wrapping_add(s[i - 1]);
            }
        }
        2 => {
            for i in 2..s.len() {
                let pred = 2i64.wrapping_mul(s[i - 1]).wrapping_sub(s[i - 2]);
                s[i] = s[i].wrapping_add(pred);
            }
        }
        3 => {
            for i in 3..s.len() {
                let pred = 3i64
                    .wrapping_mul(s[i - 1])
                    .wrapping_sub(3i64.wrapping_mul(s[i - 2]))
                    .wrapping_add(s[i - 3]);
                s[i] = s[i].wrapping_add(pred);
            }
        }
        4 => {
            for i in 4..s.len() {
                let pred = 4i64
                    .wrapping_mul(s[i - 1])
                    .wrapping_sub(6i64.wrapping_mul(s[i - 2]))
                    .wrapping_add(4i64.wrapping_mul(s[i - 3]))
                    .wrapping_sub(s[i - 4]);
                s[i] = s[i].wrapping_add(pred);
            }
        }
        _ => unreachable!(),
    }
    Ok(())
}

/// LPC predictor (orders 1..=32) reconstructed in place.
fn decode_lpc_subframe(
    reader: &mut BitReader<'_>,
    blocksize: usize,
    eff_bps: u8,
    order: usize,
    out: &mut Vec<i64>,
) -> RsResult<()> {
    if order > blocksize {
        return Err(RsError::InvalidData(
            "FLAC: LPC predictor order exceeds blocksize".into(),
        ));
    }
    out.reserve(blocksize);
    for _ in 0..order {
        out.push(reader.read_signed_bits(eff_bps)?);
    }
    let prec_minus_1 = reader.read_bits(4)? as u8;
    if prec_minus_1 == 15 {
        return Err(RsError::InvalidData(
            "FLAC: invalid LPC coefficient precision 15".into(),
        ));
    }
    let precision = prec_minus_1 + 1;
    let shift = reader.read_signed_bits(5)? as i32;

    let mut coeff_buf = [0i64; 32];
    let coeffs = &mut coeff_buf[..order];
    for c in coeffs.iter_mut() {
        *c = reader.read_signed_bits(precision)?;
    }

    decode_residual(reader, blocksize, order, out)?;
    let s = out.as_mut_slice();
    for i in order..s.len() {
        let mut sum: i64 = 0;
        for (j, &coeff) in coeffs.iter().enumerate() {
            sum = sum.wrapping_add(coeff.wrapping_mul(s[i - 1 - j]));
        }
        let pred = if shift >= 0 {
            sum >> shift
        } else {
            sum.wrapping_shl((-shift) as u32)
        };
        s[i] = s[i].wrapping_add(pred);
    }

    Ok(())
}

/// Decode `blocksize - predictor_order` Rice-coded residuals, appending them
/// to `out` (which already holds the warm-up samples).
fn decode_residual(
    reader: &mut BitReader<'_>,
    blocksize: usize,
    predictor_order: usize,
    out: &mut Vec<i64>,
) -> RsResult<()> {
    let method = reader.read_bits(2)? as u8;
    let (param_bits, escape) = match method {
        0 => (4u8, 15u8),
        1 => (5u8, 31u8),
        _ => {
            return Err(RsError::InvalidData(
                format!("FLAC: reserved residual coding method {method}").into(),
            ))
        }
    };

    let p_order = reader.read_bits(4)? as u32;
    let partitions = 1usize << p_order;
    if blocksize % partitions != 0 {
        return Err(RsError::InvalidData(
            "FLAC: blocksize is not divisible by Rice partition count".into(),
        ));
    }
    let part_samples = blocksize >> p_order;
    if part_samples < predictor_order {
        return Err(RsError::InvalidData(
            "FLAC: Rice partition size smaller than predictor order".into(),
        ));
    }

    for p in 0..partitions {
        let n = if p == 0 {
            part_samples - predictor_order
        } else {
            part_samples
        };
        let param = reader.read_bits(param_bits)? as u8;
        if param == escape {
            let esc_bps = reader.read_bits(5)? as u8;
            for _ in 0..n {
                let val = if esc_bps == 0 {
                    0
                } else {
                    reader.read_signed_bits(esc_bps)?
                };
                out.push(val);
            }
        } else {
            for _ in 0..n {
                let q = reader.read_unary()?;
                if q > (u64::MAX >> param) {
                    return Err(RsError::InvalidData("FLAC: Rice quotient overflow".into()));
                }
                let r = reader.read_bits(param)?;
                let u = (q << param) | r;
                let val = if (u & 1) == 0 {
                    (u >> 1) as i64
                } else {
                    -((u >> 1) as i64) - 1
                };
                out.push(val);
            }
        }
    }

    Ok(())
}

fn decorrelate_channels(ch_code: u8, blocksize: usize, channels: &mut [Vec<i64>]) {
    if channels.len() != 2 {
        return;
    }
    let (head, tail) = channels.split_at_mut(1);
    let pairs = head[0].iter_mut().zip(tail[0].iter_mut()).take(blocksize);
    match ch_code {
        8 => {
            // Left / Side: right = left - side
            for (left, side) in pairs {
                *side = left.wrapping_sub(*side);
            }
        }
        9 => {
            // Side / Right: left = side + right
            for (side, right) in pairs {
                *side = side.wrapping_add(*right);
            }
        }
        10 => {
            // Mid / Side
            for (ch0, ch1) in pairs {
                let mid = *ch0;
                let side = *ch1;
                let m = (mid << 1) | (side & 1);
                *ch0 = m.wrapping_add(side) >> 1;
                *ch1 = m.wrapping_sub(side) >> 1;
            }
        }
        _ => {}
    }
}

fn pack_interleaved_pcm(
    bps: u16,
    blocksize: usize,
    channels: &[Vec<i64>],
) -> (SampleFormat, Vec<u8>) {
    let ch_count = channels.len();
    if bps <= 16 {
        let shift = 16 - u32::from(bps);
        let scale = 1i64 << shift;
        let mut bytes = Vec::with_capacity(blocksize * ch_count * 2);
        for i in 0..blocksize {
            for ch in channels {
                let s = ch[i]
                    .saturating_mul(scale)
                    .clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16;
                bytes.extend_from_slice(&s.to_le_bytes());
            }
        }
        (SampleFormat::S16, bytes)
    } else {
        let shift = 32 - u32::from(bps);
        let scale = 1i64 << shift;
        let mut bytes = Vec::with_capacity(blocksize * ch_count * 4);
        for i in 0..blocksize {
            for ch in channels {
                let s = ch[i]
                    .saturating_mul(scale)
                    .clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
                bytes.extend_from_slice(&s.to_le_bytes());
            }
        }
        (SampleFormat::S32, bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    struct BitWriter {
        bytes: Vec<u8>,
        cur: u8,
        bits_used: u8,
    }

    impl BitWriter {
        fn new() -> Self {
            Self {
                bytes: Vec::new(),
                cur: 0,
                bits_used: 0,
            }
        }

        fn write_bit(&mut self, bit: u8) {
            self.cur = (self.cur << 1) | (bit & 1);
            self.bits_used += 1;
            if self.bits_used == 8 {
                self.bytes.push(self.cur);
                self.cur = 0;
                self.bits_used = 0;
            }
        }

        fn write_bits(&mut self, val: u64, n: u8) {
            for shift in (0..n).rev() {
                self.write_bit(((val >> shift) & 1) as u8);
            }
        }

        fn write_signed_bits(&mut self, val: i64, n: u8) {
            let mask = if n == 64 { u64::MAX } else { (1u64 << n) - 1 };
            self.write_bits((val as u64) & mask, n);
        }

        fn write_unary(&mut self, q: u64) {
            for _ in 0..q {
                self.write_bit(0);
            }
            self.write_bit(1);
        }

        fn write_utf8_u64(&mut self, val: u64) {
            if val < 0x80 {
                self.write_bits(val, 8);
                return;
            }
            let total_bytes = if val < (1 << 11) {
                2
            } else if val < (1 << 16) {
                3
            } else if val < (1 << 21) {
                4
            } else if val < (1 << 26) {
                5
            } else if val < (1 << 31) {
                6
            } else {
                7
            };
            let cont = total_bytes - 1;
            let prefix_ones = ((1u8 << total_bytes) - 1) << (8 - total_bytes);
            let top = if total_bytes < 7 {
                ((val >> (cont * 6)) as u8) & ((1u8 << (7 - total_bytes)) - 1)
            } else {
                0
            };
            self.write_bits(u64::from(prefix_ones | top), 8);
            for i in (0..cont).rev() {
                let chunk = ((val >> (i * 6)) as u8) & 0x3F;
                self.write_bits(u64::from(0x80 | chunk), 8);
            }
        }

        fn align_to_byte(&mut self) {
            if self.bits_used != 0 {
                let pad = 8 - self.bits_used;
                self.write_bits(0, pad);
            }
        }

        fn finish_frame(mut self) -> Vec<u8> {
            self.align_to_byte();
            let c16 = crc16(&self.bytes);
            self.bytes.extend_from_slice(&c16.to_be_bytes());
            self.bytes
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn write_frame_header(
        w: &mut BitWriter,
        bs_code: u8,
        sr_code: u8,
        ch_code: u8,
        bps_code: u8,
        coded_number: u64,
        explicit_bs: Option<u16>,
        explicit_sr: Option<u16>,
    ) {
        w.write_bits(0x3FFE, 14);
        w.write_bit(0); // reserved
        w.write_bit(0); // fixed-blocksize
        w.write_bits(u64::from(bs_code), 4);
        w.write_bits(u64::from(sr_code), 4);
        w.write_bits(u64::from(ch_code), 4);
        w.write_bits(u64::from(bps_code), 3);
        w.write_bit(0); // reserved
        w.write_utf8_u64(coded_number);
        if let Some(bs) = explicit_bs {
            if bs_code == 6 {
                w.write_bits(u64::from(bs - 1), 8);
            } else if bs_code == 7 {
                w.write_bits(u64::from(bs - 1), 16);
            }
        }
        if let Some(sr) = explicit_sr {
            if sr_code == 12 {
                w.write_bits(u64::from(sr), 8);
            } else if sr_code == 13 || sr_code == 14 {
                w.write_bits(u64::from(sr), 16);
            }
        }
        let c8 = crc8(&w.bytes);
        w.write_bits(u64::from(c8), 8);
    }

    fn write_rice_residuals(w: &mut BitWriter, residuals: &[i64], param: u8, method: u8) {
        w.write_bits(u64::from(method), 2);
        w.write_bits(0, 4); // partition order 0 (1 partition)
        let param_bits = if method == 0 { 4 } else { 5 };
        w.write_bits(u64::from(param), param_bits);
        let mask = if param == 0 { 0 } else { (1u64 << param) - 1 };
        for &r in residuals {
            let u: u64 = if r >= 0 {
                (r as u64) << 1
            } else {
                (((-r - 1) as u64) << 1) | 1
            };
            let q = u >> param;
            let rem = u & mask;
            w.write_unary(q);
            w.write_bits(rem, param);
        }
    }

    fn build_streaminfo(
        min_bs: u16,
        max_bs: u16,
        sample_rate: u32,
        channels: u16,
        bps: u16,
        total_samples: u64,
    ) -> Vec<u8> {
        let mut info = vec![0u8; 34];
        info[0..2].copy_from_slice(&min_bs.to_be_bytes());
        info[2..4].copy_from_slice(&max_bs.to_be_bytes());
        info[10] = (sample_rate >> 12) as u8;
        info[11] = (sample_rate >> 4) as u8;
        let ch_m1 = ((channels - 1) as u8) & 0x07;
        let bps_m1 = ((bps - 1) as u8) & 0x1F;
        info[12] = (((sample_rate & 0x0F) as u8) << 4) | (ch_m1 << 1) | (bps_m1 >> 4);
        info[13] = ((bps_m1 & 0x0F) << 4) | (((total_samples >> 32) as u8) & 0x0F);
        info[14..18].copy_from_slice(&(total_samples as u32).to_be_bytes());
        info
    }

    #[test]
    fn test_crc8_and_crc16_vectors() {
        assert_eq!(crc8(b""), 0x00);
        assert_eq!(crc8(b"123456789"), 0xF4);
        assert_eq!(crc16(b""), 0x0000);
        assert_eq!(crc16(b"123456789"), 0xFEE8);
    }

    #[test]
    fn test_bitreader_and_utf8_numbers() {
        let mut w = BitWriter::new();
        w.write_bits(0b101, 3);
        w.write_signed_bits(-5, 4);
        w.write_unary(4);
        w.align_to_byte();
        for &val in &[0u64, 1, 127, 128, 2047, 65535, 1_000_000, (1u64 << 36) - 1] {
            w.write_utf8_u64(val);
        }

        let mut r = BitReader::new(&w.bytes);
        assert_eq!(r.read_bits(3).unwrap(), 0b101);
        assert_eq!(r.read_signed_bits(4).unwrap(), -5);
        assert_eq!(r.read_unary().unwrap(), 4);
        r.align_to_byte();
        assert_eq!(r.byte_position(), 2);
        for &expected in &[0u64, 1, 127, 128, 2047, 65535, 1_000_000, (1u64 << 36) - 1] {
            assert_eq!(r.read_utf8_u64().unwrap(), expected);
        }

        // Invalid UTF-8 prefix 0xFF and stray continuation 0x80
        assert!(BitReader::new(&[0xFF]).read_utf8_u64().is_err());
        assert!(BitReader::new(&[0x80]).read_utf8_u64().is_err());
    }

    #[test]
    fn test_from_params_raw_and_flac_header_extradata() {
        let si = build_streaminfo(4096, 4096, 48_000, 2, 24, 1_234_567);

        let mut p1 = CodecParameters::new(CodecId::Flac);
        p1.extradata = Some(si.clone());
        let dec1 = FlacDecoder::from_params(&p1).unwrap();
        let out1 = dec1.get_parameters();
        assert_eq!(out1.sample_rate, Some(48_000));
        assert_eq!(out1.channels, Some(2));
        assert_eq!(out1.sample_format, Some(SampleFormat::S32));
        let parsed1 = dec1.streaminfo.unwrap();
        assert_eq!(parsed1.min_blocksize, 4096);
        assert_eq!(parsed1.max_blocksize, 4096);
        assert_eq!(parsed1.bits_per_sample, 24);
        assert_eq!(parsed1.total_samples, 1_234_567);

        // `fLaC` + metadata block header + 34 bytes
        let mut with_magic = Vec::from(&b"fLaC"[..]);
        with_magic.extend_from_slice(&[0x80, 0x00, 0x00, 34]);
        with_magic.extend_from_slice(&si);
        let mut p2 = CodecParameters::new(CodecId::Unknown);
        p2.extradata = Some(with_magic);
        let dec2 = FlacDecoder::from_params(&p2).unwrap();
        assert_eq!(dec2.streaminfo, dec1.streaminfo);

        // Reject non-FLAC codec
        let bad = CodecParameters::new(CodecId::Pcm);
        assert!(matches!(
            FlacDecoder::from_params(&bad),
            Err(RsError::Unsupported(_))
        ));
    }

    #[test]
    fn test_decode_constant_and_verbatim() {
        // 1) CONSTANT 16-bit mono, explicit blocksize 4 via code 6, 44100 Hz (code 9)
        let mut w = BitWriter::new();
        write_frame_header(&mut w, 6, 9, 0, 4, 0, Some(4), None);
        w.write_bit(0); // subframe pad
        w.write_bits(0, 6); // CONSTANT
        w.write_bit(0); // no wasted
        w.write_signed_bits(-1234, 16);
        let frame_bytes = w.finish_frame();

        let mut dec = FlacDecoder::new();
        let pkt = Packet::new(Bytes::from(frame_bytes), 0);
        let frames = dec.decode(&pkt).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].sample_rate, 44_100);
        assert_eq!(frames[0].channels, 1);
        assert_eq!(frames[0].samples, 4);
        assert_eq!(frames[0].sample_format, SampleFormat::S16);
        let decoded: Vec<i16> = frames[0].data[0]
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(decoded, vec![-1234, -1234, -1234, -1234]);

        // 2) VERBATIM 8-bit mono -> shifted up to S16
        let mut w2 = BitWriter::new();
        write_frame_header(&mut w2, 6, 10, 0, 1, 1, Some(3), None);
        w2.write_bit(0);
        w2.write_bits(1, 6); // VERBATIM
        w2.write_bit(0);
        for s in [-10i64, 0, 25] {
            w2.write_signed_bits(s, 8);
        }
        let frame2 = w2.finish_frame();
        let frames2 = dec.decode(&Packet::new(Bytes::from(frame2), 0)).unwrap();
        let decoded2: Vec<i16> = frames2[0].data[0]
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(decoded2, vec![-10 << 8, 0, 25 << 8]);
    }

    #[test]
    fn test_decode_fixed_order1_and_order2_and_escaped() {
        // Order 1 FIXED: samples [100, 105, 112, 110] -> warmup [100], residuals [+5, +7, -2]
        let mut w = BitWriter::new();
        write_frame_header(&mut w, 6, 10, 0, 4, 0, Some(4), None);
        w.write_bit(0);
        w.write_bits(8 + 1, 6); // FIXED order 1
        w.write_bit(0);
        w.write_signed_bits(100, 16);
        write_rice_residuals(&mut w, &[5, 7, -2], 2, 0);
        let bytes1 = w.finish_frame();

        let mut dec = FlacDecoder::new();
        let f1 = dec.decode(&Packet::new(Bytes::from(bytes1), 0)).unwrap();
        let out1: Vec<i16> = f1[0].data[0]
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(out1, vec![100, 105, 112, 110]);

        // Order 2 FIXED with wasted bits (k = 2) and escaped Rice2 residual:
        // True samples: [40, 80, 132, 200] (all divisible by 4, shifted by 2 -> [10, 20, 33, 50])
        // Order 2 pred: 2*s[n-1] - s[n-2]:
        //   i=2: pred = 2*20 - 10 = 30, residual = 33 - 30 = +3
        //   i=3: pred = 2*33 - 20 = 46, residual = 50 - 46 = +4
        let mut w2 = BitWriter::new();
        write_frame_header(&mut w2, 6, 10, 0, 4, 1, Some(4), None);
        w2.write_bit(0);
        w2.write_bits(8 + 2, 6); // FIXED order 2
        w2.write_bit(1); // wasted bits flag
        w2.write_unary(1); // k = 1 + 1 = 2 wasted bits -> eff_bps = 14
        w2.write_signed_bits(10, 14);
        w2.write_signed_bits(20, 14);
        // Residual method 1 (Rice2), partition order 0, escaped param = 31, esc_bps = 5
        w2.write_bits(1, 2);
        w2.write_bits(0, 4);
        w2.write_bits(31, 5);
        w2.write_bits(5, 5);
        w2.write_signed_bits(3, 5);
        w2.write_signed_bits(4, 5);
        let bytes2 = w2.finish_frame();

        let f2 = dec.decode(&Packet::new(Bytes::from(bytes2), 0)).unwrap();
        let out2: Vec<i16> = f2[0].data[0]
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(out2, vec![40, 80, 132, 200]);
    }

    #[test]
    fn test_decode_lpc_order1() {
        // LPC order 1, coeff = 3, shift = 1 (so pred = (3 * s[n-1]) >> 1), precision = 4
        // s[0] = 10
        // s[1]: pred = (3 * 10) >> 1 = 15, r = 2 => s[1] = 17
        // s[2]: pred = (3 * 17) >> 1 = 25, r = -3 => s[2] = 22
        // s[3]: pred = (3 * 22) >> 1 = 33, r = 1 => s[3] = 34
        let mut w = BitWriter::new();
        write_frame_header(&mut w, 6, 9, 0, 4, 0, Some(4), None);
        w.write_bit(0);
        w.write_bits(32, 6); // LPC order 1 (32 + 0 -> order = 32 - 31 = 1)
        w.write_bit(0);
        w.write_signed_bits(10, 16); // warmup
        w.write_bits(3, 4); // prec_minus_1 = 3 -> 4 bits
        w.write_signed_bits(1, 5); // shift = 1
        w.write_signed_bits(3, 4); // coeff[0] = 3
        write_rice_residuals(&mut w, &[2, -3, 1], 1, 0);
        let bytes = w.finish_frame();

        let mut dec = FlacDecoder::new();
        let frames = dec.decode(&Packet::new(Bytes::from(bytes), 0)).unwrap();
        let out: Vec<i16> = frames[0].data[0]
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(out, vec![10, 17, 22, 34]);
    }

    #[test]
    fn test_decode_stereo_decorrelations_and_24bit() {
        // Left-side (ch_code = 8): left = [100, -50], right = [70, -80] => side = [30, 30]
        let mut w_ls = BitWriter::new();
        write_frame_header(&mut w_ls, 6, 9, 8, 4, 0, Some(2), None);
        // ch0: left VERBATIM
        w_ls.write_bit(0);
        w_ls.write_bits(1, 6);
        w_ls.write_bit(0);
        w_ls.write_signed_bits(100, 16);
        w_ls.write_signed_bits(-50, 16);
        // ch1: side CONSTANT (17 bits because sub_bps = 16 + 1!)
        w_ls.write_bit(0);
        w_ls.write_bits(0, 6);
        w_ls.write_bit(0);
        w_ls.write_signed_bits(30, 17);
        let bytes_ls = w_ls.finish_frame();

        let mut dec = FlacDecoder::new();
        let frames_ls = dec.decode(&Packet::new(Bytes::from(bytes_ls), 0)).unwrap();
        let out_ls: Vec<i16> = frames_ls[0].data[0]
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(out_ls, vec![100, 70, -50, -80]);

        // Right-side (ch_code = 9): left = [100, -50], right = [70, -80] => side = [30, 30]
        let mut w_rs = BitWriter::new();
        write_frame_header(&mut w_rs, 6, 9, 9, 4, 0, Some(2), None);
        // ch0: side (17 bits)
        w_rs.write_bit(0);
        w_rs.write_bits(0, 6);
        w_rs.write_bit(0);
        w_rs.write_signed_bits(30, 17);
        // ch1: right VERBATIM (16 bits)
        w_rs.write_bit(0);
        w_rs.write_bits(1, 6);
        w_rs.write_bit(0);
        w_rs.write_signed_bits(70, 16);
        w_rs.write_signed_bits(-80, 16);
        let bytes_rs = w_rs.finish_frame();

        let frames_rs = dec.decode(&Packet::new(Bytes::from(bytes_rs), 0)).unwrap();
        let out_rs: Vec<i16> = frames_rs[0].data[0]
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(out_rs, vec![100, 70, -50, -80]);

        // Mid-side (ch_code = 10), 24-bit (bps_code = 6):
        // sample 0: left = 1000, right = 1001 => mid = 1000, side = -1
        // sample 1: left = -500, right = 200 => mid = -150, side = -700
        let mut w_ms = BitWriter::new();
        write_frame_header(&mut w_ms, 6, 10, 10, 6, 0, Some(2), None);
        // ch0: mid (24 bits)
        w_ms.write_bit(0);
        w_ms.write_bits(1, 6);
        w_ms.write_bit(0);
        w_ms.write_signed_bits(1000, 24);
        w_ms.write_signed_bits(-150, 24);
        // ch1: side (25 bits)
        w_ms.write_bit(0);
        w_ms.write_bits(1, 6);
        w_ms.write_bit(0);
        w_ms.write_signed_bits(-1, 25);
        w_ms.write_signed_bits(-700, 25);
        let bytes_ms = w_ms.finish_frame();

        let frames_ms = dec.decode(&Packet::new(Bytes::from(bytes_ms), 0)).unwrap();
        assert_eq!(frames_ms[0].sample_format, SampleFormat::S32);
        let out_ms: Vec<i32> = frames_ms[0].data[0]
            .chunks_exact(4)
            .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        assert_eq!(out_ms, vec![1000 << 8, 1001 << 8, -500 << 8, 200 << 8]);
    }

    #[test]
    fn test_bad_crc_multi_frame_and_lifecycle() {
        let mut w1 = BitWriter::new();
        write_frame_header(&mut w1, 6, 9, 0, 4, 0, Some(2), None);
        w1.write_bit(0);
        w1.write_bits(0, 6);
        w1.write_bit(0);
        w1.write_signed_bits(42, 16);
        let f1 = w1.finish_frame();

        // Corrupt header CRC-8
        let mut bad_hdr = f1.clone();
        bad_hdr[5] ^= 0xFF;
        let mut dec = FlacDecoder::new();
        assert!(dec.decode(&Packet::new(Bytes::from(bad_hdr), 0)).is_err());

        // Corrupt footer CRC-16
        let mut bad_ftr = f1.clone();
        let last = bad_ftr.len() - 1;
        bad_ftr[last] ^= 0x01;
        assert!(dec.decode(&Packet::new(Bytes::from(bad_ftr), 0)).is_err());

        // Multi-frame packet
        let mut w2 = BitWriter::new();
        write_frame_header(&mut w2, 6, 9, 0, 4, 1, Some(2), None);
        w2.write_bit(0);
        w2.write_bits(0, 6);
        w2.write_bit(0);
        w2.write_signed_bits(99, 16);
        let f2 = w2.finish_frame();

        let mut combined = f1;
        combined.extend_from_slice(&f2);
        let mut multi_pkt = Packet::new(Bytes::from(combined), 0);
        multi_pkt.pts = Some(0);
        multi_pkt.time_base = Rational::new(1, 44_100);

        dec.send_packet(Some(&multi_pkt)).unwrap();
        let out_frames = [
            match dec.receive_frame().unwrap() {
                DecodeStatus::Frame(f) => f,
                other => panic!("expected frame 0, got {other:?}"),
            },
            match dec.receive_frame().unwrap() {
                DecodeStatus::Frame(f) => f,
                other => panic!("expected frame 1, got {other:?}"),
            },
        ];
        assert_eq!(out_frames[0].pts, Some(0));
        assert_eq!(out_frames[1].pts, Some(2));
        assert!(matches!(
            dec.receive_frame().unwrap(),
            DecodeStatus::NeedMoreInput
        ));

        // EOS and reset lifecycle
        dec.send_packet(None).unwrap();
        assert!(matches!(
            dec.receive_frame().unwrap(),
            DecodeStatus::EndOfStream
        ));
        assert!(dec.send_packet(Some(&multi_pkt)).is_err());

        dec.reset().unwrap();
        assert!(matches!(
            dec.receive_frame().unwrap(),
            DecodeStatus::NeedMoreInput
        ));

        // FlacCodec descriptor
        let codec = FlacCodec;
        assert_eq!(codec.id(), CodecId::Flac);
        assert_eq!(codec.media_type(), MediaType::Audio);
        assert_eq!(codec.name(), "flac");
        assert!(codec.capabilities().can_decode);
        assert!(!codec.capabilities().can_encode);
        assert!(codec.capabilities().lossless);
        assert!(codec.capabilities().intra_only);
        assert!(codec.create_decoder().is_ok());
        assert!(matches!(
            codec.create_encoder(),
            Err(RsError::Unsupported(_))
        ));
    }

    /// Bit-at-a-time reference reader used to cross-check the reservoir.
    struct RefReader<'a> {
        data: &'a [u8],
        bit: usize,
    }

    impl RefReader<'_> {
        fn bits(&mut self, n: u8) -> Option<u64> {
            let mut v = 0u64;
            for _ in 0..n {
                let byte = *self.data.get(self.bit / 8)?;
                v = (v << 1) | u64::from((byte >> (7 - (self.bit % 8))) & 1);
                self.bit += 1;
            }
            Some(v)
        }
        fn unary(&mut self) -> Option<u64> {
            let mut n = 0;
            while self.bits(1)? == 0 {
                n += 1;
            }
            Some(n)
        }
    }

    #[test]
    fn bitreader_reservoir_matches_reference_on_random_streams() {
        let mut state = 0x1234_5678_9ABC_DEF0u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for len in [0usize, 1, 7, 8, 9, 15, 16, 17, 63, 64, 65, 257] {
            let data: Vec<u8> = (0..len).map(|_| (next() >> 24) as u8).collect();
            let mut r = BitReader::new(&data);
            let mut reference = RefReader {
                data: &data,
                bit: 0,
            };
            loop {
                let op = next() % 5;
                if op == 0 {
                    let a = r.read_unary().ok();
                    let b = reference.unary();
                    assert_eq!(a, b, "unary mismatch len={len}");
                    if a.is_none() {
                        break;
                    }
                } else if op == 1 {
                    r.align_to_byte();
                    reference.bit = reference.bit.div_ceil(8) * 8;
                    assert_eq!(r.byte_position(), reference.bit / 8);
                } else {
                    let n = (next() % 65) as u8;
                    let a = r.read_bits(n).ok();
                    let b = reference.bits(n);
                    assert_eq!(a, b, "read_bits({n}) mismatch len={len}");
                    if a.is_none() {
                        break;
                    }
                    assert_eq!(r.byte_position(), reference.bit / 8);
                }
            }
        }
    }

    #[test]
    fn bitreader_wide_reads_and_reservoir_boundaries() {
        let data: Vec<u8> = (0u8..32).map(|i| i.wrapping_mul(37).wrapping_add(11)).collect();
        // 64-bit reads at every bit offset 0..8.
        for skip in 0..8u8 {
            let mut r = BitReader::new(&data);
            let mut reference = RefReader {
                data: &data,
                bit: 0,
            };
            r.read_bits(skip).unwrap();
            reference.bits(skip);
            for n in [64u8, 57, 56, 1, 63, 33, 64] {
                assert_eq!(r.read_bits(n).ok(), reference.bits(n), "skip={skip} n={n}");
            }
        }
        // Signed 64/57-bit reads.
        let all_ones = [0xFFu8; 16];
        let mut r = BitReader::new(&all_ones);
        assert_eq!(r.read_signed_bits(64).unwrap(), -1);
        assert_eq!(r.read_signed_bits(57).unwrap(), -1);
        assert_eq!(r.read_signed_bits(7).unwrap(), -1);
        assert!(r.read_signed_bits(1).is_err(), "exhausted");
        assert!(BitReader::new(&[0u8; 1]).read_bits(65).is_err());
    }

    #[test]
    fn bitreader_unary_spanning_reservoir_and_eof() {
        // 150 zero bits then a one: the run crosses several 64-bit refills.
        let mut w = BitWriter::new();
        w.write_unary(150);
        w.write_bits(0b1011, 4);
        w.write_unary(0);
        w.write_unary(63);
        w.align_to_byte();
        let mut r = BitReader::new(&w.bytes);
        assert_eq!(r.read_unary().unwrap(), 150);
        assert_eq!(r.read_bits(4).unwrap(), 0b1011);
        assert_eq!(r.read_unary().unwrap(), 0);
        assert_eq!(r.read_unary().unwrap(), 63);
        r.align_to_byte();
        assert_eq!(r.byte_position(), w.bytes.len());

        // All-zero input never terminates the unary code: clean EOF error.
        let zeros = [0u8; 40];
        assert!(BitReader::new(&zeros).read_unary().is_err());
        assert!(BitReader::new(&[]).read_unary().is_err());
        assert!(BitReader::new(&[]).read_bit().is_err());

        // A one in the very last bit of a full 64-bit reservoir.
        let mut last = [0u8; 8];
        last[7] = 1;
        let mut r = BitReader::new(&last);
        assert_eq!(r.read_unary().unwrap(), 63);
        assert_eq!(r.byte_position(), 8);
        assert!(r.read_bit().is_err());
    }

    #[test]
    fn bitreader_align_and_byte_position_are_exact() {
        let data = [0xA5u8, 0x5A, 0xFF, 0x00, 0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC];
        let mut r = BitReader::new(&data);
        assert_eq!(r.byte_position(), 0);
        r.align_to_byte();
        assert_eq!(r.byte_position(), 0, "aligning at a boundary is a no-op");
        r.read_bits(3).unwrap();
        assert_eq!(r.byte_position(), 0);
        r.align_to_byte();
        assert_eq!(r.byte_position(), 1);
        assert_eq!(r.read_bits(8).unwrap(), 0x5A);
        assert_eq!(r.byte_position(), 2);
        r.read_bits(13).unwrap();
        r.align_to_byte();
        assert_eq!(r.byte_position(), 4);
        assert_eq!(r.read_bits(16).unwrap(), 0x1234);
        assert_eq!(r.read_bits(32).unwrap(), 0x5678_9ABC);
        assert_eq!(r.byte_position(), data.len());
    }

    /// Build a VERBATIM mono 16-bit frame of `samples`.
    fn verbatim_frame(samples: &[i16], frame_no: u64) -> Vec<u8> {
        let mut w = BitWriter::new();
        write_frame_header(
            &mut w,
            7,
            9,
            0,
            4,
            frame_no,
            Some(samples.len() as u16),
            None,
        );
        w.write_bit(0);
        w.write_bits(1, 6);
        w.write_bit(0);
        for &s in samples {
            w.write_signed_bits(i64::from(s), 16);
        }
        w.finish_frame()
    }

    #[test]
    fn crc16_checks_stay_byte_exact_for_large_and_odd_frames() {
        // Frames whose subframe data ends at every bit offset before the
        // byte-alignment padding: an LPC frame with an odd residual count and
        // large verbatim frames that cross many reservoir refills.
        let big: Vec<i16> = (0..4096).map(|i| ((i * 7919) % 65_536 - 32_768) as i16).collect();
        let odd: Vec<i16> = (0..777).map(|i| (i as i16).wrapping_mul(-3)).collect();
        let mut packet = verbatim_frame(&big, 0);
        packet.extend_from_slice(&verbatim_frame(&odd, 1));

        // Frame 3: 5-sample FIXED order-1 with Rice param 0 (unaligned end).
        let mut w = BitWriter::new();
        write_frame_header(&mut w, 6, 9, 0, 4, 2, Some(5), None);
        w.write_bit(0);
        w.write_bits(8 + 1, 6);
        w.write_bit(0);
        w.write_signed_bits(-7, 16);
        write_rice_residuals(&mut w, &[1, -1, 2, 0], 0, 0);
        packet.extend_from_slice(&w.finish_frame());

        let mut dec = FlacDecoder::new();
        let frames = dec.decode(&Packet::new(Bytes::from(packet.clone()), 0)).unwrap();
        assert_eq!(frames.len(), 3);
        let as_i16 = |f: &Frame| -> Vec<i16> {
            f.data[0]
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                .collect()
        };
        assert_eq!(as_i16(&frames[0]), big);
        assert_eq!(as_i16(&frames[1]), odd);
        assert_eq!(as_i16(&frames[2]), vec![-7, -6, -7, -5, -5]);

        // Flipping any single bit of the last frame's CRC-16 must be caught.
        for bit in 0..16 {
            let mut bad = packet.clone();
            let idx = bad.len() - 1 - bit / 8;
            bad[idx] ^= 1 << (bit % 8);
            assert!(dec.decode(&Packet::new(Bytes::from(bad), 0)).is_err());
        }
        // Truncating the footer is an error, not a panic.
        let short = &packet[..packet.len() - 1];
        assert!(dec
            .decode(&Packet::new(Bytes::copy_from_slice(short), 0))
            .is_err());
    }

    #[test]
    fn channel_buffers_are_reused_across_frames_and_cleared_on_reset() {
        let stereo = {
            let mut w = BitWriter::new();
            write_frame_header(&mut w, 6, 9, 1, 4, 0, Some(64), None);
            for ch in 0..2i64 {
                w.write_bit(0);
                w.write_bits(1, 6);
                w.write_bit(0);
                for i in 0..64i64 {
                    w.write_signed_bits(i * (ch + 1), 16);
                }
            }
            w.finish_frame()
        };
        let mono = verbatim_frame(&[5; 32], 1);

        let mut dec = FlacDecoder::new();
        dec.decode(&Packet::new(Bytes::from(stereo.clone()), 0))
            .unwrap();
        assert_eq!(dec.channel_buffers.len(), 2);
        let caps: Vec<usize> = dec.channel_buffers.iter().map(Vec::capacity).collect();
        assert!(caps.iter().all(|&c| c >= 64));

        // A smaller mono frame keeps (and reuses) the existing buffers.
        let f = dec.decode(&Packet::new(Bytes::from(mono), 0)).unwrap();
        assert_eq!(f[0].channels, 1);
        assert_eq!(f[0].samples, 32);
        assert_eq!(dec.channel_buffers.len(), 2);
        assert_eq!(dec.channel_buffers[0].capacity(), caps[0]);

        // Decoding the stereo frame again does not grow the buffers.
        let f = dec
            .decode(&Packet::new(Bytes::from(stereo.clone()), 0))
            .unwrap();
        assert_eq!(f[0].channels, 2);
        let caps2: Vec<usize> = dec.channel_buffers.iter().map(Vec::capacity).collect();
        assert_eq!(caps, caps2, "buffers must be reused in place");
        let pcm: Vec<i16> = f[0].data[0]
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(&pcm[..6], &[0, 0, 1, 2, 2, 4]);

        // A corrupt frame must not lose the buffers either.
        let mut bad = stereo;
        let last = bad.len() - 1;
        bad[last] ^= 0x80;
        assert!(dec.decode(&Packet::new(Bytes::from(bad), 0)).is_err());
        assert_eq!(dec.channel_buffers.len(), 2);

        dec.reset().unwrap();
        assert!(dec.channel_buffers.is_empty());
    }
}
