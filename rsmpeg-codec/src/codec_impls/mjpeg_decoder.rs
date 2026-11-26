//! Pure-Rust ITU-T T.81 Baseline DCT JPEG / Motion-JPEG video decoder.
//!
//! Supports:
//! - Standard Baseline DCT sequential JPEG (`SOF0`) and 8-bit Extended
//!   sequential DCT, Huffman-coded JPEG (`SOF1`)
//! - Motion-JPEG frames that omit `DHT` markers (pre-initialized with ITU-T T.81
//!   Annex K.3.3 default luminance/chrominance DC and AC Huffman tables)
//! - 8-bit and 16-bit quantization tables (`DQT`)
//! - Grayscale (1-component) and YCbCr (3-component) images with arbitrary
//!   sampling factors in `1..=4` (4:4:4, 4:2:2, 4:2:0, 4:1:1, etc.)
//! - Interleaved and non-interleaved baseline scans (`SOS`)
//! - Restart intervals (`DRI`) and restart markers (`RST0..RST7`)
//! - Byte-unstuffing (`0xFF 0x00` -> `0xFF`) and fill `0xFF` bytes

use std::collections::VecDeque;

use crate::codec::{Codec, CodecCapabilities, DecodeStatus, Decoder, Encoder};
use crate::codec_id::CodecId;
use crate::codec_parameters::CodecParameters;
use crate::frame::Frame;
use crate::packet::Packet;
use crate::picture_type::PictureType;
use rsmpeg_util::{MediaType, PixelFormat, RsError, RsResult};

const MAX_DIMENSION: usize = 16_384;

/// Zigzag index mapping from zigzag coefficient order `0..64` to natural 8x8 row-major order.
const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

// ── ITU-T T.81 Annex K.3.3 Standard Huffman Tables ──────────────────────────

/// Table K.3: Luminance DC differences.
const STD_DC_LUM_BITS: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
const STD_DC_LUM_VALS: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];

/// Table K.4: Chrominance DC differences.
const STD_DC_CHR_BITS: [u8; 16] = [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0];
const STD_DC_CHR_VALS: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];

/// Table K.5: Luminance AC coefficients.
const STD_AC_LUM_BITS: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 125];
const STD_AC_LUM_VALS: [u8; 162] = [
    0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07,
    0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xA1, 0x08, 0x23, 0x42, 0xB1, 0xC1, 0x15, 0x52, 0xD1, 0xF0,
    0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0A, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x25, 0x26, 0x27, 0x28,
    0x29, 0x2A, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49,
    0x4A, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69,
    0x6A, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89,
    0x8A, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7,
    0xA8, 0xA9, 0xAA, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3, 0xC4, 0xC5,
    0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xE1, 0xE2,
    0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8,
    0xF9, 0xFA,
];

/// Table K.6: Chrominance AC coefficients.
const STD_AC_CHR_BITS: [u8; 16] = [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 119];
const STD_AC_CHR_VALS: [u8; 162] = [
    0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71,
    0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xA1, 0xB1, 0xC1, 0x09, 0x23, 0x33, 0x52, 0xF0,
    0x15, 0x62, 0x72, 0xD1, 0x0A, 0x16, 0x24, 0x34, 0xE1, 0x25, 0xF1, 0x17, 0x18, 0x19, 0x1A, 0x26,
    0x27, 0x28, 0x29, 0x2A, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48,
    0x49, 0x4A, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68,
    0x69, 0x6A, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87,
    0x88, 0x89, 0x8A, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0xA2, 0xA3, 0xA4, 0xA5,
    0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3,
    0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA,
    0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8,
    0xF9, 0xFA,
];

// ── Public Codec & Decoder API ──────────────────────────────────────────────

/// Motion JPEG codec descriptor.
pub struct MjpegCodec;

impl Codec for MjpegCodec {
    fn id(&self) -> CodecId {
        CodecId::Mjpeg
    }

    fn media_type(&self) -> MediaType {
        MediaType::Video
    }

    fn name(&self) -> &'static str {
        "mjpeg"
    }

    fn long_name(&self) -> &'static str {
        "Pure-Rust Motion JPEG video decoder"
    }

    fn capabilities(&self) -> CodecCapabilities {
        CodecCapabilities {
            can_decode: true,
            can_encode: false,
            lossless: false,
            intra_only: true,
        }
    }

    fn create_decoder(&self) -> RsResult<Box<dyn Decoder>> {
        Ok(Box::new(MjpegDecoder::new()))
    }

    fn create_encoder(&self) -> RsResult<Box<dyn Encoder>> {
        Err(RsError::Unsupported(
            "MJPEG encoding not implemented".into(),
        ))
    }
}

/// Pure-Rust Motion JPEG (`CodecId::Mjpeg`) baseline decoder.
pub struct MjpegDecoder {
    params: CodecParameters,
    pending: VecDeque<Frame>,
    eof: bool,
}

impl MjpegDecoder {
    /// Create a new MJPEG decoder with default parameters.
    pub fn new() -> Self {
        let mut params = CodecParameters::new(CodecId::Mjpeg);
        params.pixel_format = Some(PixelFormat::Rgb24);
        Self {
            params,
            pending: VecDeque::new(),
            eof: false,
        }
    }

    /// Construct an MJPEG decoder from stream [`CodecParameters`].
    pub fn from_params(params: &CodecParameters) -> RsResult<Self> {
        if params.codec_id != CodecId::Mjpeg && params.codec_id != CodecId::Unknown {
            return Err(RsError::Unsupported(
                format!(
                    "MjpegDecoder only supports MJPEG, got {}",
                    params.codec_id.name()
                )
                .into(),
            ));
        }

        let mut stored = params.clone();
        stored.codec_id = CodecId::Mjpeg;
        stored.media_type = MediaType::Video;
        if stored.pixel_format.is_none() {
            stored.pixel_format = Some(PixelFormat::Rgb24);
        }

        Ok(Self {
            params: stored,
            pending: VecDeque::new(),
            eof: false,
        })
    }
}

impl Default for MjpegDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder for MjpegDecoder {
    fn codec_id(&self) -> CodecId {
        CodecId::Mjpeg
    }

    fn send_packet(&mut self, packet: Option<&Packet>) -> RsResult<()> {
        match packet {
            Some(pkt) => {
                if self.eof {
                    return Err(RsError::Codec(
                        "cannot send packet after end-of-stream; call reset() first".into(),
                    ));
                }
                let decoded = decode_baseline_jpeg(&pkt.data)?;
                let mut frame = Frame::new_video(decoded.width, decoded.height, PixelFormat::Rgb24);
                frame.data[0] = decoded.rgb24;
                frame.pts = pkt.pts;
                frame.duration = pkt.duration;
                frame.time_base = pkt.time_base;
                frame.key_frame = true;
                frame.pict_type = PictureType::I;

                self.params.width = Some(decoded.width);
                self.params.height = Some(decoded.height);
                self.params.pixel_format = Some(PixelFormat::Rgb24);

                self.pending.push_back(frame);
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
        Ok(())
    }

    fn get_parameters(&self) -> CodecParameters {
        self.params.clone()
    }
}

// ── Internal JPEG Structures & Parsing ──────────────────────────────────────

struct DecodedImage {
    width: usize,
    height: usize,
    rgb24: Vec<u8>,
}

#[derive(Clone)]
struct HuffmanTable {
    mincode: [i32; 17],
    maxcode: [i32; 17],
    valptr: [usize; 17],
    values: Vec<u8>,
    /// Fast 8-bit peek LUT: `(symbol, code_len)` where `code_len == 0` means > 8 bits.
    lut: [(u8, u8); 256],
}

impl HuffmanTable {
    fn from_bits_vals(counts: &[u8; 16], vals: &[u8]) -> RsResult<Self> {
        let total: usize = counts.iter().map(|&c| usize::from(c)).sum();
        if total > 256 || total != vals.len() {
            return Err(RsError::InvalidData(
                "Invalid JPEG Huffman symbol count".into(),
            ));
        }

        let mut mincode = [0i32; 17];
        let mut maxcode = [-1i32; 17];
        let mut valptr = [0usize; 17];
        let mut lut = [(0u8, 0u8); 256];

        let mut code: u32 = 0;
        let mut idx: usize = 0;
        for len in 1..=16 {
            let count = usize::from(counts[len - 1]);
            if count == 0 {
                maxcode[len] = -1;
            } else {
                let limit = 1u32 << len;
                if code + (count as u32) > limit {
                    return Err(RsError::InvalidData(
                        "JPEG Huffman table counts exceed canonical code space".into(),
                    ));
                }
                valptr[len] = idx;
                mincode[len] = code as i32;
                if len <= 8 {
                    let shift = 8 - len;
                    let span = 1usize << shift;
                    for offset in 0..count {
                        let sym = vals[idx + offset];
                        let base = ((code + offset as u32) as usize) << shift;
                        for entry in &mut lut[base..base + span] {
                            *entry = (sym, len as u8);
                        }
                    }
                }
                code += count as u32;
                maxcode[len] = (code - 1) as i32;
                idx += count;
            }
            code <<= 1;
        }

        Ok(Self {
            mincode,
            maxcode,
            valptr,
            values: vals.to_vec(),
            lut,
        })
    }
}

#[derive(Clone, Copy)]
struct ComponentInfo {
    id: u8,
    h_samp: usize,
    v_samp: usize,
    qt_id: usize,
}

struct FrameHeader {
    width: usize,
    height: usize,
    components: Vec<ComponentInfo>,
    max_h: usize,
    max_v: usize,
    mcu_cols: usize,
    mcu_rows: usize,
}

#[derive(Clone, Copy)]
struct ScanComponent {
    comp_idx: usize,
    dc_table_id: usize,
    ac_table_id: usize,
}

struct CompPlane {
    stride: usize,
    data: Vec<u8>,
}

impl CompPlane {
    fn try_new(width_blocks: usize, height_blocks: usize) -> RsResult<Self> {
        let stride = width_blocks
            .checked_mul(8)
            .ok_or_else(|| RsError::InvalidData("JPEG plane stride overflow".into()))?;
        let height_px = height_blocks
            .checked_mul(8)
            .ok_or_else(|| RsError::InvalidData("JPEG plane height overflow".into()))?;
        let len = stride
            .checked_mul(height_px)
            .ok_or_else(|| RsError::InvalidData("JPEG plane buffer overflow".into()))?;
        // Neutral 128 fill prevents green/black artifacts if EOI arrives early.
        Ok(Self {
            stride,
            data: vec![128u8; len],
        })
    }

    fn write_block(&mut self, bx: usize, by: usize, block: &[u8; 64]) {
        let base_x = bx * 8;
        let base_y = by * 8;
        for r in 0..8 {
            let dst_off = (base_y + r) * self.stride + base_x;
            let src_off = r * 8;
            self.data[dst_off..dst_off + 8].copy_from_slice(&block[src_off..src_off + 8]);
        }
    }
}

enum ScanStop {
    EarlyEoi,
    UnexpectedMarker(u8),
    UnexpectedEof,
    InvalidHuffman,
    InvalidCategory(u8),
}

fn scan_stop_to_error(stop: ScanStop) -> RsError {
    match stop {
        ScanStop::EarlyEoi => RsError::InvalidData("Unexpected early EOI in JPEG scan".into()),
        ScanStop::UnexpectedMarker(m) => {
            RsError::InvalidData(format!("Unexpected marker 0xFF{m:02X} in JPEG scan").into())
        }
        ScanStop::UnexpectedEof => {
            RsError::InvalidData("Truncated JPEG entropy-coded bitstream".into())
        }
        ScanStop::InvalidHuffman => {
            RsError::InvalidData("Invalid Huffman code in JPEG bitstream".into())
        }
        ScanStop::InvalidCategory(s) => {
            RsError::InvalidData(format!("Invalid JPEG coefficient category {s}").into())
        }
    }
}

/// MSB-first JPEG entropy bit reader with `0xFF 0x00` byte-unstuffing.
struct JpegBitReader<'a> {
    data: &'a [u8],
    pos: usize,
    bit_buf: u32,
    bit_count: u8,
    hit_eof: bool,
    hit_marker: Option<u8>,
}

impl<'a> JpegBitReader<'a> {
    fn new(data: &'a [u8], start_pos: usize) -> Self {
        Self {
            data,
            pos: start_pos,
            bit_buf: 0,
            bit_count: 0,
            hit_eof: false,
            hit_marker: None,
        }
    }

    fn fill_bits(&mut self) {
        while self.bit_count <= 24 && !self.hit_eof && self.hit_marker.is_none() {
            if self.pos >= self.data.len() {
                self.hit_eof = true;
                break;
            }
            let b = self.data[self.pos];
            if b == 0xFF {
                if self.pos + 1 >= self.data.len() {
                    self.hit_eof = true;
                    break;
                }
                let next = self.data[self.pos + 1];
                if next == 0x00 {
                    // Stuffed 0xFF data byte
                    self.pos += 2;
                    self.bit_buf |= 0xFFu32 << (24 - self.bit_count);
                    self.bit_count += 8;
                } else if next == 0xFF {
                    // Fill 0xFF bytes preceding a marker: leave self.pos at the last 0xFF
                    let mut p = self.pos + 1;
                    while p < self.data.len() && self.data[p] == 0xFF {
                        p += 1;
                    }
                    if p >= self.data.len() {
                        self.hit_eof = true;
                        break;
                    }
                    if self.data[p] == 0x00 {
                        self.hit_marker = Some(0x00);
                        break;
                    }
                    self.pos = p - 1;
                    self.hit_marker = Some(self.data[p]);
                    break;
                } else {
                    // Marker 0xFF <next> starts at self.pos; do not consume it here.
                    self.hit_marker = Some(next);
                    break;
                }
            } else {
                self.pos += 1;
                self.bit_buf |= u32::from(b) << (24 - self.bit_count);
                self.bit_count += 8;
            }
        }
    }

    fn no_more_bits_err(&self) -> ScanStop {
        match self.hit_marker {
            Some(0xD9) => ScanStop::EarlyEoi,
            Some(m) => ScanStop::UnexpectedMarker(m),
            None => ScanStop::UnexpectedEof,
        }
    }

    fn read_bit(&mut self) -> Result<u32, ScanStop> {
        if self.bit_count == 0 {
            self.fill_bits();
        }
        if self.bit_count == 0 {
            return Err(self.no_more_bits_err());
        }
        let bit = self.bit_buf >> 31;
        self.bit_buf <<= 1;
        self.bit_count -= 1;
        Ok(bit)
    }

    fn read_bits(&mut self, n: u8) -> Result<u32, ScanStop> {
        if n == 0 {
            return Ok(0);
        }
        if n > 16 {
            return Err(ScanStop::InvalidCategory(n));
        }
        if self.bit_count < n {
            self.fill_bits();
        }
        if self.bit_count < n {
            return Err(self.no_more_bits_err());
        }
        let val = self.bit_buf >> (32 - n);
        self.bit_buf <<= n;
        self.bit_count -= n;
        Ok(val)
    }

    fn decode_huffman(&mut self, table: &HuffmanTable) -> Result<u8, ScanStop> {
        if self.bit_count < 8 {
            self.fill_bits();
        }
        if self.bit_count > 0 {
            let top8 = (self.bit_buf >> 24) as usize;
            let (sym, len) = table.lut[top8];
            if len > 0 && len <= self.bit_count {
                self.bit_buf <<= len;
                self.bit_count -= len;
                return Ok(sym);
            }
        }

        let mut code: i32 = 0;
        for len in 1..=16 {
            code = (code << 1) | (self.read_bit()? as i32);
            if code <= table.maxcode[len] {
                if code < table.mincode[len] {
                    return Err(ScanStop::InvalidHuffman);
                }
                let idx = table.valptr[len] + (code - table.mincode[len]) as usize;
                return table
                    .values
                    .get(idx)
                    .copied()
                    .ok_or(ScanStop::InvalidHuffman);
            }
        }
        Err(ScanStop::InvalidHuffman)
    }

    fn receive_extend(&mut self, nbits: u8) -> Result<i32, ScanStop> {
        if nbits == 0 {
            return Ok(0);
        }
        if nbits > 15 {
            return Err(ScanStop::InvalidCategory(nbits));
        }
        let v = self.read_bits(nbits)? as i32;
        let vt = 1i32 << (nbits - 1);
        if v < vt {
            Ok(v - (1i32 << nbits) + 1)
        } else {
            Ok(v)
        }
    }

    fn consume_restart_marker(&mut self) -> RsResult<()> {
        let rem = self.bit_count % 8;
        if rem > 0 {
            self.bit_buf <<= rem;
            self.bit_count -= rem;
        }
        if self.bit_count > 0 {
            return Err(RsError::InvalidData(
                "Unexpected trailing entropy data before JPEG restart marker".into(),
            ));
        }
        if self.pos >= self.data.len() || self.data[self.pos] != 0xFF {
            return Err(RsError::InvalidData(
                "Missing JPEG RST marker at restart interval".into(),
            ));
        }
        while self.pos < self.data.len() && self.data[self.pos] == 0xFF {
            self.pos += 1;
        }
        if self.pos >= self.data.len() {
            return Err(RsError::InvalidData("Truncated JPEG restart marker".into()));
        }
        let marker = self.data[self.pos];
        self.pos += 1;
        if !(0xD0..=0xD7).contains(&marker) {
            return Err(RsError::InvalidData(
                format!("Expected JPEG RST0..RST7 marker, found 0xFF{marker:02X}").into(),
            ));
        }
        self.hit_marker = None;
        self.hit_eof = false;
        Ok(())
    }

    fn finish_scan_position(&self) -> usize {
        self.pos
    }
}

fn default_dc_tables() -> RsResult<[Option<HuffmanTable>; 4]> {
    Ok([
        Some(HuffmanTable::from_bits_vals(
            &STD_DC_LUM_BITS,
            &STD_DC_LUM_VALS,
        )?),
        Some(HuffmanTable::from_bits_vals(
            &STD_DC_CHR_BITS,
            &STD_DC_CHR_VALS,
        )?),
        None,
        None,
    ])
}

fn default_ac_tables() -> RsResult<[Option<HuffmanTable>; 4]> {
    Ok([
        Some(HuffmanTable::from_bits_vals(
            &STD_AC_LUM_BITS,
            &STD_AC_LUM_VALS,
        )?),
        Some(HuffmanTable::from_bits_vals(
            &STD_AC_CHR_BITS,
            &STD_AC_CHR_VALS,
        )?),
        None,
        None,
    ])
}

fn parse_dqt(payload: &[u8], qt_tables: &mut [Option<[i32; 64]>; 4]) -> RsResult<()> {
    if payload.is_empty() {
        return Err(RsError::InvalidData("Empty JPEG DQT segment".into()));
    }
    let mut pos = 0usize;
    while pos < payload.len() {
        let pq_tq = payload[pos];
        pos += 1;
        let precision = pq_tq >> 4;
        let table_id = usize::from(pq_tq & 0x0F);
        if table_id >= 4 {
            return Err(RsError::InvalidData(
                format!("Invalid JPEG DQT table id {table_id}").into(),
            ));
        }
        let mut table = [0i32; 64];
        match precision {
            0 => {
                if pos + 64 > payload.len() {
                    return Err(RsError::InvalidData(
                        "Truncated 8-bit JPEG DQT table".into(),
                    ));
                }
                for (dst, &src) in table.iter_mut().zip(&payload[pos..pos + 64]) {
                    *dst = i32::from(src);
                }
                pos += 64;
            }
            1 => {
                if pos + 128 > payload.len() {
                    return Err(RsError::InvalidData(
                        "Truncated 16-bit JPEG DQT table".into(),
                    ));
                }
                for (k, dst) in table.iter_mut().enumerate() {
                    let off = pos + k * 2;
                    *dst = i32::from(u16::from_be_bytes([payload[off], payload[off + 1]]));
                }
                pos += 128;
            }
            _ => {
                return Err(RsError::InvalidData(
                    format!("Invalid JPEG DQT precision {precision}").into(),
                ));
            }
        }
        qt_tables[table_id] = Some(table);
    }
    Ok(())
}

fn parse_dht(
    payload: &[u8],
    dc_tables: &mut [Option<HuffmanTable>; 4],
    ac_tables: &mut [Option<HuffmanTable>; 4],
) -> RsResult<()> {
    if payload.is_empty() {
        return Err(RsError::InvalidData("Empty JPEG DHT segment".into()));
    }
    let mut pos = 0usize;
    while pos < payload.len() {
        let tc_th = payload[pos];
        pos += 1;
        let table_class = tc_th >> 4;
        let table_id = usize::from(tc_th & 0x0F);
        if table_class > 1 || table_id >= 4 {
            return Err(RsError::InvalidData(
                format!("Invalid JPEG DHT class={table_class} id={table_id}").into(),
            ));
        }
        if pos + 16 > payload.len() {
            return Err(RsError::InvalidData(
                "Truncated JPEG DHT code length counts".into(),
            ));
        }
        let mut counts = [0u8; 16];
        counts.copy_from_slice(&payload[pos..pos + 16]);
        pos += 16;

        let total: usize = counts.iter().map(|&c| usize::from(c)).sum();
        if total > 256 {
            return Err(RsError::InvalidData(
                format!("JPEG DHT symbol count {total} exceeds 256").into(),
            ));
        }
        if pos + total > payload.len() {
            return Err(RsError::InvalidData(
                "Truncated JPEG DHT symbol values".into(),
            ));
        }
        let vals = &payload[pos..pos + total];
        pos += total;

        let table = HuffmanTable::from_bits_vals(&counts, vals)?;
        if table_class == 0 {
            dc_tables[table_id] = Some(table);
        } else {
            ac_tables[table_id] = Some(table);
        }
    }
    Ok(())
}

fn parse_sof(payload: &[u8]) -> RsResult<FrameHeader> {
    if payload.len() < 6 {
        return Err(RsError::InvalidData("Truncated JPEG SOF0 header".into()));
    }
    let precision = payload[0];
    if precision != 8 {
        return Err(RsError::Unsupported(
            format!(
                "Unsupported JPEG sample precision {precision} (only 8-bit Baseline/Extended sequential supported)"
            )
            .into(),
        ));
    }
    let height = usize::from(u16::from_be_bytes([payload[1], payload[2]]));
    let width = usize::from(u16::from_be_bytes([payload[3], payload[4]]));
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(RsError::InvalidData(
            format!("Invalid JPEG dimensions {width}x{height}").into(),
        ));
    }
    let num_comp = usize::from(payload[5]);
    if num_comp != 1 && num_comp != 3 {
        return Err(RsError::Unsupported(
            format!("Unsupported JPEG component count {num_comp} (only 1 or 3 supported)").into(),
        ));
    }
    if payload.len() != 6 + num_comp * 3 {
        return Err(RsError::InvalidData(
            "Invalid JPEG SOF0 component specification length".into(),
        ));
    }

    let mut components = Vec::with_capacity(num_comp);
    let mut max_h = 0usize;
    let mut max_v = 0usize;
    for i in 0..num_comp {
        let base = 6 + i * 3;
        let id = payload[base];
        if components.iter().any(|c: &ComponentInfo| c.id == id) {
            return Err(RsError::InvalidData(
                format!("Duplicate JPEG component id {id}").into(),
            ));
        }
        let hv = payload[base + 1];
        let h_samp = usize::from(hv >> 4);
        let v_samp = usize::from(hv & 0x0F);
        if !(1..=4).contains(&h_samp) || !(1..=4).contains(&v_samp) {
            return Err(RsError::InvalidData(
                format!("Invalid JPEG sampling factor {h_samp}x{v_samp}").into(),
            ));
        }
        let qt_id = usize::from(payload[base + 2]);
        if qt_id >= 4 {
            return Err(RsError::InvalidData(
                format!("Invalid JPEG component quantization table id {qt_id}").into(),
            ));
        }
        max_h = max_h.max(h_samp);
        max_v = max_v.max(v_samp);
        components.push(ComponentInfo {
            id,
            h_samp,
            v_samp,
            qt_id,
        });
    }

    let mcu_w = max_h * 8;
    let mcu_h = max_v * 8;
    let mcu_cols = width.div_ceil(mcu_w);
    let mcu_rows = height.div_ceil(mcu_h);

    Ok(FrameHeader {
        width,
        height,
        components,
        max_h,
        max_v,
        mcu_cols,
        mcu_rows,
    })
}

fn parse_sos_header(
    payload: &[u8],
    frame: &FrameHeader,
    qt_tables: &[Option<[i32; 64]>; 4],
    dc_tables: &[Option<HuffmanTable>; 4],
    ac_tables: &[Option<HuffmanTable>; 4],
) -> RsResult<Vec<ScanComponent>> {
    if payload.is_empty() {
        return Err(RsError::InvalidData("Empty JPEG SOS header".into()));
    }
    let ns = usize::from(payload[0]);
    if ns == 0 || ns > frame.components.len() {
        return Err(RsError::InvalidData(
            format!("Invalid JPEG SOS component count {ns}").into(),
        ));
    }
    if payload.len() != 1 + ns * 2 + 3 {
        return Err(RsError::InvalidData(
            "Invalid JPEG SOS header length".into(),
        ));
    }

    let mut scan_comps = Vec::with_capacity(ns);
    for i in 0..ns {
        let cs = payload[1 + i * 2];
        let td_ta = payload[1 + i * 2 + 1];
        let Some(comp_idx) = frame.components.iter().position(|c| c.id == cs) else {
            return Err(RsError::InvalidData(
                format!("JPEG SOS references unknown component id {cs}").into(),
            ));
        };
        if scan_comps
            .iter()
            .any(|sc: &ScanComponent| sc.comp_idx == comp_idx)
        {
            return Err(RsError::InvalidData(
                format!("Duplicate component id {cs} in JPEG SOS").into(),
            ));
        }

        let qt_id = frame.components[comp_idx].qt_id;
        if qt_tables[qt_id].is_none() {
            return Err(RsError::InvalidData(
                format!("Missing JPEG quantization table {qt_id}").into(),
            ));
        }

        let dc_table_id = usize::from(td_ta >> 4);
        let ac_table_id = usize::from(td_ta & 0x0F);
        if dc_table_id >= 4 || ac_table_id >= 4 {
            return Err(RsError::InvalidData(
                format!(
                    "Invalid JPEG SOS Huffman table selector dc={dc_table_id} ac={ac_table_id}"
                )
                .into(),
            ));
        }
        if dc_tables[dc_table_id].is_none() {
            return Err(RsError::InvalidData(
                format!("Missing JPEG DC Huffman table {dc_table_id}").into(),
            ));
        }
        if ac_tables[ac_table_id].is_none() {
            return Err(RsError::InvalidData(
                format!("Missing JPEG AC Huffman table {ac_table_id}").into(),
            ));
        }

        scan_comps.push(ScanComponent {
            comp_idx,
            dc_table_id,
            ac_table_id,
        });
    }

    let tail = 1 + ns * 2;
    let ss = payload[tail];
    let se = payload[tail + 1];
    let ah_al = payload[tail + 2];
    if ss != 0 || se != 63 || ah_al != 0 {
        return Err(RsError::Unsupported(
            format!(
                "Non-baseline JPEG SOS parameters (Ss={ss}, Se={se}, AhAl=0x{ah_al:02X}) are not supported"
            )
            .into(),
        ));
    }

    Ok(scan_comps)
}

// ── Fixed-point integer IDCT (ITU-T T.81 A.3.3, Loeffler/Ligtenberg/Moschytz) ──
//
// Separable 2D 8x8 IDCT using the well-known 13-bit fixed-point butterfly
// (the "islow" factorisation used by the IJG reference decoder). Column pass
// first, keeping `IDCT_PASS1_BITS` of extra precision in the workspace, then
// the row pass descales, level-shifts by +128 and clamps to `0..=255`.
// Accuracy is within ±1 LSB of the exact floating-point IDCT (IEEE 1180).

const IDCT_CONST_BITS: u32 = 13;
const IDCT_PASS1_BITS: u32 = 2;

const FIX_0_298631336: i64 = 2446;
const FIX_0_390180644: i64 = 3196;
const FIX_0_541196100: i64 = 4433;
const FIX_0_765366865: i64 = 6270;
const FIX_0_899976223: i64 = 7373;
const FIX_1_175875602: i64 = 9633;
const FIX_1_501321110: i64 = 12299;
const FIX_1_847759065: i64 = 15137;
const FIX_1_961570560: i64 = 16069;
const FIX_2_053119869: i64 = 16819;
const FIX_2_562915447: i64 = 20995;
const FIX_3_072711026: i64 = 25172;

/// Dequantized coefficients are clamped to this magnitude before the IDCT.
/// Legal 8-bit JPEG data never exceeds ±2^14 after dequantization, so this
/// only bounds hostile 16-bit DQT products and keeps every intermediate value
/// comfortably inside `i64`.
const MAX_DEQUANT_COEFF: i32 = 1 << 16;

/// Rounding arithmetic right shift (`x / 2^n`, rounded half up).
#[inline(always)]
fn descale(x: i64, n: u32) -> i64 {
    (x + (1i64 << (n - 1))) >> n
}

#[inline(always)]
fn clamp_u8(v: i64) -> u8 {
    v.clamp(0, 255) as u8
}

/// One 1D 8-point IDCT butterfly. Returns the eight outputs scaled by
/// `2^IDCT_CONST_BITS` (not yet descaled).
#[inline(always)]
fn idct_1d(c: [i64; 8]) -> [i64; 8] {
    // Even part.
    let z2 = c[2];
    let z3 = c[6];
    let z1 = (z2 + z3) * FIX_0_541196100;
    let tmp2 = z1 - z3 * FIX_1_847759065;
    let tmp3 = z1 + z2 * FIX_0_765366865;

    let tmp0 = (c[0] + c[4]) << IDCT_CONST_BITS;
    let tmp1 = (c[0] - c[4]) << IDCT_CONST_BITS;

    let tmp10 = tmp0 + tmp3;
    let tmp13 = tmp0 - tmp3;
    let tmp11 = tmp1 + tmp2;
    let tmp12 = tmp1 - tmp2;

    // Odd part.
    let mut t0 = c[7];
    let mut t1 = c[5];
    let mut t2 = c[3];
    let mut t3 = c[1];

    let mut z1 = t0 + t3;
    let mut z2 = t1 + t2;
    let mut z3 = t0 + t2;
    let mut z4 = t1 + t3;
    let z5 = (z3 + z4) * FIX_1_175875602;

    t0 *= FIX_0_298631336;
    t1 *= FIX_2_053119869;
    t2 *= FIX_3_072711026;
    t3 *= FIX_1_501321110;
    z1 *= -FIX_0_899976223;
    z2 *= -FIX_2_562915447;
    z3 *= -FIX_1_961570560;
    z4 *= -FIX_0_390180644;

    z3 += z5;
    z4 += z5;

    t0 += z1 + z3;
    t1 += z2 + z4;
    t2 += z2 + z3;
    t3 += z1 + z4;

    [
        tmp10 + t3,
        tmp11 + t2,
        tmp12 + t1,
        tmp13 + t0,
        tmp13 - t0,
        tmp12 - t1,
        tmp11 - t2,
        tmp10 - t3,
    ]
}

/// Separable 2D 8x8 fixed-point integer IDCT on dequantized coefficients in
/// natural (row-major) order, with level-shift `+128` and clamping to `0..=255`.
fn idct_8x8_int(coeff: &[i32; 64], out: &mut [u8; 64]) {
    // DC-only fast path: every output pixel is DC/8 + 128.
    if coeff[1..].iter().all(|&c| c == 0) {
        let dc = i64::from(coeff[0].clamp(-MAX_DEQUANT_COEFF, MAX_DEQUANT_COEFF));
        out.fill(clamp_u8(descale(dc, 3) + 128));
        return;
    }

    let mut ws = [0i64; 64];

    // Pass 1: columns, results scaled up by 2^IDCT_PASS1_BITS.
    for col in 0..8 {
        let mut c = [0i64; 8];
        for (row, slot) in c.iter_mut().enumerate() {
            *slot = i64::from(coeff[row * 8 + col].clamp(-MAX_DEQUANT_COEFF, MAX_DEQUANT_COEFF));
        }
        if c[1..].iter().all(|&v| v == 0) {
            // Column AC-zero fast path: the column is flat.
            let dc = c[0] << IDCT_PASS1_BITS;
            for row in 0..8 {
                ws[row * 8 + col] = dc;
            }
            continue;
        }
        let r = idct_1d(c);
        for (row, &v) in r.iter().enumerate() {
            ws[row * 8 + col] = descale(v, IDCT_CONST_BITS - IDCT_PASS1_BITS);
        }
    }

    // Pass 2: rows, final descale by 2^(CONST_BITS + PASS1_BITS + 3)
    // (the +3 is the 1/8 normalisation of the 2D transform).
    const FINAL_SHIFT: u32 = IDCT_CONST_BITS + IDCT_PASS1_BITS + 3;
    for row in 0..8 {
        let base = row * 8;
        let mut c = [0i64; 8];
        c.copy_from_slice(&ws[base..base + 8]);
        let dst = &mut out[base..base + 8];
        if c[1..].iter().all(|&v| v == 0) {
            // Row AC-zero fast path.
            dst.fill(clamp_u8(descale(c[0], IDCT_PASS1_BITS + 3) + 128));
            continue;
        }
        let r = idct_1d(c);
        for (d, &v) in dst.iter_mut().zip(r.iter()) {
            *d = clamp_u8(descale(v, FINAL_SHIFT) + 128);
        }
    }
}

struct BlockTables<'a> {
    dc_table: &'a HuffmanTable,
    ac_table: &'a HuffmanTable,
    qt: &'a [i32; 64],
}

enum BlockDecodeOutcome {
    Complete([u8; 64]),
    PartialBeforeEoi([u8; 64]),
    EoiBeforeBlock,
}

fn decode_block(
    bit_reader: &mut JpegBitReader<'_>,
    dc_pred: &mut i32,
    tables: &BlockTables<'_>,
) -> RsResult<BlockDecodeOutcome> {
    let dc_cat = match bit_reader.decode_huffman(tables.dc_table) {
        Ok(cat) => cat,
        Err(ScanStop::EarlyEoi) => return Ok(BlockDecodeOutcome::EoiBeforeBlock),
        Err(stop) => return Err(scan_stop_to_error(stop)),
    };
    let dc_diff = match bit_reader.receive_extend(dc_cat) {
        Ok(diff) => diff,
        Err(ScanStop::EarlyEoi) => return Ok(BlockDecodeOutcome::EoiBeforeBlock),
        Err(stop) => return Err(scan_stop_to_error(stop)),
    };
    *dc_pred = dc_pred.wrapping_add(dc_diff);

    let mut zz = [0i32; 64];
    zz[0] = *dc_pred;

    let mut hit_early_eoi = false;
    let mut k = 1usize;
    while k < 64 {
        let rs = match bit_reader.decode_huffman(tables.ac_table) {
            Ok(sym) => sym,
            Err(ScanStop::EarlyEoi) => {
                hit_early_eoi = true;
                break;
            }
            Err(stop) => return Err(scan_stop_to_error(stop)),
        };
        if rs == 0x00 {
            break;
        }
        let run = usize::from(rs >> 4);
        let size = rs & 0x0F;
        if size == 0 {
            if rs == 0xF0 {
                if k + 16 > 64 {
                    return Err(RsError::InvalidData(
                        "JPEG AC ZRL exceeds 64-coefficient block".into(),
                    ));
                }
                k += 16;
                continue;
            }
            return Err(RsError::InvalidData(
                format!("Invalid JPEG AC Huffman symbol 0x{rs:02X}").into(),
            ));
        }

        k += run;
        if k >= 64 {
            return Err(RsError::InvalidData(
                "JPEG AC run-length exceeds 64-coefficient block".into(),
            ));
        }
        let val = match bit_reader.receive_extend(size) {
            Ok(v) => v,
            Err(ScanStop::EarlyEoi) => {
                hit_early_eoi = true;
                break;
            }
            Err(stop) => return Err(scan_stop_to_error(stop)),
        };
        zz[k] = val;
        k += 1;
    }

    let mut dequant = [0i32; 64];
    for (idx, (&coeff, &q)) in zz.iter().zip(tables.qt.iter()).enumerate() {
        dequant[ZIGZAG[idx]] = coeff.saturating_mul(q);
    }

    let mut pixels = [0u8; 64];
    idct_8x8_int(&dequant, &mut pixels);
    if hit_early_eoi {
        Ok(BlockDecodeOutcome::PartialBeforeEoi(pixels))
    } else {
        Ok(BlockDecodeOutcome::Complete(pixels))
    }
}

struct ScanDecodeCtx<'a> {
    frame: &'a FrameHeader,
    scan_comps: &'a [ScanComponent],
    qt_tables: &'a [Option<[i32; 64]>; 4],
    dc_tables: &'a [Option<HuffmanTable>; 4],
    ac_tables: &'a [Option<HuffmanTable>; 4],
    restart_interval: usize,
}

fn expected_scan_blocks(frame: &FrameHeader, scan_comps: &[ScanComponent]) -> usize {
    if scan_comps.len() == 1 {
        let comp = &frame.components[scan_comps[0].comp_idx];
        let cw = (frame.width * comp.h_samp).div_ceil(frame.max_h);
        let ch = (frame.height * comp.v_samp).div_ceil(frame.max_v);
        let blocks_x = cw.div_ceil(8);
        let blocks_y = ch.div_ceil(8);
        blocks_x.saturating_mul(blocks_y)
    } else {
        let blocks_per_mcu: usize = scan_comps
            .iter()
            .map(|sc| {
                let c = &frame.components[sc.comp_idx];
                c.h_samp * c.v_samp
            })
            .sum();
        frame
            .mcu_cols
            .saturating_mul(frame.mcu_rows)
            .saturating_mul(blocks_per_mcu)
    }
}

fn decode_scan(
    data: &[u8],
    scan_start: usize,
    ctx: &ScanDecodeCtx<'_>,
    planes: &mut [CompPlane],
) -> RsResult<usize> {
    let mut bit_reader = JpegBitReader::new(data, scan_start);
    let mut dc_preds = [0i32; 4];

    if ctx.scan_comps.len() == 1 {
        let sc = ctx.scan_comps[0];
        let comp = &ctx.frame.components[sc.comp_idx];
        let cw = (ctx.frame.width * comp.h_samp).div_ceil(ctx.frame.max_h);
        let ch = (ctx.frame.height * comp.v_samp).div_ceil(ctx.frame.max_v);
        let blocks_x = cw.div_ceil(8);
        let blocks_y = ch.div_ceil(8);

        let tables = BlockTables {
            dc_table: ctx.dc_tables[sc.dc_table_id]
                .as_ref()
                .ok_or_else(|| RsError::InvalidData("Missing JPEG DC table".into()))?,
            ac_table: ctx.ac_tables[sc.ac_table_id]
                .as_ref()
                .ok_or_else(|| RsError::InvalidData("Missing JPEG AC table".into()))?,
            qt: ctx.qt_tables[comp.qt_id]
                .as_ref()
                .ok_or_else(|| RsError::InvalidData("Missing JPEG quantization table".into()))?,
        };

        let mut mcu_idx = 0usize;
        'non_interleaved: for by in 0..blocks_y {
            for bx in 0..blocks_x {
                if ctx.restart_interval > 0 && mcu_idx > 0 && mcu_idx % ctx.restart_interval == 0 {
                    bit_reader.consume_restart_marker()?;
                    dc_preds[0] = 0;
                }
                match decode_block(&mut bit_reader, &mut dc_preds[0], &tables)? {
                    BlockDecodeOutcome::Complete(pixels) => {
                        planes[sc.comp_idx].write_block(bx, by, &pixels);
                    }
                    BlockDecodeOutcome::PartialBeforeEoi(pixels) => {
                        planes[sc.comp_idx].write_block(bx, by, &pixels);
                        break 'non_interleaved;
                    }
                    BlockDecodeOutcome::EoiBeforeBlock => {
                        break 'non_interleaved;
                    }
                }
                mcu_idx += 1;
            }
        }
    } else {
        let mut mcu_idx = 0usize;
        'interleaved: for mcu_y in 0..ctx.frame.mcu_rows {
            for mcu_x in 0..ctx.frame.mcu_cols {
                if ctx.restart_interval > 0 && mcu_idx > 0 && mcu_idx % ctx.restart_interval == 0 {
                    bit_reader.consume_restart_marker()?;
                    dc_preds = [0i32; 4];
                }

                for sc in ctx.scan_comps {
                    let comp = &ctx.frame.components[sc.comp_idx];
                    let tables = BlockTables {
                        dc_table: ctx.dc_tables[sc.dc_table_id]
                            .as_ref()
                            .ok_or_else(|| RsError::InvalidData("Missing JPEG DC table".into()))?,
                        ac_table: ctx.ac_tables[sc.ac_table_id]
                            .as_ref()
                            .ok_or_else(|| RsError::InvalidData("Missing JPEG AC table".into()))?,
                        qt: ctx.qt_tables[comp.qt_id].as_ref().ok_or_else(|| {
                            RsError::InvalidData("Missing JPEG quantization table".into())
                        })?,
                    };

                    for by in 0..comp.v_samp {
                        for bx in 0..comp.h_samp {
                            let plane_bx = mcu_x * comp.h_samp + bx;
                            let plane_by = mcu_y * comp.v_samp + by;
                            match decode_block(
                                &mut bit_reader,
                                &mut dc_preds[sc.comp_idx],
                                &tables,
                            )? {
                                BlockDecodeOutcome::Complete(pixels) => {
                                    planes[sc.comp_idx].write_block(plane_bx, plane_by, &pixels);
                                }
                                BlockDecodeOutcome::PartialBeforeEoi(pixels) => {
                                    planes[sc.comp_idx].write_block(plane_bx, plane_by, &pixels);
                                    break 'interleaved;
                                }
                                BlockDecodeOutcome::EoiBeforeBlock => {
                                    break 'interleaved;
                                }
                            }
                        }
                    }
                }
                mcu_idx += 1;
            }
        }
    }

    Ok(bit_reader.finish_scan_position())
}

/// JFIF full-range YCbCr -> RGB in 16-bit fixed point:
///
/// ```text
/// R = Y + 1.402    * Cr'
/// G = Y - 0.344136 * Cb' - 0.714136 * Cr'
/// B = Y + 1.772    * Cb'
/// ```
///
/// with `Cb' = Cb - 128`, `Cr' = Cr - 128`, constants scaled by `2^16` and
/// rounded (`+ 32768`) before the shift.
#[inline(always)]
fn ycbcr_to_rgb(y: u8, cb: u8, cr: u8) -> [u8; 3] {
    let y = i32::from(y);
    let cb = i32::from(cb) - 128;
    let cr = i32::from(cr) - 128;
    let r = y + ((91_881 * cr + 32_768) >> 16);
    let g = y - ((22_554 * cb + 46_802 * cr + 32_768) >> 16);
    let b = y + ((116_130 * cb + 32_768) >> 16);
    [
        r.clamp(0, 255) as u8,
        g.clamp(0, 255) as u8,
        b.clamp(0, 255) as u8,
    ]
}

fn assemble_rgb24(frame: &FrameHeader, planes: &[CompPlane]) -> RsResult<Vec<u8>> {
    let width = frame.width;
    let height = frame.height;
    let total_bytes = width
        .checked_mul(height)
        .and_then(|px| px.checked_mul(3))
        .ok_or_else(|| RsError::InvalidData("JPEG frame size overflow".into()))?;
    let mut rgb = vec![0u8; total_bytes];
    let row_bytes = width * 3;

    if frame.components.len() == 1 {
        // A single component always has the maximum sampling factors, so the
        // plane maps 1:1 onto output pixels.
        let y_plane = &planes[0];
        for (y, dst_row) in rgb.chunks_exact_mut(row_bytes).enumerate() {
            let src = &y_plane.data[y * y_plane.stride..y * y_plane.stride + width];
            for (dst, &v) in dst_row.chunks_exact_mut(3).zip(src) {
                dst[0] = v;
                dst[1] = v;
                dst[2] = v;
            }
        }
        return Ok(rgb);
    }

    let y_comp = &frame.components[0];
    let cb_comp = &frame.components[1];
    let cr_comp = &frame.components[2];
    let y_plane = &planes[0];
    let cb_plane = &planes[1];
    let cr_plane = &planes[2];

    let luma_full = y_comp.h_samp == frame.max_h && y_comp.v_samp == frame.max_v;
    let chroma_1x1 =
        cb_comp.h_samp == 1 && cb_comp.v_samp == 1 && cr_comp.h_samp == 1 && cr_comp.v_samp == 1;

    if luma_full && chroma_1x1 && frame.max_h <= 2 && frame.max_v <= 2 && frame.max_h >= frame.max_v
    {
        // Fast paths: 4:4:4 (1x1), 4:2:2 (2x1) and 4:2:0 (2x2). Chroma
        // coordinates are plain shifts of the luma coordinates — no divisions.
        let h_shift = u32::from(frame.max_h == 2);
        let v_shift = u32::from(frame.max_v == 2);
        for (y, dst_row) in rgb.chunks_exact_mut(row_bytes).enumerate() {
            let y_row = &y_plane.data[y * y_plane.stride..y * y_plane.stride + width];
            let cy = y >> v_shift;
            let cb_row = &cb_plane.data[cy * cb_plane.stride..];
            let cr_row = &cr_plane.data[cy * cr_plane.stride..];
            if h_shift == 0 {
                // 4:4:4
                for (((dst, &yv), &cb), &cr) in dst_row
                    .chunks_exact_mut(3)
                    .zip(y_row)
                    .zip(&cb_row[..width])
                    .zip(&cr_row[..width])
                {
                    dst.copy_from_slice(&ycbcr_to_rgb(yv, cb, cr));
                }
            } else {
                // 4:2:2 / 4:2:0: each chroma sample covers two luma columns.
                for (x, (dst, &yv)) in dst_row.chunks_exact_mut(3).zip(y_row).enumerate() {
                    let cx = x >> 1;
                    dst.copy_from_slice(&ycbcr_to_rgb(yv, cb_row[cx], cr_row[cx]));
                }
            }
        }
        return Ok(rgb);
    }

    // General integer fallback for any other sampling ratio (4:1:1, 4:4:0,
    // subsampled luma, ...).
    for (y, dst_row) in rgb.chunks_exact_mut(row_bytes).enumerate() {
        let yy = y * y_comp.v_samp / frame.max_v;
        let cby = y * cb_comp.v_samp / frame.max_v;
        let cry = y * cr_comp.v_samp / frame.max_v;
        let y_row = yy * y_plane.stride;
        let cb_row = cby * cb_plane.stride;
        let cr_row = cry * cr_plane.stride;

        for (x, dst) in dst_row.chunks_exact_mut(3).enumerate() {
            let yx = x * y_comp.h_samp / frame.max_h;
            let cbx = x * cb_comp.h_samp / frame.max_h;
            let crx = x * cr_comp.h_samp / frame.max_h;
            dst.copy_from_slice(&ycbcr_to_rgb(
                y_plane.data[y_row + yx],
                cb_plane.data[cb_row + cbx],
                cr_plane.data[cr_row + crx],
            ));
        }
    }

    Ok(rgb)
}

fn decode_baseline_jpeg(data: &[u8]) -> RsResult<DecodedImage> {
    if data.len() < 2 || data[0] != 0xFF || data[1] != 0xD8 {
        return Err(RsError::InvalidData("Missing JPEG SOI marker".into()));
    }

    let mut pos = 2usize;
    let mut qt_tables: [Option<[i32; 64]>; 4] = [None, None, None, None];
    let mut dc_tables = default_dc_tables()?;
    let mut ac_tables = default_ac_tables()?;
    let mut frame_header: Option<FrameHeader> = None;
    let mut planes: Option<Vec<CompPlane>> = None;
    let mut restart_interval: usize = 0;
    let mut decoded_scan = false;
    let mut saw_eoi = false;

    while pos < data.len() {
        if data[pos] != 0xFF {
            return Err(RsError::InvalidData(
                format!("Expected JPEG marker prefix 0xFF at offset {pos}").into(),
            ));
        }
        while pos < data.len() && data[pos] == 0xFF {
            pos += 1;
        }
        if pos >= data.len() {
            return Err(RsError::InvalidData("Truncated JPEG marker".into()));
        }
        let marker = data[pos];
        pos += 1;

        match marker {
            0x00 => {
                return Err(RsError::InvalidData(
                    "Unexpected 0xFF00 byte stuffing outside entropy scan".into(),
                ));
            }
            0xD8 => {
                return Err(RsError::InvalidData(
                    "Unexpected duplicate JPEG SOI marker".into(),
                ));
            }
            0xD9 => {
                saw_eoi = true;
                break;
            }
            // Standalone markers without segment length
            0x01 | 0xD0..=0xD7 => {}
            0xC2 => {
                return Err(RsError::Unsupported(
                    "Progressive JPEG (SOF2) is not supported".into(),
                ));
            }
            0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => {
                return Err(RsError::Unsupported(
                    format!("Unsupported JPEG SOF marker 0xFF{marker:02X}").into(),
                ));
            }
            _ => {
                if pos + 2 > data.len() {
                    return Err(RsError::InvalidData(
                        "Truncated JPEG marker segment length".into(),
                    ));
                }
                let seg_len = usize::from(u16::from_be_bytes([data[pos], data[pos + 1]]));
                if seg_len < 2 || pos + seg_len > data.len() {
                    return Err(RsError::InvalidData(
                        "Invalid or truncated JPEG marker segment".into(),
                    ));
                }
                let seg_payload = &data[pos + 2..pos + seg_len];
                pos += seg_len;

                match marker {
                    0xDB => parse_dqt(seg_payload, &mut qt_tables)?,
                    0xC4 => parse_dht(seg_payload, &mut dc_tables, &mut ac_tables)?,
                    // SOF0 (Baseline DCT) and SOF1 (Extended Sequential DCT,
                    // Huffman). With 8-bit precision and Ss=0/Se=63/AhAl=0
                    // scans (enforced by `parse_sof` / `parse_sos_header`) the
                    // two decode identically.
                    0xC0 | 0xC1 => {
                        if frame_header.is_some() {
                            return Err(RsError::InvalidData("Duplicate JPEG SOF marker".into()));
                        }
                        frame_header = Some(parse_sof(seg_payload)?);
                    }
                    0xDD => {
                        if seg_payload.len() != 2 {
                            return Err(RsError::InvalidData(
                                "Invalid JPEG DRI segment length".into(),
                            ));
                        }
                        restart_interval =
                            usize::from(u16::from_be_bytes([seg_payload[0], seg_payload[1]]));
                    }
                    0xDA => {
                        let frame = frame_header.as_ref().ok_or_else(|| {
                            RsError::InvalidData("JPEG SOS marker encountered before SOF0".into())
                        })?;
                        let scan_comps = parse_sos_header(
                            seg_payload,
                            frame,
                            &qt_tables,
                            &dc_tables,
                            &ac_tables,
                        )?;
                        let total_blocks = expected_scan_blocks(frame, &scan_comps);
                        let remaining_bytes = data.len().saturating_sub(pos);
                        if total_blocks > 4096 && remaining_bytes.saturating_mul(8) < total_blocks {
                            return Err(RsError::InvalidData(
                                "JPEG scan data too small for declared image dimensions".into(),
                            ));
                        }
                        if planes.is_none() {
                            let mut allocated = Vec::with_capacity(frame.components.len());
                            for c in &frame.components {
                                allocated.push(CompPlane::try_new(
                                    frame.mcu_cols * c.h_samp,
                                    frame.mcu_rows * c.v_samp,
                                )?);
                            }
                            planes = Some(allocated);
                        }
                        let comp_planes = planes.as_mut().ok_or_else(|| {
                            RsError::Bug("JPEG component planes uninitialized".into())
                        })?;
                        let scan_ctx = ScanDecodeCtx {
                            frame,
                            scan_comps: &scan_comps,
                            qt_tables: &qt_tables,
                            dc_tables: &dc_tables,
                            ac_tables: &ac_tables,
                            restart_interval,
                        };
                        pos = decode_scan(data, pos, &scan_ctx, comp_planes)?;
                        decoded_scan = true;
                    }
                    // APPn (0xFFE0..=0xFFEF), COM (0xFFFE), and other metadata markers: skip cleanly
                    _ => {}
                }
            }
        }
    }

    if !saw_eoi {
        return Err(RsError::InvalidData("Missing JPEG EOI marker".into()));
    }
    if !decoded_scan {
        return Err(RsError::InvalidData(
            "JPEG contains no image scan data".into(),
        ));
    }

    let frame = frame_header
        .as_ref()
        .ok_or_else(|| RsError::InvalidData("Missing JPEG SOF0 header".into()))?;
    let comp_planes = planes
        .as_ref()
        .ok_or_else(|| RsError::InvalidData("Missing decoded JPEG planes".into()))?;
    let rgb24 = assemble_rgb24(frame, comp_planes)?;

    Ok(DecodedImage {
        width: frame.width,
        height: frame.height,
        rgb24,
    })
}

// ── Unit Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::PacketFlags;
    use bytes::Bytes;
    use rsmpeg_util::Rational;
    use std::f64::consts::{FRAC_1_SQRT_2, PI};

    #[derive(Clone, Copy)]
    struct HuffEncEntry {
        code: u16,
        len: u8,
    }

    fn build_encoder_huff(counts: &[u8; 16], vals: &[u8]) -> Vec<HuffEncEntry> {
        let mut table = vec![HuffEncEntry { code: 0, len: 0 }; 256];
        let mut code: u32 = 0;
        let mut idx: usize = 0;
        for len in 1..=16 {
            let count = usize::from(counts[len - 1]);
            for _ in 0..count {
                let sym = vals[idx] as usize;
                table[sym] = HuffEncEntry {
                    code: code as u16,
                    len: len as u8,
                };
                code += 1;
                idx += 1;
            }
            code <<= 1;
        }
        table
    }

    struct JpegBitWriter {
        out: Vec<u8>,
        bit_buf: u32,
        bit_count: u8,
    }

    impl JpegBitWriter {
        fn new() -> Self {
            Self {
                out: Vec::new(),
                bit_buf: 0,
                bit_count: 0,
            }
        }

        fn put_bits(&mut self, bits: u16, len: u8) {
            if len == 0 {
                return;
            }
            let mask = (1u32 << len) - 1;
            self.bit_buf = (self.bit_buf << len) | (u32::from(bits) & mask);
            self.bit_count += len;
            while self.bit_count >= 8 {
                let shift = self.bit_count - 8;
                let byte = ((self.bit_buf >> shift) & 0xFF) as u8;
                self.out.push(byte);
                if byte == 0xFF {
                    self.out.push(0x00);
                }
                self.bit_count -= 8;
            }
        }

        fn flush_padded(&mut self) {
            if self.bit_count > 0 {
                let pad = 8 - self.bit_count;
                self.put_bits((1u16 << pad) - 1, pad);
            }
        }
    }

    fn fdct_8x8(samples: &[u8; 64]) -> [f64; 64] {
        let mut shifted = [0.0f64; 64];
        for (dst, &src) in shifted.iter_mut().zip(samples.iter()) {
            *dst = f64::from(src) - 128.0;
        }
        let mut out = [0.0f64; 64];
        for v in 0..8 {
            let cv = if v == 0 { FRAC_1_SQRT_2 } else { 1.0 };
            for u in 0..8 {
                let cu = if u == 0 { FRAC_1_SQRT_2 } else { 1.0 };
                let mut sum = 0.0;
                for y in 0..8 {
                    let cy = ((2 * y + 1) as f64 * v as f64 * PI / 16.0).cos();
                    for x in 0..8 {
                        let cx = ((2 * x + 1) as f64 * u as f64 * PI / 16.0).cos();
                        sum += shifted[y * 8 + x] * cx * cy;
                    }
                }
                out[v * 8 + u] = 0.25 * cu * cv * sum;
            }
        }
        out
    }

    fn encode_coeff_bits(val: i32) -> (u8, u16) {
        if val == 0 {
            return (0, 0);
        }
        let abs_v = val.unsigned_abs();
        let size = (32 - abs_v.leading_zeros()) as u8;
        let bits = if val > 0 {
            val as u16
        } else {
            ((1i32 << size) - 1 + val) as u16
        };
        (size, bits)
    }

    fn encode_block(
        bw: &mut JpegBitWriter,
        block: &[u8; 64],
        qt: &[u16; 64],
        dc_pred: &mut i32,
        dc_huff: &[HuffEncEntry],
        ac_huff: &[HuffEncEntry],
    ) {
        let dct = fdct_8x8(block);
        let mut zz = [0i32; 64];
        for k in 0..64 {
            zz[k] = (dct[ZIGZAG[k]] / f64::from(qt[k])).round() as i32;
        }

        let diff = zz[0] - *dc_pred;
        *dc_pred = zz[0];
        let (dc_size, dc_bits) = encode_coeff_bits(diff);
        let dc_entry = &dc_huff[usize::from(dc_size)];
        bw.put_bits(dc_entry.code, dc_entry.len);
        bw.put_bits(dc_bits, dc_size);

        let mut last_nz = 63usize;
        while last_nz > 0 && zz[last_nz] == 0 {
            last_nz -= 1;
        }

        let mut run = 0usize;
        for &coeff in &zz[1..=last_nz] {
            if coeff == 0 {
                run += 1;
                if run == 16 {
                    let zrl = &ac_huff[0xF0];
                    bw.put_bits(zrl.code, zrl.len);
                    run = 0;
                }
            } else {
                let (ac_size, ac_bits) = encode_coeff_bits(coeff);
                let sym = ((run as u8) << 4) | ac_size;
                let entry = &ac_huff[usize::from(sym)];
                bw.put_bits(entry.code, entry.len);
                bw.put_bits(ac_bits, ac_size);
                run = 0;
            }
        }
        if last_nz < 63 {
            let eob = &ac_huff[0x00];
            bw.put_bits(eob.code, eob.len);
        }
    }

    fn rgb_to_ycbcr_pixel(r: u8, g: u8, b: u8) -> (u8, u8, u8) {
        let rf = f64::from(r);
        let gf = f64::from(g);
        let bf = f64::from(b);
        let y = (0.299 * rf + 0.587 * gf + 0.114 * bf)
            .round()
            .clamp(0.0, 255.0) as u8;
        let cb = (128.0 - 0.168736 * rf - 0.331264 * gf + 0.5 * bf)
            .round()
            .clamp(0.0, 255.0) as u8;
        let cr = (128.0 + 0.5 * rf - 0.418688 * gf - 0.081312 * bf)
            .round()
            .clamp(0.0, 255.0) as u8;
        (y, cb, cr)
    }

    struct TestJpegOpts {
        include_dht: bool,
        use_16bit_dqt: bool,
        restart_interval: Option<u16>,
        include_app_and_com: bool,
        extra_fill_ff: bool,
    }

    impl Default for TestJpegOpts {
        fn default() -> Self {
            Self {
                include_dht: true,
                use_16bit_dqt: false,
                restart_interval: None,
                include_app_and_com: false,
                extra_fill_ff: false,
            }
        }
    }

    fn push_marker(out: &mut Vec<u8>, marker: u8, extra_ff: bool) {
        if extra_ff {
            out.push(0xFF);
            out.push(0xFF);
        }
        out.push(0xFF);
        out.push(marker);
    }

    fn push_dht_table(out: &mut Vec<u8>, tc_th: u8, bits: &[u8; 16], vals: &[u8]) {
        let len = (2 + 1 + 16 + vals.len()) as u16;
        out.push(0xFF);
        out.push(0xC4);
        out.extend_from_slice(&len.to_be_bytes());
        out.push(tc_th);
        out.extend_from_slice(bits);
        out.extend_from_slice(vals);
    }

    fn encode_test_jpeg_gray(
        width: usize,
        height: usize,
        gray_pixels: &[u8],
        opts: &TestJpegOpts,
    ) -> Vec<u8> {
        let qt = [1u16; 64];
        let dc_huff = build_encoder_huff(&STD_DC_LUM_BITS, &STD_DC_LUM_VALS);
        let ac_huff = build_encoder_huff(&STD_AC_LUM_BITS, &STD_AC_LUM_VALS);

        let mut out = vec![0xFF, 0xD8];
        if opts.include_app_and_com {
            push_marker(&mut out, 0xE0, opts.extra_fill_ff);
            let app0 = b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0";
            out.extend_from_slice(&((2 + app0.len()) as u16).to_be_bytes());
            out.extend_from_slice(app0);

            push_marker(&mut out, 0xFE, opts.extra_fill_ff);
            let com = b"rsmpeg test";
            out.extend_from_slice(&((2 + com.len()) as u16).to_be_bytes());
            out.extend_from_slice(com);
        }

        // DQT
        push_marker(&mut out, 0xDB, opts.extra_fill_ff);
        if opts.use_16bit_dqt {
            out.extend_from_slice(&(2u16 + 1 + 128).to_be_bytes());
            out.push(0x10); // precision=1, id=0
            for &v in &qt {
                out.extend_from_slice(&v.to_be_bytes());
            }
        } else {
            out.extend_from_slice(&(2u16 + 1 + 64).to_be_bytes());
            out.push(0x00); // precision=0, id=0
            for &v in &qt {
                out.push(v as u8);
            }
        }

        if opts.include_dht {
            push_dht_table(&mut out, 0x00, &STD_DC_LUM_BITS, &STD_DC_LUM_VALS);
            push_dht_table(&mut out, 0x10, &STD_AC_LUM_BITS, &STD_AC_LUM_VALS);
        }

        if let Some(dri) = opts.restart_interval {
            push_marker(&mut out, 0xDD, opts.extra_fill_ff);
            out.extend_from_slice(&4u16.to_be_bytes());
            out.extend_from_slice(&dri.to_be_bytes());
        }

        // SOF0
        push_marker(&mut out, 0xC0, opts.extra_fill_ff);
        out.extend_from_slice(&11u16.to_be_bytes());
        out.push(8);
        out.extend_from_slice(&(height as u16).to_be_bytes());
        out.extend_from_slice(&(width as u16).to_be_bytes());
        out.push(1);
        out.extend_from_slice(&[1, 0x11, 0]);

        // SOS
        push_marker(&mut out, 0xDA, opts.extra_fill_ff);
        out.extend_from_slice(&8u16.to_be_bytes());
        out.push(1);
        out.extend_from_slice(&[1, 0x00]);
        out.extend_from_slice(&[0, 63, 0]);

        let mcu_cols = width.div_ceil(8);
        let mcu_rows = height.div_ceil(8);
        let mut bw = JpegBitWriter::new();
        let mut dc_pred = 0i32;
        let mut mcu_idx = 0usize;
        let mut rst_num = 0u8;

        for my in 0..mcu_rows {
            for mx in 0..mcu_cols {
                if let Some(dri) = opts.restart_interval {
                    let dri = usize::from(dri);
                    if dri > 0 && mcu_idx > 0 && mcu_idx % dri == 0 {
                        bw.flush_padded();
                        bw.out.push(0xFF);
                        bw.out.push(0xD0 + (rst_num & 7));
                        rst_num = rst_num.wrapping_add(1);
                        dc_pred = 0;
                    }
                }
                let mut block = [0u8; 64];
                for y in 0..8 {
                    let sy = (my * 8 + y).min(height - 1);
                    for x in 0..8 {
                        let sx = (mx * 8 + x).min(width - 1);
                        block[y * 8 + x] = gray_pixels[sy * width + sx];
                    }
                }
                encode_block(&mut bw, &block, &qt, &mut dc_pred, &dc_huff, &ac_huff);
                mcu_idx += 1;
            }
        }
        bw.flush_padded();
        out.extend_from_slice(&bw.out);
        push_marker(&mut out, 0xD9, opts.extra_fill_ff);
        out
    }

    fn encode_test_jpeg_ycbcr(
        width: usize,
        height: usize,
        rgb_pixels: &[u8],
        y_h: usize,
        y_v: usize,
        opts: &TestJpegOpts,
    ) -> Vec<u8> {
        let qt = [1u16; 64];
        let dc_lum = build_encoder_huff(&STD_DC_LUM_BITS, &STD_DC_LUM_VALS);
        let ac_lum = build_encoder_huff(&STD_AC_LUM_BITS, &STD_AC_LUM_VALS);
        let dc_chr = build_encoder_huff(&STD_DC_CHR_BITS, &STD_DC_CHR_VALS);
        let ac_chr = build_encoder_huff(&STD_AC_CHR_BITS, &STD_AC_CHR_VALS);

        let mut y_plane = vec![0u8; width * height];
        let mut cb_plane = vec![0u8; width * height];
        let mut cr_plane = vec![0u8; width * height];
        for (i, px) in rgb_pixels.chunks_exact(3).enumerate() {
            let (yv, cbv, crv) = rgb_to_ycbcr_pixel(px[0], px[1], px[2]);
            y_plane[i] = yv;
            cb_plane[i] = cbv;
            cr_plane[i] = crv;
        }

        let mut out = vec![0xFF, 0xD8];
        if opts.include_app_and_com {
            push_marker(&mut out, 0xE0, opts.extra_fill_ff);
            let app0 = b"AVI1\0\0";
            out.extend_from_slice(&((2 + app0.len()) as u16).to_be_bytes());
            out.extend_from_slice(app0);

            push_marker(&mut out, 0xFE, opts.extra_fill_ff);
            let com = b"mjpeg";
            out.extend_from_slice(&((2 + com.len()) as u16).to_be_bytes());
            out.extend_from_slice(com);
        }

        // DQT table 0 & 1
        push_marker(&mut out, 0xDB, opts.extra_fill_ff);
        out.extend_from_slice(&(2u16 + 2 * 65).to_be_bytes());
        out.push(0x00);
        out.extend_from_slice(&[1u8; 64]);
        out.push(0x01);
        out.extend_from_slice(&[1u8; 64]);

        if opts.include_dht {
            push_dht_table(&mut out, 0x00, &STD_DC_LUM_BITS, &STD_DC_LUM_VALS);
            push_dht_table(&mut out, 0x01, &STD_DC_CHR_BITS, &STD_DC_CHR_VALS);
            push_dht_table(&mut out, 0x10, &STD_AC_LUM_BITS, &STD_AC_LUM_VALS);
            push_dht_table(&mut out, 0x11, &STD_AC_CHR_BITS, &STD_AC_CHR_VALS);
        }

        if let Some(dri) = opts.restart_interval {
            push_marker(&mut out, 0xDD, opts.extra_fill_ff);
            out.extend_from_slice(&4u16.to_be_bytes());
            out.extend_from_slice(&dri.to_be_bytes());
        }

        // SOF0
        push_marker(&mut out, 0xC0, opts.extra_fill_ff);
        out.extend_from_slice(&(2u16 + 6 + 9).to_be_bytes());
        out.push(8);
        out.extend_from_slice(&(height as u16).to_be_bytes());
        out.extend_from_slice(&(width as u16).to_be_bytes());
        out.push(3);
        let y_hv = ((y_h as u8) << 4) | (y_v as u8);
        out.extend_from_slice(&[1, y_hv, 0]);
        out.extend_from_slice(&[2, 0x11, 1]);
        out.extend_from_slice(&[3, 0x11, 1]);

        // SOS
        push_marker(&mut out, 0xDA, opts.extra_fill_ff);
        out.extend_from_slice(&(2u16 + 1 + 6 + 3).to_be_bytes());
        out.push(3);
        out.extend_from_slice(&[1, 0x00, 2, 0x11, 3, 0x11]);
        out.extend_from_slice(&[0, 63, 0]);

        let mcu_w = y_h * 8;
        let mcu_h = y_v * 8;
        let mcu_cols = width.div_ceil(mcu_w);
        let mcu_rows = height.div_ceil(mcu_h);

        let mut bw = JpegBitWriter::new();
        let mut dc_preds = [0i32; 3];
        let mut mcu_idx = 0usize;
        let mut rst_num = 0u8;

        for my in 0..mcu_rows {
            for mx in 0..mcu_cols {
                if let Some(dri) = opts.restart_interval {
                    let dri = usize::from(dri);
                    if dri > 0 && mcu_idx > 0 && mcu_idx % dri == 0 {
                        bw.flush_padded();
                        bw.out.push(0xFF);
                        bw.out.push(0xD0 + (rst_num & 7));
                        rst_num = rst_num.wrapping_add(1);
                        dc_preds = [0i32; 3];
                    }
                }

                // Y blocks
                for by in 0..y_v {
                    for bx in 0..y_h {
                        let mut block = [0u8; 64];
                        for py in 0..8 {
                            let sy = (my * mcu_h + by * 8 + py).min(height - 1);
                            for px in 0..8 {
                                let sx = (mx * mcu_w + bx * 8 + px).min(width - 1);
                                block[py * 8 + px] = y_plane[sy * width + sx];
                            }
                        }
                        encode_block(&mut bw, &block, &qt, &mut dc_preds[0], &dc_lum, &ac_lum);
                    }
                }

                // Cb and Cr blocks (subsampled by y_h x y_v)
                for (comp_idx, plane) in [(1usize, &cb_plane), (2usize, &cr_plane)] {
                    let mut block = [0u8; 64];
                    for py in 0..8 {
                        let sy = (my * mcu_h + py * y_v).min(height - 1);
                        for px in 0..8 {
                            let sx = (mx * mcu_w + px * y_h).min(width - 1);
                            block[py * 8 + px] = plane[sy * width + sx];
                        }
                    }
                    encode_block(
                        &mut bw,
                        &block,
                        &qt,
                        &mut dc_preds[comp_idx],
                        &dc_chr,
                        &ac_chr,
                    );
                }

                mcu_idx += 1;
            }
        }
        bw.flush_padded();
        out.extend_from_slice(&bw.out);
        push_marker(&mut out, 0xD9, opts.extra_fill_ff);
        out
    }

    fn make_packet(data: Vec<u8>) -> Packet {
        Packet {
            data: Bytes::from(data),
            pts: Some(123),
            dts: Some(123),
            duration: 33,
            stream_index: 0,
            flags: PacketFlags::KEY,
            pos: 0,
            time_base: Rational::new(1, 1000),
        }
    }

    #[test]
    fn codec_metadata_and_capabilities() {
        let codec = MjpegCodec;
        assert_eq!(codec.id(), CodecId::Mjpeg);
        assert_eq!(codec.media_type(), MediaType::Video);
        assert_eq!(codec.name(), "mjpeg");
        assert!(!codec.long_name().is_empty());

        let caps = codec.capabilities();
        assert!(caps.can_decode);
        assert!(!caps.can_encode);
        assert!(!caps.lossless);
        assert!(caps.intra_only);

        let dec = codec.create_decoder().expect("create_decoder succeeds");
        assert_eq!(dec.codec_id(), CodecId::Mjpeg);

        assert!(matches!(
            codec.create_encoder(),
            Err(RsError::Unsupported(_))
        ));
    }

    #[test]
    fn from_params_validates_codec_id() {
        let mjpeg_params = CodecParameters::for_video(640, 480, CodecId::Mjpeg);
        let dec = MjpegDecoder::from_params(&mjpeg_params).expect("MJPEG params accepted");
        assert_eq!(dec.get_parameters().width, Some(640));
        assert_eq!(dec.get_parameters().height, Some(480));
        assert_eq!(dec.get_parameters().codec_id, CodecId::Mjpeg);

        let unknown_params = CodecParameters::new(CodecId::Unknown);
        let dec_u = MjpegDecoder::from_params(&unknown_params).expect("Unknown params accepted");
        assert_eq!(dec_u.get_parameters().codec_id, CodecId::Mjpeg);

        let h264_params = CodecParameters::new(CodecId::H264);
        assert!(matches!(
            MjpegDecoder::from_params(&h264_params),
            Err(RsError::Unsupported(_))
        ));
    }

    #[test]
    fn annex_k_default_huffman_tables_are_valid() {
        let dc = default_dc_tables().expect("DC defaults build");
        let ac = default_ac_tables().expect("AC defaults build");
        assert_eq!(dc[0].as_ref().unwrap().values.len(), 12);
        assert_eq!(dc[1].as_ref().unwrap().values.len(), 12);
        assert_eq!(ac[0].as_ref().unwrap().values.len(), 162);
        assert_eq!(ac[1].as_ref().unwrap().values.len(), 162);

        // Verify distinct complete symbol sets for Annex K.3.3 tables
        for tbl_opt in [&dc[0], &dc[1]] {
            let vals = &tbl_opt.as_ref().unwrap().values;
            for s in 0..12u8 {
                assert!(vals.contains(&s), "DC table missing symbol {s}");
            }
        }
        for tbl_opt in [&ac[0], &ac[1]] {
            let vals = &tbl_opt.as_ref().unwrap().values;
            assert!(vals.contains(&0x00));
            assert!(vals.contains(&0xF0));
            for run in 0..16u8 {
                for size in 1..=10u8 {
                    let sym = (run << 4) | size;
                    assert!(vals.contains(&sym), "AC table missing symbol 0x{sym:02X}");
                }
            }
        }
    }

    #[test]
    fn decode_1_component_grayscale_8x8_solid_and_gradient() {
        let solid = vec![200u8; 64];
        let jpeg_solid = encode_test_jpeg_gray(8, 8, &solid, &TestJpegOpts::default());
        let mut dec = MjpegDecoder::new();
        let frames = dec.decode(&make_packet(jpeg_solid)).expect("decode solid");
        assert_eq!(frames.len(), 1);
        let f = &frames[0];
        assert_eq!(f.width, 8);
        assert_eq!(f.height, 8);
        assert_eq!(f.pixel_format, PixelFormat::Rgb24);
        assert_eq!(f.data[0].len(), 8 * 8 * 3);
        for px in f.data[0].chunks_exact(3) {
            assert_eq!(px[0], px[1]);
            assert_eq!(px[1], px[2]);
            assert!((i16::from(px[0]) - 200).abs() <= 1);
        }

        // Gradient 8x8 with 16-bit DQT, APP0/COM, and fill 0xFF bytes
        let mut grad = vec![0u8; 64];
        for y in 0..8 {
            for x in 0..8 {
                grad[y * 8 + x] = ((x * 24 + y * 10) + 16) as u8;
            }
        }
        let opts = TestJpegOpts {
            use_16bit_dqt: true,
            include_app_and_com: true,
            extra_fill_ff: true,
            ..Default::default()
        };
        let jpeg_grad = encode_test_jpeg_gray(8, 8, &grad, &opts);
        let frames_grad = dec
            .decode(&make_packet(jpeg_grad))
            .expect("decode gradient");
        let fg = &frames_grad[0];
        for (i, px) in fg.data[0].chunks_exact(3).enumerate() {
            assert_eq!(px[0], px[1]);
            assert_eq!(px[1], px[2]);
            let diff = (i16::from(px[0]) - i16::from(grad[i])).abs();
            assert!(diff <= 2, "pixel {i}: got {} vs {}", px[0], grad[i]);
        }
    }

    #[test]
    fn decode_3_component_444_colored_8x8() {
        let mut rgb = vec![0u8; 8 * 8 * 3];
        for y in 0..8 {
            for x in 0..8 {
                let idx = (y * 8 + x) * 3;
                if x < 4 {
                    rgb[idx] = 220;
                    rgb[idx + 1] = 30;
                    rgb[idx + 2] = 40;
                } else {
                    rgb[idx] = 30;
                    rgb[idx + 1] = 50;
                    rgb[idx + 2] = 230;
                }
            }
        }
        let jpeg = encode_test_jpeg_ycbcr(8, 8, &rgb, 1, 1, &TestJpegOpts::default());
        let mut dec = MjpegDecoder::new();
        let frames = dec.decode(&make_packet(jpeg)).expect("decode 4:4:4");
        let f = &frames[0];
        assert_eq!(f.width, 8);
        assert_eq!(f.height, 8);
        for (i, (got, exp)) in f.data[0]
            .chunks_exact(3)
            .zip(rgb.chunks_exact(3))
            .enumerate()
        {
            for c in 0..3 {
                let diff = (i16::from(got[c]) - i16::from(exp[c])).abs();
                assert!(
                    diff <= 4,
                    "4:4:4 px {i} ch {c}: got {} vs {}",
                    got[c],
                    exp[c]
                );
            }
        }
    }

    #[test]
    fn decode_3_component_420_16x16_quadrants() {
        let w = 16usize;
        let h = 16usize;
        let mut rgb = vec![0u8; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let color = match (x < 8, y < 8) {
                    (true, true) => [235u8, 20, 20],   // Top-left: Red
                    (false, true) => [20, 220, 30],    // Top-right: Green
                    (true, false) => [20, 30, 235],    // Bottom-left: Blue
                    (false, false) => [240, 240, 240], // Bottom-right: White
                };
                let idx = (y * w + x) * 3;
                rgb[idx..idx + 3].copy_from_slice(&color);
            }
        }

        let jpeg = encode_test_jpeg_ycbcr(w, h, &rgb, 2, 2, &TestJpegOpts::default());
        let mut dec = MjpegDecoder::new();
        let frames = dec.decode(&make_packet(jpeg)).expect("decode 4:2:0");
        let f = &frames[0];
        assert_eq!(f.width, 16);
        assert_eq!(f.height, 16);
        for (i, (got, exp)) in f.data[0]
            .chunks_exact(3)
            .zip(rgb.chunks_exact(3))
            .enumerate()
        {
            for c in 0..3 {
                let diff = (i16::from(got[c]) - i16::from(exp[c])).abs();
                assert!(
                    diff <= 4,
                    "4:2:0 px {i} ch {c}: got {} vs {}",
                    got[c],
                    exp[c]
                );
            }
        }
    }

    #[test]
    fn decode_mjpeg_omitting_dht_uses_annex_k_defaults() {
        let w = 16usize;
        let h = 16usize;
        let mut rgb = vec![0u8; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let idx = (y * w + x) * 3;
                rgb[idx] = ((x / 2) * 32) as u8;
                rgb[idx + 1] = (y * 16) as u8;
                rgb[idx + 2] = 128;
            }
        }

        let opts = TestJpegOpts {
            include_dht: false,
            include_app_and_com: true,
            ..Default::default()
        };
        let mjpeg = encode_test_jpeg_ycbcr(w, h, &rgb, 2, 1, &opts);
        // Confirm no DHT marker is present in the frame
        assert!(!mjpeg.windows(2).any(|win| win == [0xFF, 0xC4]));

        let mut dec = MjpegDecoder::new();
        let frames = dec
            .decode(&make_packet(mjpeg))
            .expect("decode MJPEG without DHT");
        let f = &frames[0];
        assert_eq!(f.width, 16);
        assert_eq!(f.height, 16);
        for (i, (got, exp)) in f.data[0]
            .chunks_exact(3)
            .zip(rgb.chunks_exact(3))
            .enumerate()
        {
            for c in 0..3 {
                let diff = (i16::from(got[c]) - i16::from(exp[c])).abs();
                assert!(
                    diff <= 8,
                    "4:2:2 no-DHT px {i} ch {c}: got {} vs {}",
                    got[c],
                    exp[c]
                );
            }
        }
    }

    #[test]
    fn decode_with_dri_and_rst_restart_markers() {
        let w = 16usize;
        let h = 16usize;
        let mut gray = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                gray[y * w + x] = match (x < 8, y < 8) {
                    (true, true) => 30,
                    (false, true) => 220,
                    (true, false) => 120,
                    (false, false) => 250,
                };
            }
        }

        let opts = TestJpegOpts {
            restart_interval: Some(1), // 4 MCUs -> RST0, RST1, RST2
            ..Default::default()
        };
        let jpeg = encode_test_jpeg_gray(w, h, &gray, &opts);
        assert!(jpeg.windows(2).any(|win| win == [0xFF, 0xDD]));
        assert!(jpeg.windows(2).any(|win| win == [0xFF, 0xD0]));
        assert!(jpeg.windows(2).any(|win| win == [0xFF, 0xD1]));
        assert!(jpeg.windows(2).any(|win| win == [0xFF, 0xD2]));

        let mut dec = MjpegDecoder::new();
        let frames = dec.decode(&make_packet(jpeg)).expect("decode with DRI+RST");
        let f = &frames[0];
        for (i, px) in f.data[0].chunks_exact(3).enumerate() {
            let diff = (i16::from(px[0]) - i16::from(gray[i])).abs();
            assert!(diff <= 2, "RST px {i}: got {} vs {}", px[0], gray[i]);
        }
    }

    #[test]
    fn decode_odd_dimensions_and_byte_stuffing() {
        // Odd dimensions 13x10 with 4:2:0 and high-contrast 255 white blocks to exercise byte stuffing
        let w = 13usize;
        let h = 10usize;
        let mut rgb = vec![255u8; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                if (x + y) % 2 == 0 {
                    let idx = (y * w + x) * 3;
                    rgb[idx] = 0;
                    rgb[idx + 1] = 255;
                    rgb[idx + 2] = 0;
                }
            }
        }
        let jpeg = encode_test_jpeg_ycbcr(w, h, &rgb, 2, 2, &TestJpegOpts::default());
        let mut dec = MjpegDecoder::new();
        let frames = dec.decode(&make_packet(jpeg)).expect("decode odd 13x10");
        assert_eq!(frames[0].width, 13);
        assert_eq!(frames[0].height, 10);
        assert_eq!(frames[0].data[0].len(), 13 * 10 * 3);
    }

    #[test]
    fn malformed_and_truncated_inputs_return_err_without_panic() {
        let mut dec = MjpegDecoder::new();

        // Empty and invalid magic
        assert!(dec.decode(&make_packet(vec![])).is_err());
        assert!(dec.decode(&make_packet(vec![0x00, 0x01, 0x02])).is_err());

        // Progressive JPEG (SOF2) cleanly rejected as Unsupported
        let sof2 = vec![
            0xFF, 0xD8, 0xFF, 0xC2, 0x00, 0x0B, 0x08, 0x00, 0x08, 0x00, 0x08, 0x01, 0x01, 0x11,
            0x00, 0xFF, 0xD9,
        ];
        assert!(matches!(
            dec.decode(&make_packet(sof2)),
            Err(RsError::Unsupported(_))
        ));

        // Zero width / oversized dimensions in SOF0
        let zero_dim = vec![
            0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00, 0x08, 0x00, 0x00, 0x01, 0x01, 0x11,
            0x00, 0xFF, 0xD9,
        ];
        assert!(dec.decode(&make_packet(zero_dim)).is_err());

        let huge_dim = vec![
            0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x40, 0x01, 0x40, 0x01, 0x01, 0x01, 0x11,
            0x00, 0xFF, 0xD9,
        ];
        assert!(dec.decode(&make_packet(huge_dim)).is_err());

        // Invalid sampling factor (0x00)
        let bad_samp = vec![
            0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00, 0x08, 0x00, 0x08, 0x01, 0x01, 0x00,
            0x00, 0xFF, 0xD9,
        ];
        assert!(dec.decode(&make_packet(bad_samp)).is_err());

        // Truncate a valid JPEG at every single prefix length
        let solid = vec![128u8; 64];
        let valid = encode_test_jpeg_gray(8, 8, &solid, &TestJpegOpts::default());
        for cut in 0..valid.len() {
            assert!(
                dec.decode(&make_packet(valid[..cut].to_vec())).is_err(),
                "truncation at {cut}/{} should fail cleanly",
                valid.len()
            );
        }
    }

    #[test]
    fn send_receive_reset_eos_lifecycle() {
        let solid = vec![90u8; 64];
        let jpeg = encode_test_jpeg_gray(8, 8, &solid, &TestJpegOpts::default());
        let pkt = make_packet(jpeg);

        let mut dec = MjpegDecoder::new();
        assert!(matches!(
            dec.receive_frame().unwrap(),
            DecodeStatus::NeedMoreInput
        ));

        dec.send_packet(Some(&pkt)).unwrap();
        match dec.receive_frame().unwrap() {
            DecodeStatus::Frame(f) => {
                assert_eq!(f.pts, Some(123));
                assert_eq!(f.duration, 33);
                assert_eq!(f.time_base, Rational::new(1, 1000));
                assert!(f.key_frame);
                assert_eq!(f.pict_type, PictureType::I);
                assert_eq!(f.width, 8);
                assert_eq!(f.height, 8);
                assert_eq!(f.pixel_format, PixelFormat::Rgb24);
            }
            other => panic!("expected Frame, got {other:?}"),
        }

        let updated = dec.get_parameters();
        assert_eq!(updated.width, Some(8));
        assert_eq!(updated.height, Some(8));
        assert_eq!(updated.pixel_format, Some(PixelFormat::Rgb24));

        assert!(matches!(
            dec.receive_frame().unwrap(),
            DecodeStatus::NeedMoreInput
        ));

        dec.send_packet(None).unwrap();
        assert!(matches!(
            dec.receive_frame().unwrap(),
            DecodeStatus::EndOfStream
        ));
        assert!(dec.send_packet(Some(&pkt)).is_err());

        dec.reset().unwrap();
        assert!(matches!(
            dec.receive_frame().unwrap(),
            DecodeStatus::NeedMoreInput
        ));
        dec.send_packet(Some(&pkt)).unwrap();
        assert!(matches!(
            dec.receive_frame().unwrap(),
            DecodeStatus::Frame(_)
        ));
    }

    #[test]
    fn early_eoi_at_end_of_scan_returns_cleanly() {
        // Strip the last padding byte right before EOI in a solid 16x8 image where
        // the second block has all-zero AC coeffs and hits EOI during trailing EOB.
        let solid = vec![128u8; 16 * 8];
        let mut jpeg = encode_test_jpeg_gray(16, 8, &solid, &TestJpegOpts::default());
        // Remove the final entropy byte immediately before [0xFF, 0xD9], preserving EOI.
        let eoi_pos = jpeg.len() - 2;
        assert_eq!(&jpeg[eoi_pos..], &[0xFF, 0xD9]);
        jpeg.remove(eoi_pos - 1);

        let mut dec = MjpegDecoder::new();
        let frames = dec
            .decode(&make_packet(jpeg))
            .expect("early EOI at end of scan must decode cleanly");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].width, 16);
        assert_eq!(frames[0].height, 8);
    }

    /// Floating-point reference IDCT (direct ITU-T T.81 A.3.3 formula) with
    /// level shift, round-half-away-from-zero and clamping.
    fn reference_idct(coeff: &[i32; 64]) -> [u8; 64] {
        let mut out = [0u8; 64];
        for y in 0..8 {
            for x in 0..8 {
                let mut sum = 0.0f64;
                for v in 0..8 {
                    let cv = if v == 0 { FRAC_1_SQRT_2 } else { 1.0 };
                    let cos_y = ((2 * y + 1) as f64 * v as f64 * PI / 16.0).cos();
                    for u in 0..8 {
                        let cu = if u == 0 { FRAC_1_SQRT_2 } else { 1.0 };
                        let cos_x = ((2 * x + 1) as f64 * u as f64 * PI / 16.0).cos();
                        sum += cu * cv * f64::from(coeff[v * 8 + u]) * cos_x * cos_y;
                    }
                }
                out[y * 8 + x] = (sum / 4.0 + 128.0).round().clamp(0.0, 255.0) as u8;
            }
        }
        out
    }

    /// Small deterministic xorshift PRNG for reproducible fixtures.
    struct XorShift(u64);
    impl XorShift {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
        fn range(&mut self, lo: i32, hi: i32) -> i32 {
            lo + (self.next() % (hi - lo + 1) as u64) as i32
        }
    }

    fn max_idct_error(coeff: &[i32; 64]) -> i32 {
        let mut got = [0u8; 64];
        idct_8x8_int(coeff, &mut got);
        let exp = reference_idct(coeff);
        got.iter()
            .zip(exp.iter())
            .map(|(&g, &e)| (i32::from(g) - i32::from(e)).abs())
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn integer_idct_matches_float_reference_within_one_lsb() {
        let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
        // IEEE 1180-style random blocks at several dynamic ranges, plus
        // sparse blocks that exercise the row/column AC-zero fast paths.
        for &(lo, hi) in &[(-256, 255), (-5, 5), (-300, 300), (-40, 40)] {
            for _ in 0..2_000 {
                let mut c = [0i32; 64];
                for v in c.iter_mut() {
                    *v = rng.range(lo, hi);
                }
                assert!(max_idct_error(&c) <= 1, "dense block {c:?}");

                let mut sparse = [0i32; 64];
                sparse[0] = rng.range(-1024, 1016);
                for _ in 0..3 {
                    let idx = (rng.next() % 64) as usize;
                    sparse[idx] = rng.range(lo, hi);
                }
                assert!(max_idct_error(&sparse) <= 1, "sparse block {sparse:?}");
            }
        }
    }

    #[test]
    fn integer_idct_known_fixtures() {
        // DC-only: DC/8 + 128 everywhere.
        let mut c = [0i32; 64];
        let mut out = [0u8; 64];
        for (dc, expected) in [(0, 128u8), (80, 138), (-80, 118), (1016, 255), (-1024, 0)] {
            c[0] = dc;
            idct_8x8_int(&c, &mut out);
            assert!(out.iter().all(|&p| p == expected), "dc={dc}: {out:?}");
        }
        // Huge (hostile 16-bit DQT) products clamp instead of overflowing.
        c[0] = i32::MAX;
        c[63] = i32::MIN;
        idct_8x8_int(&c, &mut out);
        let _ = out;

        // A single horizontal first-harmonic: columns are flat, rows vary
        // monotonically from left to right (cos((2x+1)π/16) decreases).
        let mut h = [0i32; 64];
        h[1] = 200;
        idct_8x8_int(&h, &mut out);
        assert_eq!(out, reference_idct(&h));
        for row in out.chunks_exact(8) {
            assert_eq!(row, &out[..8], "every row must be identical");
            assert!(row.windows(2).all(|w| w[0] >= w[1]), "{row:?}");
        }
        // The vertical counterpart is the transpose.
        let mut v = [0i32; 64];
        v[8] = 200;
        let mut out_v = [0u8; 64];
        idct_8x8_int(&v, &mut out_v);
        for y in 0..8 {
            for x in 0..8 {
                assert_eq!(out_v[y * 8 + x], out[x * 8 + y]);
            }
        }
    }

    #[test]
    fn fixed_point_ycbcr_matches_float_jfif_within_one() {
        for y in (0..=255u8).step_by(5) {
            for cb in (0..=255u8).step_by(3) {
                for cr in (0..=255u8).step_by(7) {
                    let [r, g, b] = ycbcr_to_rgb(y, cb, cr);
                    let yf = f64::from(y);
                    let cbf = f64::from(cb) - 128.0;
                    let crf = f64::from(cr) - 128.0;
                    let rf = (yf + 1.402 * crf).round().clamp(0.0, 255.0);
                    let gf = (yf - 0.344136 * cbf - 0.714136 * crf)
                        .round()
                        .clamp(0.0, 255.0);
                    let bf = (yf + 1.772 * cbf).round().clamp(0.0, 255.0);
                    for (got, exp) in [(r, rf), (g, gf), (b, bf)] {
                        assert!(
                            (f64::from(got) - exp).abs() <= 1.0,
                            "Y={y} Cb={cb} Cr={cr}: {got} vs {exp}"
                        );
                    }
                }
            }
        }
        // Neutral chroma is exact grey.
        assert_eq!(ycbcr_to_rgb(77, 128, 128), [77, 77, 77]);
    }

    fn assert_rgb_close(got: &[u8], exp: &[u8], tol: i16, label: &str) {
        assert_eq!(got.len(), exp.len(), "{label}: size mismatch");
        for (i, (g, e)) in got.chunks_exact(3).zip(exp.chunks_exact(3)).enumerate() {
            for c in 0..3 {
                let diff = (i16::from(g[c]) - i16::from(e[c])).abs();
                assert!(
                    diff <= tol,
                    "{label} px {i} ch {c}: got {} vs {}",
                    g[c],
                    e[c]
                );
            }
        }
    }

    /// Smooth RGB image whose chroma is constant over 4x2 cells, so every
    /// supported subsampling ratio reproduces it closely.
    fn smooth_rgb(w: usize, h: usize) -> Vec<u8> {
        let mut rgb = vec![0u8; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let idx = (y * w + x) * 3;
                let cell = ((x / 4) * 37 + (y / 2) * 53) % 160;
                rgb[idx] = (40 + cell) as u8;
                rgb[idx + 1] = (180 - cell / 2) as u8;
                rgb[idx + 2] = (60 + (cell * 3) % 150) as u8;
            }
        }
        rgb
    }

    #[test]
    fn decode_422_fast_path_matches_source() {
        // Flat 16x8 MCUs per colour: chroma is constant per MCU, so 4:2:2
        // reconstruction must be near-exact.
        let w = 32usize;
        let h = 8usize;
        let mut rgb = vec![0u8; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let color = if x < 16 {
                    [200u8, 40, 60]
                } else {
                    [30, 90, 210]
                };
                let idx = (y * w + x) * 3;
                rgb[idx..idx + 3].copy_from_slice(&color);
            }
        }
        let jpeg = encode_test_jpeg_ycbcr(w, h, &rgb, 2, 1, &TestJpegOpts::default());
        let mut dec = MjpegDecoder::new();
        let frames = dec.decode(&make_packet(jpeg)).expect("decode 4:2:2");
        assert_eq!((frames[0].width, frames[0].height), (32, 8));
        assert_rgb_close(&frames[0].data[0], &rgb, 4, "4:2:2");
    }

    #[test]
    fn decode_general_sampling_fallbacks_411_and_440() {
        let w = 32usize;
        let h = 16usize;
        let rgb = smooth_rgb(w, h);
        for (y_h, y_v, label) in [(4usize, 1usize, "4:1:1"), (1, 2, "4:4:0")] {
            let jpeg = encode_test_jpeg_ycbcr(w, h, &rgb, y_h, y_v, &TestJpegOpts::default());
            let mut dec = MjpegDecoder::new();
            let frames = dec.decode(&make_packet(jpeg)).expect(label);
            assert_eq!((frames[0].width, frames[0].height), (w, h), "{label}");
            // Chroma subsampling is lossy for this pattern; just require it to
            // stay close (and to agree with the general integer path).
            assert_rgb_close(&frames[0].data[0], &rgb, 48, label);
        }
    }

    #[test]
    fn fast_paths_agree_with_general_fallback() {
        // Decode once through the public API (fast path), then re-run the
        // general per-pixel loop by hand on a frame header that forces it.
        let w = 24usize;
        let h = 16usize;
        let rgb = smooth_rgb(w, h);
        for (y_h, y_v) in [(1usize, 1usize), (2, 1), (2, 2)] {
            let jpeg = encode_test_jpeg_ycbcr(w, h, &rgb, y_h, y_v, &TestJpegOpts::default());
            let mut dec = MjpegDecoder::new();
            let fast = dec.decode(&make_packet(jpeg)).expect("decode")[0].data[0].clone();
            assert_rgb_close(&fast, &rgb, 12, "fast path vs source");

            // Synthesize planes and compare fast vs general on the same data.
            let comps = vec![
                ComponentInfo {
                    id: 1,
                    h_samp: y_h,
                    v_samp: y_v,
                    qt_id: 0,
                },
                ComponentInfo {
                    id: 2,
                    h_samp: 1,
                    v_samp: 1,
                    qt_id: 0,
                },
                ComponentInfo {
                    id: 3,
                    h_samp: 1,
                    v_samp: 1,
                    qt_id: 0,
                },
            ];
            let frame = FrameHeader {
                width: w,
                height: h,
                components: comps,
                max_h: y_h,
                max_v: y_v,
                mcu_cols: w.div_ceil(8 * y_h),
                mcu_rows: h.div_ceil(8 * y_v),
            };
            let mut planes = Vec::new();
            let mut rng = XorShift(7 + (y_h * 10 + y_v) as u64);
            for c in &frame.components {
                let mut p =
                    CompPlane::try_new(frame.mcu_cols * c.h_samp, frame.mcu_rows * c.v_samp)
                        .unwrap();
                for b in p.data.iter_mut() {
                    *b = (rng.next() & 0xFF) as u8;
                }
                planes.push(p);
            }
            let fast_out = assemble_rgb24(&frame, &planes).unwrap();
            let mut general = vec![0u8; w * h * 3];
            for y in 0..h {
                for x in 0..w {
                    let yv = planes[0].data[y * planes[0].stride + x];
                    let cy = y / y_v;
                    let cx = x / y_h;
                    let cb = planes[1].data[cy * planes[1].stride + cx];
                    let cr = planes[2].data[cy * planes[2].stride + cx];
                    general[(y * w + x) * 3..(y * w + x) * 3 + 3]
                        .copy_from_slice(&ycbcr_to_rgb(yv, cb, cr));
                }
            }
            assert_eq!(fast_out, general, "{y_h}x{y_v} fast path diverged");
        }
    }

    fn to_sof1(mut jpeg: Vec<u8>) -> Vec<u8> {
        let pos = jpeg
            .windows(2)
            .position(|w| w == [0xFF, 0xC0])
            .expect("SOF0 present");
        jpeg[pos + 1] = 0xC1;
        jpeg
    }

    #[test]
    fn decode_sof1_extended_sequential_8bit() {
        let rgb = smooth_rgb(16, 16);
        let sof0 = encode_test_jpeg_ycbcr(16, 16, &rgb, 2, 2, &TestJpegOpts::default());
        let sof1 = to_sof1(sof0.clone());
        assert!(sof1.windows(2).any(|w| w == [0xFF, 0xC1]));
        assert!(!sof1.windows(2).any(|w| w == [0xFF, 0xC0]));

        let mut dec = MjpegDecoder::new();
        let a = dec.decode(&make_packet(sof0)).expect("SOF0")[0].data[0].clone();
        let b = dec.decode(&make_packet(sof1)).expect("SOF1")[0].data[0].clone();
        assert_eq!(a, b, "8-bit SOF1 must decode identically to SOF0");

        // Gray SOF1 too.
        let gray = vec![77u8; 64];
        let g = to_sof1(encode_test_jpeg_gray(8, 8, &gray, &TestJpegOpts::default()));
        let f = dec.decode(&make_packet(g)).expect("gray SOF1");
        assert!(f[0].data[0].iter().all(|&p| (i16::from(p) - 77).abs() <= 1));
    }

    #[test]
    fn sof1_rejects_12_bit_precision_and_duplicate_sof() {
        let gray = vec![77u8; 64];
        let mut jpeg = to_sof1(encode_test_jpeg_gray(8, 8, &gray, &TestJpegOpts::default()));
        let pos = jpeg.windows(2).position(|w| w == [0xFF, 0xC1]).unwrap();
        // Precision byte follows the 2-byte segment length.
        jpeg[pos + 4] = 12;
        let mut dec = MjpegDecoder::new();
        assert!(matches!(
            dec.decode(&make_packet(jpeg)),
            Err(RsError::Unsupported(_))
        ));

        // SOF0 followed by SOF1 is a duplicate frame header.
        let base = encode_test_jpeg_gray(8, 8, &gray, &TestJpegOpts::default());
        let sof_pos = base.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap();
        let sof_seg = base[sof_pos..sof_pos + 2 + 11].to_vec();
        let mut dup = base[..sof_pos + 13].to_vec();
        let mut second = sof_seg;
        second[1] = 0xC1;
        dup.extend_from_slice(&second);
        dup.extend_from_slice(&base[sof_pos + 13..]);
        assert!(matches!(
            dec.decode(&make_packet(dup)),
            Err(RsError::InvalidData(_))
        ));

        // Other SOF types stay unsupported.
        let mut sof3 = encode_test_jpeg_gray(8, 8, &gray, &TestJpegOpts::default());
        let p3 = sof3.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap();
        sof3[p3 + 1] = 0xC3;
        assert!(matches!(
            dec.decode(&make_packet(sof3)),
            Err(RsError::Unsupported(_))
        ));
    }
}
