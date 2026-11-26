//! AVI (RIFF `AVI `) demuxer.
//!
//! Parses `LIST hdrl` (`avih` + one `LIST strl` per stream), locates
//! `LIST movi` and the optional legacy `idx1` index, and streams packets either
//! from the index (exact keyframe flags, cheap seeking) or — when there is no
//! usable index — by walking the `movi` chunks sequentially (descending into
//! `LIST rec ` groups and skipping `JUNK` / `ix##` chunks).
//!
//! Every length read from the file is treated as hostile: all arithmetic is
//! checked or saturating, every allocation is bounded, and every loop has an
//! iteration cap, so truncated or malicious files yield `Err` / `Ok(None)`
//! instead of a panic or an out-of-memory abort. Packet payloads are read one
//! chunk at a time; the file is never loaded into memory as a whole.

use crate::format::InputFormat;
use crate::format_context::FormatContext;
use crate::io_context::IOContext;
use crate::probe::ProbeScore;
use crate::stream::Stream;
use bytes::Bytes;
use rsmpeg_codec::{CodecId, H264BitstreamFormat, Packet, PacketFlags};
use rsmpeg_util::{MediaType, PixelFormat, Rational, RsError, RsResult, SampleFormat};
use std::io::SeekFrom;

/// Largest single chunk payload accepted as a packet.
const MAX_PACKET_BYTES: u32 = 32 * 1024 * 1024;
/// AVI chunk ids carry a two-digit stream number.
const MAX_STREAMS: usize = 100;
/// Upper bound on retained `idx1` entries (~64 MiB of index state).
const MAX_INDEX_ENTRIES: usize = 1 << 21;
/// Cap for header chunks we buffer (`avih`, `strh`, `strf`, `strn`).
const MAX_HEADER_CHUNK_BYTES: u64 = 1 << 20;
const MAX_TOP_LEVEL_CHUNKS: usize = 4096;
const MAX_LIST_CHILDREN: usize = 4096;
/// Non-media chunks (`JUNK`, `ix##`, unknown ids) tolerated per `read_frame`.
const MAX_CHUNKS_PER_READ: usize = 1 << 20;
/// Nesting depth for `LIST rec ` groups inside `movi`.
const MAX_LIST_DEPTH: usize = 8;
/// `idx1` is read in batches of this many 16-byte entries (64 KiB).
const IDX1_BATCH_ENTRIES: usize = 4096;
const AVIIF_KEYFRAME: u32 = 0x10;

fn bad(msg: &'static str) -> RsError {
    RsError::InvalidData(msg.into())
}

fn le_u16(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

fn le_u32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn fourcc_at(b: &[u8], off: usize) -> [u8; 4] {
    [b[off], b[off + 1], b[off + 2], b[off + 3]]
}

/// RIFF chunks are padded to an even length.
fn padded(size: u32) -> u64 {
    size as u64 + (size as u64 & 1)
}

/// Read into `buf` until it is full or the input ends; returns bytes read.
fn read_up_to(io: &mut IOContext, buf: &mut [u8]) -> RsResult<usize> {
    let mut filled = 0usize;
    while filled < buf.len() {
        match io.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(filled)
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

/// `num/den` reduced to lowest terms, if both are non-zero and fit `i32`.
fn reduced_rational(num: u64, den: u64) -> Option<Rational> {
    if num == 0 || den == 0 {
        return None;
    }
    let g = gcd(num, den);
    let (n, d) = (num / g, den / g);
    if n <= i32::MAX as u64 && d <= i32::MAX as u64 {
        Some(Rational::new(n as i32, d as i32))
    } else {
        None
    }
}

/// Convert `ticks` in `tb` to milliseconds (saturating, never panics).
fn ticks_to_ms(ticks: i64, tb: Rational) -> i64 {
    if tb.den <= 0 {
        return 0;
    }
    let ms = (ticks as i128)
        .saturating_mul(tb.num as i128)
        .saturating_mul(1000)
        / tb.den as i128;
    ms.clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

// ───────────────────────── shared RIFF codec helpers ─────────────────────────
// (also used by the Matroska demuxer for `V_MS/VFW/FOURCC` and `A_MS/ACM`)

/// Parsed `BITMAPINFOHEADER`.
#[derive(Debug, Clone)]
pub(crate) struct BitmapInfo {
    pub width: i32,
    pub height: i32,
    pub bit_count: u16,
    pub compression: [u8; 4],
    /// Codec-specific bytes following the 40-byte header.
    pub extradata: Vec<u8>,
}

/// Parse a `BITMAPINFOHEADER` (needs at least 40 bytes).
pub(crate) fn parse_bitmap_info_header(buf: &[u8]) -> Option<BitmapInfo> {
    if buf.len() < 40 {
        return None;
    }
    Some(BitmapInfo {
        width: le_u32(buf, 4) as i32,
        height: le_u32(buf, 8) as i32,
        bit_count: le_u16(buf, 14),
        compression: fourcc_at(buf, 16),
        extradata: buf[40..].to_vec(),
    })
}

/// Parsed `WAVEFORMATEX` (with `WAVE_FORMAT_EXTENSIBLE` resolved).
#[derive(Debug, Clone)]
pub(crate) struct WaveFormat {
    pub format_tag: u16,
    pub channels: u16,
    pub sample_rate: u32,
    pub avg_bytes_per_sec: u32,
    #[allow(dead_code)]
    pub block_align: u16,
    pub bits_per_sample: u16,
    /// `cbSize` trailing bytes (empty for `WAVE_FORMAT_EXTENSIBLE`).
    pub extradata: Vec<u8>,
}

/// Parse a `WAVEFORMATEX` (needs at least the 16-byte `WAVEFORMAT` core).
pub(crate) fn parse_wave_format_ex(buf: &[u8]) -> Option<WaveFormat> {
    if buf.len() < 16 {
        return None;
    }
    let mut format_tag = le_u16(buf, 0);
    let cb_size = if buf.len() >= 18 {
        le_u16(buf, 16) as usize
    } else {
        0
    };
    let extra = if buf.len() > 18 {
        let end = 18usize.saturating_add(cb_size).min(buf.len());
        &buf[18..end]
    } else {
        &[][..]
    };
    let mut extradata = extra.to_vec();
    // WAVE_FORMAT_EXTENSIBLE: the real format tag is the first two bytes of
    // the SubFormat GUID (offset 6 in the extension).
    if format_tag == 0xFFFE && extra.len() >= 22 {
        format_tag = le_u16(extra, 6);
        extradata.clear();
    }
    Some(WaveFormat {
        format_tag,
        channels: le_u16(buf, 2),
        sample_rate: le_u32(buf, 4),
        avg_bytes_per_sec: le_u32(buf, 8),
        block_align: le_u16(buf, 12),
        bits_per_sample: le_u16(buf, 14),
        extradata,
    })
}

/// Map a `WAVEFORMATEX.wFormatTag` (+ bit depth) to a codec and sample format.
pub(crate) fn wave_format_codec(tag: u16, bits: u16) -> (CodecId, Option<SampleFormat>) {
    match tag {
        0x0001 => (
            CodecId::Pcm,
            match bits {
                8 => Some(SampleFormat::U8),
                16 => Some(SampleFormat::S16),
                24 | 32 => Some(SampleFormat::S32),
                _ => None,
            },
        ),
        0x0003 => (
            CodecId::Pcm,
            Some(if bits == 64 {
                SampleFormat::F64
            } else {
                SampleFormat::F32
            }),
        ),
        0x0050 | 0x0055 => (CodecId::Mp3, None),
        0x00FF | 0x706D => (CodecId::Aac, None),
        0xF1AC => (CodecId::Flac, None),
        _ => (CodecId::Unknown, None),
    }
}

/// Pixel format of an uncompressed `BITMAPINFOHEADER` / raw FourCC.
pub(crate) fn raw_pixel_format(fourcc: &[u8], bit_count: u16) -> Option<PixelFormat> {
    if fourcc.len() != 4 {
        return None;
    }
    let up = fourcc.to_ascii_uppercase();
    match up.as_slice() {
        [0, 0, 0, 0] | [3, 0, 0, 0] | b"DIB " | b"RGB " => match bit_count {
            24 => Some(PixelFormat::Bgr24),
            32 => Some(PixelFormat::Bgra),
            8 => Some(PixelFormat::Gray8),
            _ => None,
        },
        b"I420" | b"IYUV" => Some(PixelFormat::Yuv420P),
        b"NV12" => Some(PixelFormat::Nv12),
        b"Y800" => Some(PixelFormat::Gray8),
        _ => None,
    }
}

/// Map a video FourCC (`strh.fccHandler` or `biCompression`) to a codec.
///
/// Matching is ASCII case-insensitive (`mjpg` == `MJPG`).
pub(crate) fn fourcc_to_codec_id(fourcc: &[u8]) -> CodecId {
    if fourcc.len() != 4 {
        return CodecId::Unknown;
    }
    let up = fourcc.to_ascii_uppercase();
    match up.as_slice() {
        // BI_RGB (0) and BI_BITFIELDS (3) are uncompressed.
        [0, 0, 0, 0] | [3, 0, 0, 0] => CodecId::RawVideo,
        b"DIB " | b"RGB " | b"I420" | b"IYUV" | b"NV12" | b"Y800" => CodecId::RawVideo,
        b"H264" | b"X264" | b"AVC1" | b"DAVC" => CodecId::H264,
        b"HEVC" | b"H265" | b"HVC1" | b"HEV1" => CodecId::Hevc,
        b"XVID" | b"DIVX" | b"MP4V" | b"FMP4" | b"DX50" | b"MP42" | b"MP43" => CodecId::Mpeg4,
        b"MJPG" | b"JPEG" => CodecId::Mjpeg,
        b"H263" => CodecId::H263,
        b"VP80" => CodecId::Vp8,
        b"VP90" => CodecId::Vp9,
        b"AV01" => CodecId::Av1,
        b"FFV1" => CodecId::Ffv1,
        b"PNG " => CodecId::Png,
        b"BMP " => CodecId::Bmp,
        _ => CodecId::Unknown,
    }
}

/// H.264 framing: `avcC` extradata (version byte 1) means length-prefixed NALs.
pub(crate) fn h264_format_from_extradata(extradata: Option<&[u8]>) -> H264BitstreamFormat {
    match extradata {
        Some(e) if e.len() >= 5 && e[0] == 1 => H264BitstreamFormat::Avcc {
            nal_length_size: (e[4] & 0x03) + 1,
        },
        _ => H264BitstreamFormat::AnnexB,
    }
}

/// Parse the stream number from a chunk id like `00dc` / `01wb`.
///
/// Two decimal digits are read as decimal; otherwise two hex digits as hex.
fn parse_stream_number(ckid: &[u8; 4]) -> Option<usize> {
    let (a, b) = (ckid[0], ckid[1]);
    if a.is_ascii_digit() && b.is_ascii_digit() {
        return Some(((a - b'0') as usize) * 10 + (b - b'0') as usize);
    }
    let hi = (a as char).to_digit(16)?;
    let lo = (b as char).to_digit(16)?;
    Some((hi * 16 + lo) as usize)
}

fn is_intra_only(codec: CodecId) -> bool {
    matches!(
        codec,
        CodecId::Mjpeg | CodecId::RawVideo | CodecId::Png | CodecId::Bmp
    )
}

// ───────────────────────────────── demuxer ──────────────────────────────────

/// Per-stream timing state.
#[derive(Debug, Clone)]
struct AviStream {
    /// Index into `FormatContext::streams`.
    stream_index: usize,
    media_type: MediaType,
    /// `strh.dwSampleSize`: >0 means each chunk holds `len / sample_size` ticks.
    sample_size: u32,
    /// `strh.dwStart`, the first tick.
    start: i64,
    time_base: Rational,
    intra_only: bool,
}

impl AviStream {
    fn chunk_ticks(&self, size: u32) -> u32 {
        size.checked_div(self.sample_size).unwrap_or(1)
    }
}

/// One usable `idx1` entry.
#[derive(Debug, Clone, Copy)]
struct IndexEntry {
    /// Absolute file offset of the chunk header (`ckid`).
    offset: u64,
    pts: i64,
    size: u32,
    duration: u32,
    stream: u8,
    key: bool,
}

/// Cursor for index-less sequential `movi` streaming.
#[derive(Debug, Clone, Default)]
struct ScanState {
    pos: u64,
    /// End offsets of the `LIST rec ` groups we are inside.
    list_ends: Vec<u64>,
    /// Next tick per AVI stream.
    ticks: Vec<i64>,
}

/// A media chunk located by [`AVIDemuxer::next_chunk`].
#[derive(Debug, Clone, Copy)]
struct ChunkRef {
    stream: usize,
    header_pos: u64,
    size: u32,
    pts: i64,
    duration: u32,
}

#[derive(Debug, Default, Clone, Copy)]
struct MainHeader {
    us_per_frame: u32,
    total_frames: u32,
    width: u32,
    height: u32,
}

/// AVI (Audio Video Interleave) demuxer.
#[derive(Debug, Default)]
pub struct AVIDemuxer {
    header_parsed: bool,
    file_len: u64,
    streams: Vec<AviStream>,
    main: MainHeader,
    /// Position of the `movi` FourCC (base for relative `idx1` offsets).
    movi_list_type_pos: u64,
    movi_data_start: u64,
    movi_end: u64,
    index: Vec<IndexEntry>,
    entry_cursor: usize,
    scan: ScanState,
}

impl InputFormat for AVIDemuxer {
    fn name(&self) -> &'static str {
        "avi"
    }

    fn description(&self) -> &'static str {
        "AVI (Audio Video Interleave)"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["avi"]
    }

    fn probe(&self, buf: &[u8]) -> ProbeScore {
        if buf.len() >= 12 && &buf[0..4] == b"RIFF" && &buf[8..12] == b"AVI " {
            ProbeScore::Certain
        } else {
            ProbeScore::NoMatch
        }
    }

    fn read_header(&mut self, ctx: &mut FormatContext) -> RsResult<()> {
        *self = AVIDemuxer::default();
        let io = ctx
            .io
            .as_mut()
            .ok_or_else(|| RsError::InvalidData("No IO context".into()))?;
        let file_len = io.seek(SeekFrom::End(0))?;
        self.file_len = file_len;
        io.seek(SeekFrom::Start(0))?;

        let mut riff = [0u8; 12];
        if read_up_to(io, &mut riff)? < 12 || &riff[0..4] != b"RIFF" || &riff[8..12] != b"AVI " {
            return Err(bad("AVI: missing RIFF/AVI header"));
        }

        let base_index = ctx.streams.len();
        let mut new_streams: Vec<Stream> = Vec::new();
        let mut hdrl_seen = false;
        let mut movi: Option<(u64, u64)> = None;
        let mut idx1: Option<(u64, u32)> = None;

        // The RIFF size is frequently wrong (or followed by `RIFF AVIX`
        // extensions), so top-level chunks are walked up to the file length.
        let mut pos = 12u64;
        for _ in 0..MAX_TOP_LEVEL_CHUNKS {
            let Some((id, size)) = read_chunk_header(io, pos, file_len)? else {
                break;
            };
            let data = pos + 8;
            let mut next = data.saturating_add(padded(size));
            match &id {
                b"LIST" => {
                    let Some(kind) = read_fourcc(io, data, file_len)? else {
                        break;
                    };
                    let list_end = if size < 4 {
                        file_len
                    } else {
                        data.saturating_add(size as u64).min(file_len)
                    };
                    match &kind {
                        b"hdrl" if !hdrl_seen => {
                            hdrl_seen = true;
                            self.parse_hdrl(io, data + 4, list_end, base_index, &mut new_streams)?;
                        }
                        b"movi" if movi.is_none() => {
                            movi = Some((data, list_end));
                            if size < 4 {
                                // Unfinished capture: `movi` runs to EOF.
                                next = file_len;
                            }
                        }
                        _ => {}
                    }
                }
                b"idx1" if idx1.is_none() => idx1 = Some((data, size)),
                _ => {}
            }
            if next <= pos || next >= file_len {
                break;
            }
            pos = next;
        }

        if !hdrl_seen {
            return Err(bad("AVI: missing LIST hdrl"));
        }
        if new_streams.is_empty() {
            return Err(bad("AVI: no stream headers (LIST strl) found"));
        }

        match movi {
            Some((type_pos, end)) => {
                self.movi_list_type_pos = type_pos;
                self.movi_data_start = (type_pos + 4).min(end);
                self.movi_end = end;
            }
            None => {
                tracing::warn!("AVI: no LIST movi found; stream has no packets");
                self.movi_list_type_pos = file_len;
                self.movi_data_start = file_len;
                self.movi_end = file_len;
            }
        }

        let mut index_end_ticks: Option<Vec<i64>> = None;
        if let Some((data, size)) = idx1 {
            if movi.is_some() {
                index_end_ticks = Some(self.parse_idx1(io, data, size)?);
            }
        }
        if self.index.is_empty() {
            index_end_ticks = None;
        }

        // Durations: prefer strh.dwLength, else what the index accumulated.
        let mut duration_ms = 0i64;
        for (n, avi) in self.streams.iter().enumerate() {
            let stream = &mut new_streams[n];
            if stream.duration <= 0 {
                if let Some(ticks) = &index_end_ticks {
                    stream.duration = ticks[n].saturating_sub(avi.start).max(0);
                }
            }
            duration_ms = duration_ms.max(ticks_to_ms(stream.duration, avi.time_base));
        }
        if duration_ms == 0 && self.main.us_per_frame > 0 {
            duration_ms = (self.main.total_frames as i64 * self.main.us_per_frame as i64) / 1000;
        }
        ctx.duration = duration_ms;
        ctx.streams.extend(new_streams);

        self.reset_scan();
        io.seek(SeekFrom::Start(self.movi_data_start))?;
        self.header_parsed = true;
        tracing::info!(
            "AVI: {} stream(s), {} idx1 entries, duration {} ms",
            self.streams.len(),
            self.index.len(),
            ctx.duration
        );
        Ok(())
    }

    fn read_frame(&mut self, ctx: &mut FormatContext) -> RsResult<Option<Packet>> {
        if !self.header_parsed {
            return Ok(None);
        }
        let io = ctx
            .io
            .as_mut()
            .ok_or_else(|| RsError::InvalidData("No IO context".into()))?;

        if !self.index.is_empty() {
            let Some(e) = self.index.get(self.entry_cursor).copied() else {
                return Ok(None);
            };
            self.entry_cursor += 1;
            io.seek(SeekFrom::Start(e.offset + 8))?;
            let data = io.read_bytes(e.size as usize)?;
            let st = &self.streams[e.stream as usize];
            return Ok(Some(make_packet(
                data, st, e.pts, e.duration, e.key, e.offset,
            )));
        }

        let Some(c) = self.next_chunk(io)? else {
            return Ok(None);
        };
        io.seek(SeekFrom::Start(c.header_pos + 8))?;
        let data = io.read_bytes(c.size as usize)?;
        let st = &self.streams[c.stream];
        let key = st.media_type == MediaType::Audio || st.intra_only;
        Ok(Some(make_packet(
            data,
            st,
            c.pts,
            c.duration,
            key,
            c.header_pos,
        )))
    }

    fn seek(&mut self, ctx: &mut FormatContext, timestamp: i64) -> RsResult<()> {
        if !self.header_parsed {
            return Err(bad("AVI: seek before read_header"));
        }
        let target = timestamp.max(0);
        let primary = self.primary_stream();

        if !self.index.is_empty() {
            let mut chosen = 0usize;
            if let Some(p) = primary {
                let tb = self.streams[p].time_base;
                for (i, e) in self.index.iter().enumerate() {
                    if e.stream as usize != p || !e.key {
                        continue;
                    }
                    if ticks_to_ms(e.pts, tb) <= target {
                        chosen = i;
                    } else {
                        break;
                    }
                }
            }
            self.entry_cursor = chosen;
            return Ok(());
        }

        self.reset_scan();
        let Some(p) = primary else {
            return Ok(());
        };
        if target == 0 {
            return Ok(());
        }
        let io = ctx
            .io
            .as_mut()
            .ok_or_else(|| RsError::InvalidData("No IO context".into()))?;
        let tb = self.streams[p].time_base;
        let mut best = self.scan.clone();
        // Without an index every primary-stream chunk is a candidate seek point.
        while let Ok(Some(c)) = self.next_chunk(io) {
            if c.stream != p {
                continue;
            }
            if ticks_to_ms(c.pts, tb) > target {
                break;
            }
            // Rewind the post-chunk state to just before this chunk.
            let mut snap = self.scan.clone();
            snap.pos = c.header_pos;
            snap.ticks[p] = c.pts;
            best = snap;
        }
        self.scan = best;
        Ok(())
    }
}

fn make_packet(
    data: Vec<u8>,
    st: &AviStream,
    pts: i64,
    duration: u32,
    key: bool,
    pos: u64,
) -> Packet {
    let mut pkt = Packet::new(Bytes::from(data), st.stream_index);
    pkt.pts = Some(pts);
    pkt.dts = Some(pts);
    pkt.duration = duration as i64;
    pkt.time_base = st.time_base;
    pkt.pos = i64::try_from(pos).unwrap_or(i64::MAX);
    if key {
        pkt.flags |= PacketFlags::KEY;
    }
    pkt
}

/// Read a chunk header at `pos` if all 8 bytes lie below `limit`.
fn read_chunk_header(io: &mut IOContext, pos: u64, limit: u64) -> RsResult<Option<([u8; 4], u32)>> {
    if pos.checked_add(8).map_or(true, |e| e > limit) {
        return Ok(None);
    }
    io.seek(SeekFrom::Start(pos))?;
    let mut b = [0u8; 8];
    if read_up_to(io, &mut b)? < 8 {
        return Ok(None);
    }
    Ok(Some((fourcc_at(&b, 0), le_u32(&b, 4))))
}

fn read_fourcc(io: &mut IOContext, pos: u64, limit: u64) -> RsResult<Option<[u8; 4]>> {
    if pos.checked_add(4).map_or(true, |e| e > limit) {
        return Ok(None);
    }
    io.seek(SeekFrom::Start(pos))?;
    let mut b = [0u8; 4];
    if read_up_to(io, &mut b)? < 4 {
        return Ok(None);
    }
    Ok(Some(b))
}

/// Read up to `min(size, MAX_HEADER_CHUNK_BYTES)` bytes of a header chunk,
/// clipped to `limit` (a truncated chunk yields the bytes that exist).
fn read_header_chunk(io: &mut IOContext, data: u64, size: u32, limit: u64) -> RsResult<Vec<u8>> {
    let n = (size as u64)
        .min(MAX_HEADER_CHUNK_BYTES)
        .min(limit.saturating_sub(data));
    io.seek(SeekFrom::Start(data))?;
    let mut buf = vec![0u8; n as usize];
    let got = read_up_to(io, &mut buf)?;
    buf.truncate(got);
    Ok(buf)
}

impl AVIDemuxer {
    fn reset_scan(&mut self) {
        self.entry_cursor = 0;
        self.scan = ScanState {
            pos: self.movi_data_start,
            list_ends: Vec::new(),
            ticks: self.streams.iter().map(|s| s.start).collect(),
        };
    }

    /// First video stream, else first audio stream, else stream 0.
    fn primary_stream(&self) -> Option<usize> {
        self.streams
            .iter()
            .position(|s| s.media_type == MediaType::Video)
            .or_else(|| {
                self.streams
                    .iter()
                    .position(|s| s.media_type == MediaType::Audio)
            })
            .or(if self.streams.is_empty() {
                None
            } else {
                Some(0)
            })
    }

    fn parse_hdrl(
        &mut self,
        io: &mut IOContext,
        start: u64,
        end: u64,
        base_index: usize,
        out: &mut Vec<Stream>,
    ) -> RsResult<()> {
        let mut pos = start;
        for _ in 0..MAX_LIST_CHILDREN {
            let Some((id, size)) = read_chunk_header(io, pos, end)? else {
                break;
            };
            let data = pos + 8;
            match &id {
                b"avih" => {
                    let b = read_header_chunk(io, data, size, end)?;
                    if b.len() >= 40 {
                        self.main = MainHeader {
                            us_per_frame: le_u32(&b, 0),
                            total_frames: le_u32(&b, 16),
                            width: le_u32(&b, 32),
                            height: le_u32(&b, 36),
                        };
                    }
                }
                b"LIST" => {
                    if let Some(kind) = read_fourcc(io, data, end)? {
                        if &kind == b"strl" && size >= 4 {
                            if self.streams.len() >= MAX_STREAMS {
                                tracing::warn!(
                                    "AVI: more than {MAX_STREAMS} streams; ignoring the rest"
                                );
                            } else {
                                let list_end = data.saturating_add(size as u64).min(end);
                                let index = base_index + out.len();
                                let stream = self.parse_strl(io, data + 4, list_end, index)?;
                                out.push(stream);
                            }
                        }
                    }
                }
                _ => {}
            }
            let next = data.saturating_add(padded(size));
            if next <= pos {
                break;
            }
            pos = next;
        }
        Ok(())
    }

    /// Parse one `LIST strl`. Always yields a stream (possibly `Data`/`Unknown`)
    /// so AVI stream numbers in chunk ids stay aligned with stream order.
    fn parse_strl(
        &mut self,
        io: &mut IOContext,
        start: u64,
        end: u64,
        index: usize,
    ) -> RsResult<Stream> {
        let mut strh: Option<Vec<u8>> = None;
        let mut strf: Option<Vec<u8>> = None;
        let mut name: Option<String> = None;
        let mut pos = start;
        for _ in 0..MAX_LIST_CHILDREN {
            let Some((id, size)) = read_chunk_header(io, pos, end)? else {
                break;
            };
            let data = pos + 8;
            match &id {
                b"strh" if strh.is_none() => strh = Some(read_header_chunk(io, data, size, end)?),
                b"strf" if strf.is_none() => strf = Some(read_header_chunk(io, data, size, end)?),
                b"strn" if name.is_none() => {
                    let b = read_header_chunk(io, data, size.min(1024), end)?;
                    let text = b.split(|&c| c == 0).next().unwrap_or(&[]);
                    let text = String::from_utf8_lossy(text).trim().to_string();
                    if !text.is_empty() {
                        name = Some(text);
                    }
                }
                _ => {}
            }
            let next = data.saturating_add(padded(size));
            if next <= pos {
                break;
            }
            pos = next;
        }

        let strh = strh.filter(|b| b.len() >= 48);
        let (fcc_type, handler, scale, rate, start_ticks, length, sample_size) = match &strh {
            Some(b) => (
                fourcc_at(b, 0),
                fourcc_at(b, 4),
                le_u32(b, 20),
                le_u32(b, 24),
                le_u32(b, 28),
                le_u32(b, 32),
                le_u32(b, 44),
            ),
            None => {
                tracing::warn!("AVI: strl #{index} has no usable strh");
                ([0; 4], [0; 4], 0, 0, 0, 0, 0)
            }
        };

        let mut stream;
        let mut time_base = reduced_rational(scale as u64, rate as u64);
        let mut intra_only = false;
        match &fcc_type {
            b"vids" => {
                let bih = strf.as_deref().and_then(parse_bitmap_info_header);
                let mut codec = bih
                    .as_ref()
                    .map_or(CodecId::Unknown, |b| fourcc_to_codec_id(&b.compression));
                if codec == CodecId::Unknown {
                    codec = fourcc_to_codec_id(&handler);
                }
                stream = Stream::new(index, codec);
                stream.media_type = MediaType::Video;
                let p = &mut stream.codec_params;
                p.media_type = MediaType::Video;
                let (mut w, mut h) = (self.main.width as usize, self.main.height as usize);
                if let Some(b) = &bih {
                    if b.width != 0 && b.height != 0 {
                        w = b.width.unsigned_abs() as usize;
                        h = b.height.unsigned_abs() as usize;
                    }
                    if codec == CodecId::RawVideo {
                        p.pixel_format = raw_pixel_format(&b.compression, b.bit_count)
                            .or_else(|| raw_pixel_format(&handler, b.bit_count));
                    }
                    if !b.extradata.is_empty() {
                        p.extradata = Some(b.extradata.clone());
                    }
                }
                if w > 0 && h > 0 {
                    p.width = Some(w);
                    p.height = Some(h);
                }
                if codec == CodecId::H264 {
                    p.h264_bitstream_format = h264_format_from_extradata(p.extradata.as_deref());
                }
                if time_base.is_none() && self.main.us_per_frame > 0 {
                    time_base = reduced_rational(self.main.us_per_frame as u64, 1_000_000);
                }
                let tb = *time_base.get_or_insert(Rational::new(1, 25));
                let fps = reduced_rational(rate as u64, scale as u64)
                    .unwrap_or(Rational::new(tb.den, tb.num.max(1)));
                stream.avg_frame_rate = fps;
                stream.r_frame_rate = fps;
                intra_only = is_intra_only(codec);
            }
            b"auds" => {
                let wf = strf.as_deref().and_then(parse_wave_format_ex);
                let (codec, sample_format) = wf.as_ref().map_or((CodecId::Unknown, None), |w| {
                    wave_format_codec(w.format_tag, w.bits_per_sample)
                });
                stream = Stream::new(index, codec);
                stream.media_type = MediaType::Audio;
                let p = &mut stream.codec_params;
                p.media_type = MediaType::Audio;
                p.sample_format = sample_format;
                if let Some(w) = &wf {
                    if w.sample_rate > 0 {
                        p.sample_rate = Some(w.sample_rate);
                    }
                    if w.channels > 0 {
                        p.channels = Some(w.channels);
                    }
                    if w.avg_bytes_per_sec > 0 {
                        p.bit_rate = Some(w.avg_bytes_per_sec as u64 * 8);
                    }
                    if !w.extradata.is_empty() {
                        p.extradata = Some(w.extradata.clone());
                    }
                    if time_base.is_none() {
                        time_base = reduced_rational(1, w.sample_rate as u64);
                    }
                }
            }
            b"txts" => {
                stream = Stream::new(index, CodecId::Unknown);
                stream.media_type = MediaType::Subtitle;
                stream.codec_params.media_type = MediaType::Subtitle;
            }
            _ => {
                stream = Stream::new(index, CodecId::Unknown);
                stream.media_type = MediaType::Data;
                stream.codec_params.media_type = MediaType::Data;
            }
        }
        let time_base = time_base.unwrap_or(Rational::new(1, 1000));
        stream.time_base = time_base;
        stream.duration = length as i64;
        if let Some(n) = name {
            stream.metadata.insert("title", n);
        }
        self.streams.push(AviStream {
            stream_index: index,
            media_type: stream.media_type,
            sample_size,
            start: start_ticks as i64,
            time_base,
            intra_only,
        });
        Ok(stream)
    }

    /// Decide whether `idx1` offsets are relative to the `movi` FourCC or
    /// absolute, by checking which candidate actually holds the entry's
    /// `ckid`. Several leading entries are probed so one corrupt entry cannot
    /// pin the wrong base; without any match, the classic heuristic applies
    /// (offsets below the `movi` position must be relative).
    fn detect_idx1_base(&self, io: &mut IOContext, data: u64, total: usize) -> RsResult<u64> {
        const MAX_PROBES: usize = 32;
        let relative = self.movi_list_type_pos;
        let n = total.min(IDX1_BATCH_ENTRIES);
        io.seek(SeekFrom::Start(data))?;
        let buf = io.read_bytes(n * 16)?;
        let mut fallback: Option<u64> = None;
        let mut probes = 0usize;
        for e in buf.chunks_exact(16) {
            let ckid = fourcc_at(e, 0);
            if parse_stream_number(&ckid)
                .filter(|&s| s < self.streams.len())
                .is_none()
            {
                continue;
            }
            let off = le_u32(e, 8) as u64;
            fallback.get_or_insert(if off < relative { relative } else { 0 });
            for base in [relative, 0] {
                if let Some(at) = base.checked_add(off) {
                    if read_fourcc(io, at, self.file_len)? == Some(ckid) {
                        return Ok(base);
                    }
                }
            }
            probes += 1;
            if probes >= MAX_PROBES {
                break;
            }
        }
        Ok(fallback.unwrap_or(relative))
    }

    /// Load `idx1` into `self.index`; returns the per-stream end ticks.
    fn parse_idx1(&mut self, io: &mut IOContext, data: u64, size: u32) -> RsResult<Vec<i64>> {
        let avail = self.file_len.saturating_sub(data) / 16;
        let total = (size as u64 / 16).min(avail).min(MAX_INDEX_ENTRIES as u64) as usize;
        let mut ticks: Vec<i64> = self.streams.iter().map(|s| s.start).collect();
        let base = self.detect_idx1_base(io, data, total)?;
        let mut done = 0usize;
        while done < total {
            let batch = (total - done).min(IDX1_BATCH_ENTRIES);
            io.seek(SeekFrom::Start(data + (done as u64) * 16))?;
            let buf = io.read_bytes(batch * 16)?;
            done += batch;
            for e in buf.chunks_exact(16) {
                let ckid = fourcc_at(e, 0);
                let flags = le_u32(e, 4);
                let off = le_u32(e, 8);
                let len = le_u32(e, 12);
                let Some(sn) = parse_stream_number(&ckid).filter(|&n| n < self.streams.len())
                else {
                    continue; // `rec `, `7Fxx`, junk…
                };
                let st = &self.streams[sn];
                let dur = st.chunk_ticks(len);
                let pts = ticks[sn];
                ticks[sn] = pts.saturating_add(dur as i64);
                if len == 0 || len > MAX_PACKET_BYTES {
                    continue;
                }
                let Some(offset) = base.checked_add(off as u64) else {
                    continue;
                };
                let in_file = offset
                    .checked_add(8 + len as u64)
                    .is_some_and(|end| end <= self.file_len);
                if !in_file || self.index.len() >= MAX_INDEX_ENTRIES {
                    continue;
                }
                let key = flags & AVIIF_KEYFRAME != 0
                    || st.media_type == MediaType::Audio
                    || st.intra_only;
                self.index.push(IndexEntry {
                    offset,
                    pts,
                    size: len,
                    duration: dur,
                    stream: sn as u8,
                    key,
                });
            }
        }
        Ok(ticks)
    }

    /// Advance the sequential `movi` cursor to the next media chunk.
    fn next_chunk(&mut self, io: &mut IOContext) -> RsResult<Option<ChunkRef>> {
        for _ in 0..MAX_CHUNKS_PER_READ {
            while let Some(&end) = self.scan.list_ends.last() {
                if self.scan.pos >= end {
                    self.scan.list_ends.pop();
                } else {
                    break;
                }
            }
            let limit = self.scan.list_ends.last().copied().unwrap_or(self.movi_end);
            let pos = self.scan.pos;
            let Some((id, size)) = read_chunk_header(io, pos, limit)? else {
                if let Some(end) = self.scan.list_ends.pop() {
                    self.scan.pos = end.max(pos);
                    continue;
                }
                self.scan.pos = self.movi_end;
                return Ok(None);
            };
            let data = pos + 8;
            let next = data.saturating_add(padded(size));
            if &id == b"LIST" {
                if size >= 4 && self.scan.list_ends.len() < MAX_LIST_DEPTH {
                    if let Some(kind) = read_fourcc(io, data, limit)? {
                        if &kind == b"rec " {
                            self.scan.list_ends.push(next.min(limit));
                            self.scan.pos = data + 4;
                            continue;
                        }
                    }
                }
                self.scan.pos = next;
                continue;
            }
            self.scan.pos = next;
            let Some(sn) = parse_stream_number(&id).filter(|&n| n < self.streams.len()) else {
                continue; // JUNK, ix##, unknown
            };
            let dur = self.streams[sn].chunk_ticks(size);
            let pts = self.scan.ticks[sn];
            self.scan.ticks[sn] = pts.saturating_add(dur as i64);
            if size == 0 {
                continue;
            }
            if size > MAX_PACKET_BYTES {
                return Err(RsError::InvalidData(
                    format!("AVI: chunk of {size} bytes at offset {pos} exceeds the packet cap")
                        .into(),
                ));
            }
            if data + size as u64 > self.movi_end {
                // Truncated final chunk.
                self.scan.pos = self.movi_end;
                return Ok(None);
            }
            return Ok(Some(ChunkRef {
                stream: sn,
                header_pos: pos,
                size,
                pts,
                duration: dur,
            }));
        }
        Err(bad("AVI: too many non-media chunks in movi"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io_context::IOContext;

    fn ctx_with(data: Vec<u8>) -> FormatContext {
        FormatContext {
            input: None,
            output: None,
            streams: Vec::new(),
            io: Some(IOContext::from_buffer(data)),
            metadata: rsmpeg_util::Dict::new(),
            duration: 0,
            bit_rate: 0,
            filename: None,
            format_name: Some("avi".into()),
        }
    }

    fn chunk(id: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut v = id.to_vec();
        v.extend_from_slice(&(data.len() as u32).to_le_bytes());
        v.extend_from_slice(data);
        if data.len() % 2 == 1 {
            v.push(0);
        }
        v
    }

    fn list(kind: &[u8; 4], children: &[u8]) -> Vec<u8> {
        let mut body = kind.to_vec();
        body.extend_from_slice(children);
        chunk(b"LIST", &body)
    }

    fn u32s(vals: &[u32]) -> Vec<u8> {
        vals.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    fn avih(us_per_frame: u32, frames: u32, streams: u32, w: u32, h: u32) -> Vec<u8> {
        chunk(
            b"avih",
            &u32s(&[
                us_per_frame,
                0,
                0,
                0x10,
                frames,
                0,
                streams,
                0,
                w,
                h,
                0,
                0,
                0,
                0,
            ]),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn strh(
        kind: &[u8; 4],
        handler: &[u8; 4],
        scale: u32,
        rate: u32,
        start: u32,
        length: u32,
        sample_size: u32,
    ) -> Vec<u8> {
        let mut b = kind.to_vec();
        b.extend_from_slice(handler);
        b.extend(u32s(&[
            0,
            0,
            0,
            scale,
            rate,
            start,
            length,
            0,
            0,
            sample_size,
        ]));
        b.extend([0u8; 8]); // rcFrame
        chunk(b"strh", &b)
    }

    fn bih(w: i32, h: i32, bits: u16, compression: &[u8; 4], extra: &[u8]) -> Vec<u8> {
        let mut b = u32s(&[40, w as u32, h as u32]);
        b.extend(1u16.to_le_bytes());
        b.extend(bits.to_le_bytes());
        b.extend_from_slice(compression);
        b.extend([0u8; 20]);
        b.extend_from_slice(extra);
        chunk(b"strf", &b)
    }

    fn wfx(tag: u16, ch: u16, rate: u32, align: u16, bits: u16) -> Vec<u8> {
        let mut b = tag.to_le_bytes().to_vec();
        b.extend(ch.to_le_bytes());
        b.extend(rate.to_le_bytes());
        b.extend((rate * align as u32).to_le_bytes());
        b.extend(align.to_le_bytes());
        b.extend(bits.to_le_bytes());
        b.extend(0u16.to_le_bytes());
        chunk(b"strf", &b)
    }

    /// MJPEG 25 fps 320x240 + PCM s16 stereo 44.1 kHz header.
    fn mjpeg_pcm_hdrl() -> Vec<u8> {
        let v = list(
            b"strl",
            &[
                strh(b"vids", b"MJPG", 1, 25, 0, 3, 0),
                bih(320, 240, 24, b"MJPG", &[]),
                chunk(b"strn", b"camera\0"),
            ]
            .concat(),
        );
        let a = list(
            b"strl",
            &[
                strh(b"auds", &[0; 4], 4, 176_400, 0, 882, 4),
                wfx(1, 2, 44_100, 4, 16),
            ]
            .concat(),
        );
        list(b"hdrl", &[avih(40_000, 3, 2, 320, 240), v, a].concat())
    }

    struct Movi {
        bytes: Vec<u8>,
        /// (ckid, offset relative to the `movi` FourCC, len)
        entries: Vec<([u8; 4], u32, u32)>,
    }

    fn movi(chunks: &[(&[u8; 4], Vec<u8>)]) -> Movi {
        let mut bytes = Vec::new();
        let mut entries = Vec::new();
        for (id, data) in chunks {
            entries.push((**id, 4 + bytes.len() as u32, data.len() as u32));
            bytes.extend(chunk(id, data));
        }
        Movi { bytes, entries }
    }

    fn idx1(entries: &[([u8; 4], u32, u32)], keys: &[bool], base: u32) -> Vec<u8> {
        let mut b = Vec::new();
        for (i, (id, off, len)) in entries.iter().enumerate() {
            b.extend_from_slice(id);
            b.extend((if keys[i] { AVIIF_KEYFRAME } else { 0 }).to_le_bytes());
            b.extend((off + base).to_le_bytes());
            b.extend(len.to_le_bytes());
        }
        chunk(b"idx1", &b)
    }

    fn riff(parts: &[Vec<u8>]) -> Vec<u8> {
        let body = parts.concat();
        let mut v = b"RIFF".to_vec();
        v.extend(((body.len() + 4) as u32).to_le_bytes());
        v.extend_from_slice(b"AVI ");
        v.extend(body);
        v
    }

    fn build(hdrl: Vec<u8>, m: &Movi, keys: Option<&[bool]>, absolute: bool) -> Vec<u8> {
        let movi_list = list(b"movi", &m.bytes);
        let movi_type_pos = (12 + hdrl.len() + 8) as u32;
        let mut parts = vec![hdrl, movi_list];
        if let Some(k) = keys {
            parts.push(idx1(
                &m.entries,
                k,
                if absolute { movi_type_pos } else { 0 },
            ));
        }
        riff(&parts)
    }

    fn sample_movi() -> Movi {
        movi(&[
            (b"00dc", vec![0xFF, 0xD8, 1, 2, 0xD9]), // odd length → padded
            (b"01wb", vec![1u8; 1764]),
            (b"00dc", vec![0xFF, 0xD8, 3, 4]),
            (b"01wb", vec![2u8; 1764]),
            (b"00dc", vec![0xFF, 0xD8, 5]),
        ])
    }

    fn read_all(ctx: &mut FormatContext, d: &mut AVIDemuxer) -> Vec<Packet> {
        let mut out = Vec::new();
        for _ in 0..10_000 {
            match d.read_frame(ctx) {
                Ok(Some(p)) => out.push(p),
                _ => break,
            }
        }
        out
    }

    fn check_mjpeg_pcm(data: Vec<u8>) {
        let mut ctx = ctx_with(data);
        let mut d = AVIDemuxer::default();
        d.read_header(&mut ctx).unwrap();
        assert_eq!(ctx.streams.len(), 2);
        let v = &ctx.streams[0];
        assert_eq!(v.codec_id, CodecId::Mjpeg);
        assert_eq!(v.media_type, MediaType::Video);
        assert_eq!(v.codec_params.width, Some(320));
        assert_eq!(v.codec_params.height, Some(240));
        assert_eq!(v.time_base, Rational::new(1, 25));
        assert_eq!(v.avg_frame_rate, Rational::new(25, 1));
        assert_eq!(v.metadata.get("title"), Some("camera"));
        let a = &ctx.streams[1];
        assert_eq!(a.codec_id, CodecId::Pcm);
        assert_eq!(a.codec_params.sample_format, Some(SampleFormat::S16));
        assert_eq!(a.codec_params.sample_rate, Some(44_100));
        assert_eq!(a.codec_params.channels, Some(2));
        assert_eq!(a.time_base, Rational::new(1, 44_100));
        assert_eq!(ctx.duration, 120);

        let pkts = read_all(&mut ctx, &mut d);
        let summary: Vec<(usize, i64, i64, usize)> = pkts
            .iter()
            .map(|p| (p.stream_index, p.pts.unwrap(), p.duration, p.data.len()))
            .collect();
        assert_eq!(
            summary,
            vec![
                (0, 0, 1, 5),
                (1, 0, 441, 1764),
                (0, 1, 1, 4),
                (1, 441, 441, 1764),
                (0, 2, 1, 3)
            ]
        );
        assert_eq!(&pkts[0].data[..], &[0xFF, 0xD8, 1, 2, 0xD9]);
        assert!(pkts.iter().all(|p| p.is_key()));
        assert_eq!(pkts[3].time_base, Rational::new(1, 44_100));
    }

    #[test]
    fn mjpeg_pcm_with_relative_idx1() {
        let m = sample_movi();
        check_mjpeg_pcm(build(mjpeg_pcm_hdrl(), &m, Some(&[true; 5]), false));
    }

    #[test]
    fn mjpeg_pcm_with_absolute_idx1() {
        let m = sample_movi();
        check_mjpeg_pcm(build(mjpeg_pcm_hdrl(), &m, Some(&[true; 5]), true));
    }

    #[test]
    fn mjpeg_pcm_without_idx1() {
        let m = sample_movi();
        check_mjpeg_pcm(build(mjpeg_pcm_hdrl(), &m, None, false));
    }

    #[test]
    fn no_idx1_with_rec_lists_junk_and_padding() {
        let mut body = list(
            b"rec ",
            &[chunk(b"00dc", &[1, 2, 3]), chunk(b"01wb", &[9; 8])].concat(),
        );
        body.extend(chunk(b"JUNK", &[0; 5]));
        body.extend(chunk(b"ix00", &[0; 8]));
        body.extend(chunk(b"00dc", &[4, 5, 6, 7]));
        body.extend(list(b"rec ", &chunk(b"01wb", &[8; 4])));
        body.extend(chunk(b"00dc", &[]));
        body.extend(chunk(b"00dc", &[0xAA]));
        let m = Movi {
            bytes: body,
            entries: Vec::new(),
        };
        let mut ctx = ctx_with(build(mjpeg_pcm_hdrl(), &m, None, false));
        let mut d = AVIDemuxer::default();
        d.read_header(&mut ctx).unwrap();
        let pkts = read_all(&mut ctx, &mut d);
        let summary: Vec<(usize, i64, Vec<u8>)> = pkts
            .iter()
            .map(|p| (p.stream_index, p.pts.unwrap(), p.data.to_vec()))
            .collect();
        assert_eq!(
            summary,
            vec![
                (0, 0, vec![1, 2, 3]),
                (1, 0, vec![9; 8]),
                (0, 1, vec![4, 5, 6, 7]),
                (1, 2, vec![8; 4]),
                // the empty 00dc chunk consumed tick 2
                (0, 3, vec![0xAA]),
            ]
        );

        // Index-less seek: at 25 fps, 60 ms falls between video tick 1 (40 ms)
        // and the next emitted video chunk (tick 3, 120 ms) → resume at tick 1.
        d.seek(&mut ctx, 60).unwrap();
        let p = d.read_frame(&mut ctx).unwrap().unwrap();
        assert_eq!((p.stream_index, p.pts), (0, Some(1)));
        d.seek(&mut ctx, 0).unwrap();
        let p = d.read_frame(&mut ctx).unwrap().unwrap();
        assert_eq!((p.stream_index, p.pts), (0, Some(0)));
        d.seek(&mut ctx, 10_000).unwrap();
        let p = d.read_frame(&mut ctx).unwrap().unwrap();
        assert_eq!((p.stream_index, p.pts), (0, Some(3)));
        assert!(d.read_frame(&mut ctx).unwrap().is_none());
    }

    #[test]
    fn seek_with_idx1_lands_on_keyframe() {
        let v = list(
            b"strl",
            &[
                strh(b"vids", b"H264", 1, 25, 0, 6, 0),
                bih(64, 48, 24, b"H264", &[]),
            ]
            .concat(),
        );
        let a = list(
            b"strl",
            &[
                strh(b"auds", &[0; 4], 1152, 44_100, 0, 6, 0),
                wfx(0x55, 2, 44_100, 1, 0),
            ]
            .concat(),
        );
        let hdrl = list(b"hdrl", &[avih(40_000, 6, 2, 64, 48), v, a].concat());
        let mut chunks: Vec<(&[u8; 4], Vec<u8>)> = Vec::new();
        for i in 0..6u8 {
            chunks.push((b"00dc", vec![0, 0, 0, 1, i]));
            chunks.push((b"01wb", vec![0xFF, 0xFB, i]));
        }
        let m = movi(&chunks);
        let keys: Vec<bool> = (0..12).map(|i| i == 0 || i == 6).collect();
        let mut ctx = ctx_with(build(hdrl, &m, Some(&keys), false));
        let mut d = AVIDemuxer::default();
        d.read_header(&mut ctx).unwrap();
        assert_eq!(ctx.streams[0].codec_id, CodecId::H264);
        assert_eq!(
            ctx.streams[0].codec_params.h264_bitstream_format,
            H264BitstreamFormat::AnnexB
        );
        assert_eq!(ctx.streams[1].codec_id, CodecId::Mp3);

        let pkts = read_all(&mut ctx, &mut d);
        assert_eq!(pkts.len(), 12);
        assert!(pkts[0].is_key() && !pkts[2].is_key() && pkts[6].is_key());
        assert!(pkts[1].is_key(), "audio is always key");

        // frame 3 sits at 120 ms
        d.seek(&mut ctx, 150).unwrap();
        let p = d.read_frame(&mut ctx).unwrap().unwrap();
        assert_eq!((p.stream_index, p.pts, p.is_key()), (0, Some(3), true));
        d.seek(&mut ctx, 100).unwrap();
        let p = d.read_frame(&mut ctx).unwrap().unwrap();
        assert_eq!((p.stream_index, p.pts), (0, Some(0)));
        d.seek(&mut ctx, -50).unwrap();
        let p = d.read_frame(&mut ctx).unwrap().unwrap();
        assert_eq!(p.pts, Some(0));
    }

    #[test]
    fn raw_video_and_float_audio_params() {
        let v = list(
            b"strl",
            &[
                strh(b"vids", b"DIB ", 1, 30, 0, 0, 0),
                bih(16, -8, 32, &[0; 4], &[]),
            ]
            .concat(),
        );
        let a = list(
            b"strl",
            &[
                strh(b"auds", &[0; 4], 8, 384_000, 0, 0, 8),
                wfx(3, 2, 48_000, 8, 32),
            ]
            .concat(),
        );
        let hdrl = list(b"hdrl", &[avih(33_333, 0, 2, 0, 0), v, a].concat());
        let m = movi(&[(b"00db", vec![0; 16 * 8 * 4]), (b"01wb", vec![0; 80])]);
        let mut ctx = ctx_with(build(hdrl, &m, Some(&[true, true]), false));
        let mut d = AVIDemuxer::default();
        d.read_header(&mut ctx).unwrap();
        let v = &ctx.streams[0].codec_params;
        assert_eq!(ctx.streams[0].codec_id, CodecId::RawVideo);
        assert_eq!((v.width, v.height), (Some(16), Some(8)));
        assert_eq!(v.pixel_format, Some(PixelFormat::Bgra));
        let a = &ctx.streams[1].codec_params;
        assert_eq!(a.sample_format, Some(SampleFormat::F32));
        // duration recovered from the index when dwLength is 0
        assert_eq!(ctx.streams[1].duration, 10);
        let pkts = read_all(&mut ctx, &mut d);
        assert_eq!(pkts[1].duration, 10);
    }

    #[test]
    fn fourcc_mapping_is_accurate() {
        for f in [b"H264", b"h264", b"x264", b"AVC1", b"avc1"] {
            assert_eq!(fourcc_to_codec_id(f), CodecId::H264, "{f:?}");
        }
        for f in [
            b"XVID", b"xvid", b"DIVX", b"divx", b"MP4V", b"mp4v", b"FMP4", b"DX50", b"MP42",
            b"MP43",
        ] {
            assert_eq!(fourcc_to_codec_id(f), CodecId::Mpeg4, "{f:?}");
        }
        for f in [b"MJPG", b"mjpg", b"JPEG"] {
            assert_eq!(fourcc_to_codec_id(f), CodecId::Mjpeg);
        }
        for f in [&[0u8; 4], b"DIB ", b"RGB ", b"I420", b"IYUV"] {
            assert_eq!(fourcc_to_codec_id(f), CodecId::RawVideo);
        }
        assert_eq!(fourcc_to_codec_id(b"VP80"), CodecId::Vp8);
        assert_eq!(fourcc_to_codec_id(b"VP90"), CodecId::Vp9);
        assert_eq!(fourcc_to_codec_id(b"AV01"), CodecId::Av1);
        assert_eq!(fourcc_to_codec_id(b"PNG "), CodecId::Png);
        assert_eq!(fourcc_to_codec_id(b"BMP "), CodecId::Bmp);
        assert_eq!(fourcc_to_codec_id(b"ABCD"), CodecId::Unknown);
        assert_eq!(fourcc_to_codec_id(b"abc"), CodecId::Unknown);

        assert_eq!(raw_pixel_format(b"DIB ", 24), Some(PixelFormat::Bgr24));
        assert_eq!(raw_pixel_format(&[0; 4], 8), Some(PixelFormat::Gray8));
        assert_eq!(raw_pixel_format(b"I420", 12), Some(PixelFormat::Yuv420P));
        assert_eq!(raw_pixel_format(b"MJPG", 24), None);

        assert_eq!(parse_stream_number(b"00dc"), Some(0));
        assert_eq!(parse_stream_number(b"12wb"), Some(12));
        assert_eq!(parse_stream_number(b"ix00"), None);
        assert_eq!(parse_stream_number(b"7Fxx"), Some(0x7F));
    }

    #[test]
    fn wave_format_extensible_resolves_subformat() {
        let mut b = 0xFFFEu16.to_le_bytes().to_vec();
        b.extend(6u16.to_le_bytes());
        b.extend(48_000u32.to_le_bytes());
        b.extend((48_000u32 * 18).to_le_bytes());
        b.extend(18u16.to_le_bytes());
        b.extend(24u16.to_le_bytes());
        b.extend(22u16.to_le_bytes());
        b.extend(24u16.to_le_bytes()); // valid bits
        b.extend(0x3Fu32.to_le_bytes()); // channel mask
        b.extend(1u16.to_le_bytes()); // KSDATAFORMAT_SUBTYPE_PCM
        b.extend([0u8; 14]);
        let w = parse_wave_format_ex(&b).unwrap();
        assert_eq!(w.format_tag, 1);
        assert_eq!(w.channels, 6);
        assert!(w.extradata.is_empty());
        assert_eq!(
            wave_format_codec(w.format_tag, w.bits_per_sample).1,
            Some(SampleFormat::S32)
        );
        assert!(parse_wave_format_ex(&[0; 10]).is_none());
        assert!(parse_bitmap_info_header(&[0; 39]).is_none());
    }

    #[test]
    fn rejects_non_avi_and_missing_hdrl() {
        let mut d = AVIDemuxer::default();
        assert!(d.read_header(&mut ctx_with(Vec::new())).is_err());
        assert!(d
            .read_header(&mut ctx_with(b"RIFF\0\0\0\0WAVE".to_vec()))
            .is_err());
        let no_hdrl = riff(&[list(b"movi", &chunk(b"00dc", &[1]))]);
        assert!(d.read_header(&mut ctx_with(no_hdrl)).is_err());
        let empty_hdrl = riff(&[list(b"hdrl", &avih(1, 1, 1, 1, 1))]);
        assert!(d.read_header(&mut ctx_with(empty_hdrl)).is_err());
    }

    #[test]
    fn idx1_with_bogus_entries_is_filtered() {
        let m = sample_movi();
        let hdrl = mjpeg_pcm_hdrl();
        let movi_list = list(b"movi", &m.bytes);
        let mut idx = Vec::new();
        // garbage entries: unknown stream, huge size, offset past EOF
        for (id, off, len) in [
            (*b"7Fxx", 4u32, 4u32),
            (*b"rec ", 4, 4),
            (*b"00dc", 0xFFFF_FFF0, 4),
            (*b"00dc", 4, 0xFFFF_FFFF),
        ] {
            idx.extend_from_slice(&id);
            idx.extend(AVIIF_KEYFRAME.to_le_bytes());
            idx.extend(off.to_le_bytes());
            idx.extend(len.to_le_bytes());
        }
        let (id, off, len) = m.entries[0];
        idx.extend_from_slice(&id);
        idx.extend(AVIIF_KEYFRAME.to_le_bytes());
        idx.extend(off.to_le_bytes());
        idx.extend(len.to_le_bytes());
        idx.extend([1, 2, 3]); // ragged tail
        let data = riff(&[hdrl, movi_list, chunk(b"idx1", &idx)]);
        let mut ctx = ctx_with(data);
        let mut d = AVIDemuxer::default();
        d.read_header(&mut ctx).unwrap();
        let pkts = read_all(&mut ctx, &mut d);
        assert_eq!(pkts.len(), 1);
        assert_eq!(pkts[0].pts, Some(2), "skipped entries still advance ticks");
        assert_eq!(
            &pkts[0].data[..],
            &[0xFF, 0xD8, 1, 2, 0xD9],
            "relative base detected"
        );
    }

    #[test]
    fn oversized_movi_chunk_is_an_error_not_an_allocation() {
        let mut body = chunk(b"00dc", &[1, 2]);
        body.extend(b"00dc");
        body.extend(0x7FFF_FFF0u32.to_le_bytes());
        body.extend([0u8; 16]);
        let m = Movi {
            bytes: body,
            entries: Vec::new(),
        };
        let mut ctx = ctx_with(build(mjpeg_pcm_hdrl(), &m, None, false));
        let mut d = AVIDemuxer::default();
        d.read_header(&mut ctx).unwrap();
        assert!(d.read_frame(&mut ctx).unwrap().is_some());
        assert!(d.read_frame(&mut ctx).is_err());
    }

    #[test]
    fn huge_header_chunk_sizes_do_not_panic() {
        let mut hdrl_body = b"hdrl".to_vec();
        hdrl_body.extend(b"strh");
        hdrl_body.extend(0xFFFF_FFF0u32.to_le_bytes());
        hdrl_body.extend([0u8; 30]);
        let data = riff(&[chunk(b"LIST", &hdrl_body)]);
        let mut d = AVIDemuxer::default();
        let _ = d.read_header(&mut ctx_with(data));

        let mut s = b"strl".to_vec();
        s.extend(b"strf");
        s.extend(0xFFFF_FFF0u32.to_le_bytes());
        s.extend([7u8; 64]);
        let data = riff(&[list(b"hdrl", &list(b"strl", &s)[8..])]);
        let mut ctx = ctx_with(data);
        let _ = d.read_header(&mut ctx);
        let _ = d.read_frame(&mut ctx);
    }

    #[test]
    fn truncated_files_never_panic() {
        let m = sample_movi();
        for (keys, absolute) in [
            (Some(&[true; 5][..]), false),
            (None, false),
            (Some(&[true; 5][..]), true),
        ] {
            let full = build(mjpeg_pcm_hdrl(), &m, keys, absolute);
            for len in 0..full.len() {
                let mut ctx = ctx_with(full[..len].to_vec());
                let mut d = AVIDemuxer::default();
                if d.read_header(&mut ctx).is_ok() {
                    let pkts = read_all(&mut ctx, &mut d);
                    assert!(pkts.len() <= 5);
                    let _ = d.seek(&mut ctx, 50);
                    let _ = read_all(&mut ctx, &mut d);
                }
            }
        }
    }

    #[test]
    fn corrupt_bytes_never_panic() {
        let m = sample_movi();
        let full = build(mjpeg_pcm_hdrl(), &m, Some(&[true; 5]), false);
        let mut seed = 0x1234_5678u32;
        for _ in 0..300 {
            let mut data = full.clone();
            for _ in 0..8 {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let at = 12 + (seed as usize % (data.len() - 12));
                data[at] = (seed >> 24) as u8;
            }
            let mut ctx = ctx_with(data);
            let mut d = AVIDemuxer::default();
            if d.read_header(&mut ctx).is_ok() {
                let _ = read_all(&mut ctx, &mut d);
                let _ = d.seek(&mut ctx, 40);
                let _ = read_all(&mut ctx, &mut d);
            }
        }
    }
}
