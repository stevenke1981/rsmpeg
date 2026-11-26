//! Matroska / WebM demuxer.
//!
//! `read_header` parses the `EBML` header, locates the `Segment`, and scans
//! the top-level children up to the first `Cluster` (`SeekHead`, `Info`,
//! `Tracks`, `Cues`). Anything not seen before the first cluster — typically
//! the `Cues` FFmpeg writes at the end of the file — is fetched through the
//! `SeekHead`. `read_frame` then streams `Cluster` → `SimpleBlock` /
//! `BlockGroup` elements (sized or unknown-size clusters, all four lacing
//! modes), and `seek` uses `Cues` or falls back to a cluster-timestamp scan.
//!
//! Only small header masters and single blocks are ever buffered, each behind
//! a hard size cap; all arithmetic on file-provided values is checked or
//! saturating, and every loop is bounded, so hostile or truncated files
//! produce `Err(InvalidData)` / `Ok(None)` rather than panics or huge
//! allocations.

use super::avi_demuxer::{
    fourcc_to_codec_id, h264_format_from_extradata, parse_bitmap_info_header, parse_wave_format_ex,
    raw_pixel_format, wave_format_codec,
};
use crate::format::InputFormat;
use crate::format_context::FormatContext;
use crate::io_context::IOContext;
use crate::probe::ProbeScore;
use crate::stream::Stream;
use bytes::Bytes;
use rsmpeg_codec::{CodecId, H264BitstreamFormat, Packet, PacketFlags};
use rsmpeg_util::{MediaType, Rational, RsError, RsResult, SampleFormat};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::SeekFrom;
use std::ops::Range;

// ── Element IDs (marker bits kept) ──
const ID_EBML: u32 = 0x1A45_DFA3;
const ID_DOCTYPE: u32 = 0x4282;
const ID_SEGMENT: u32 = 0x1853_8067;
const ID_SEEKHEAD: u32 = 0x114D_9B74;
const ID_SEEK: u32 = 0x4DBB;
const ID_SEEKID: u32 = 0x53AB;
const ID_SEEKPOSITION: u32 = 0x53AC;
const ID_INFO: u32 = 0x1549_A966;
const ID_TIMESTAMPSCALE: u32 = 0x2A_D7B1;
const ID_DURATION: u32 = 0x4489;
const ID_TITLE: u32 = 0x7BA9;
const ID_TRACKS: u32 = 0x1654_AE6B;
const ID_TRACKENTRY: u32 = 0xAE;
const ID_TRACKNUMBER: u32 = 0xD7;
const ID_TRACKTYPE: u32 = 0x83;
const ID_CODECID: u32 = 0x86;
const ID_CODECPRIVATE: u32 = 0x63A2;
const ID_LANGUAGE: u32 = 0x22_B59C;
const ID_LANGUAGE_BCP47: u32 = 0x22_B59D;
const ID_NAME: u32 = 0x536E;
const ID_DEFAULTDURATION: u32 = 0x23_E383;
const ID_VIDEO: u32 = 0xE0;
const ID_PIXELWIDTH: u32 = 0xB0;
const ID_PIXELHEIGHT: u32 = 0xBA;
const ID_COLOURSPACE: u32 = 0x2E_B524;
const ID_AUDIO: u32 = 0xE1;
const ID_SAMPLINGFREQUENCY: u32 = 0xB5;
const ID_CHANNELS: u32 = 0x9F;
const ID_BITDEPTH: u32 = 0x6264;
const ID_CONTENTENCODINGS: u32 = 0x6D80;
const ID_CONTENTENCODING: u32 = 0x6240;
const ID_CONTENTENCODINGSCOPE: u32 = 0x5032;
const ID_CONTENTENCODINGTYPE: u32 = 0x5033;
const ID_CONTENTCOMPRESSION: u32 = 0x5034;
const ID_CONTENTCOMPALGO: u32 = 0x4254;
const ID_CONTENTCOMPSETTINGS: u32 = 0x4255;
const ID_CUES: u32 = 0x1C53_BB6B;
const ID_CUEPOINT: u32 = 0xBB;
const ID_CUETIME: u32 = 0xB3;
const ID_CUETRACKPOSITIONS: u32 = 0xB7;
const ID_CUECLUSTERPOSITION: u32 = 0xF1;
const ID_CLUSTER: u32 = 0x1F43_B675;
const ID_TIMESTAMP: u32 = 0xE7;
const ID_SIMPLEBLOCK: u32 = 0xA3;
const ID_BLOCKGROUP: u32 = 0xA0;
const ID_BLOCK: u32 = 0xA1;
const ID_BLOCKDURATION: u32 = 0x9B;
const ID_REFERENCEBLOCK: u32 = 0xFB;
const ID_TAGS: u32 = 0x1254_C367;
const ID_CHAPTERS: u32 = 0x1043_A770;
const ID_ATTACHMENTS: u32 = 0x1941_A469;

// ── Safety caps ──
/// Largest packet payload accepted.
const MAX_PACKET_BYTES: usize = 32 * 1024 * 1024;
/// Matroska lacing allows at most 256 frames per block.
const MAX_LACE_FRAMES: usize = 256;
/// Largest top-level element skipped while scanning the header (64 GiB).
const MAX_SKIP_SIZE: u64 = 1 << 36;
/// Block body = packet payload + track/timestamp/flags/lace headers.
const MAX_BLOCK_BYTES: usize = MAX_PACKET_BYTES + 1024 * 1024;
const MAX_BLOCKGROUP_BYTES: usize = MAX_BLOCK_BYTES + 1024 * 1024;
const MAX_EBML_HEADER_BYTES: usize = 64 * 1024;
/// `SeekHead` / `Info` / `Tracks` (CodecPrivate lives here).
const MAX_HEADER_MASTER_BYTES: usize = 16 * 1024 * 1024;
const MAX_CUES_BYTES: usize = 32 * 1024 * 1024;
const MAX_TOP_LEVEL_ELEMENTS: usize = 65_536;
const MAX_SEEKHEAD_HOPS: usize = 16;
const MAX_SEEK_ENTRIES: usize = 4096;
const MAX_TRACKS: usize = 1024;
const MAX_CUE_POINTS: usize = 1 << 20;
/// Elements consumed by one `read_frame` before giving up.
const MAX_ELEMENTS_PER_READ: usize = 1 << 20;
/// Elements visited by one cue-less `seek` scan.
const MAX_SEEK_SCAN_ELEMENTS: usize = 1 << 22;
/// Children inspected looking for a cluster's `Timestamp`.
const MAX_TIMESTAMP_PROBE_CHILDREN: usize = 64;

const DEFAULT_TIMESTAMP_SCALE: u64 = 1_000_000;

fn bad(msg: &'static str) -> RsError {
    RsError::InvalidData(msg.into())
}

fn bad_owned(msg: String) -> RsError {
    RsError::InvalidData(msg.into())
}

// ───────────────────────────── EBML primitives ─────────────────────────────

/// Total VINT length (1..=8) implied by its first byte; `None` for `0x00`.
fn vint_length(first: u8) -> Option<usize> {
    if first == 0 {
        None
    } else {
        Some(first.leading_zeros() as usize + 1)
    }
}

/// Decode an element ID (1..=4 bytes, marker bit **kept**). Returns `(id, len)`.
fn parse_element_id(buf: &[u8]) -> RsResult<(u32, usize)> {
    let first = *buf
        .first()
        .ok_or_else(|| bad("EBML: truncated element ID"))?;
    let len = vint_length(first)
        .filter(|&l| l <= 4)
        .ok_or_else(|| bad("EBML: invalid element ID prefix"))?;
    let bytes = buf
        .get(..len)
        .ok_or_else(|| bad("EBML: truncated element ID"))?;
    let id = bytes.iter().fold(0u32, |acc, &b| (acc << 8) | b as u32);
    Ok((id, len))
}

/// Decode a VINT (1..=8 bytes) with the marker bit **stripped**.
/// Returns the raw value (all-ones sentinel not interpreted) and its length.
fn parse_vint_raw(buf: &[u8]) -> RsResult<(u64, usize)> {
    let first = *buf.first().ok_or_else(|| bad("EBML: truncated VINT"))?;
    let len = vint_length(first).ok_or_else(|| bad("EBML: invalid VINT prefix (0x00)"))?;
    let bytes = buf.get(..len).ok_or_else(|| bad("EBML: truncated VINT"))?;
    let mut value = (first as u64) & (0xFFu64 >> len);
    for &b in &bytes[1..] {
        value = (value << 8) | b as u64;
    }
    Ok((value, len))
}

/// Decode an element data size; `None` is the unknown-size sentinel
/// (all value bits set, e.g. `0xFF`, `0x7FFF`, …, `0x01FF_FFFF_FFFF_FFFF`).
fn parse_element_size(buf: &[u8]) -> RsResult<(Option<u64>, usize)> {
    let (value, len) = parse_vint_raw(buf)?;
    let all_ones = (1u64 << (7 * len)) - 1;
    Ok((if value == all_ones { None } else { Some(value) }, len))
}

fn read_uint(body: &[u8]) -> RsResult<u64> {
    if body.len() > 8 {
        return Err(bad("EBML: unsigned integer longer than 8 bytes"));
    }
    Ok(body.iter().fold(0u64, |acc, &b| (acc << 8) | b as u64))
}

fn read_float(body: &[u8]) -> RsResult<f64> {
    match body.len() {
        0 => Ok(0.0),
        4 => Ok(f32::from_be_bytes([body[0], body[1], body[2], body[3]]) as f64),
        8 => {
            let mut b = [0u8; 8];
            b.copy_from_slice(body);
            Ok(f64::from_be_bytes(b))
        }
        _ => Err(bad("EBML: float must be 0, 4 or 8 bytes")),
    }
}

fn read_string(body: &[u8]) -> String {
    let end = body.iter().position(|&b| b == 0).unwrap_or(body.len());
    String::from_utf8_lossy(&body[..end]).into_owned()
}

/// A child element inside an in-memory master body.
struct Child<'a> {
    id: u32,
    body: &'a [u8],
    /// Body range within the parent buffer.
    range: Range<usize>,
}

/// Iterator over the children of an in-memory master element.
///
/// Every step consumes at least the two header bytes, so it always
/// terminates; an unknown-size child extends to the end of the parent.
struct EbmlSlice<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> EbmlSlice<'a> {
    fn new(buf: &'a [u8]) -> Self {
        EbmlSlice { buf, pos: 0 }
    }

    fn next_child(&mut self) -> RsResult<Option<Child<'a>>> {
        if self.pos >= self.buf.len() {
            return Ok(None);
        }
        let rest = &self.buf[self.pos..];
        let (id, il) = parse_element_id(rest)?;
        let (size, sl) = parse_element_size(&rest[il..])?;
        let header = il + sl;
        let avail = rest.len() - header;
        let len = match size {
            None => avail,
            Some(s) if s <= avail as u64 => s as usize,
            Some(_) => return Err(bad("EBML: child element overruns its parent")),
        };
        let start = self.pos + header;
        let range = start..start + len;
        self.pos = range.end;
        Ok(Some(Child {
            id,
            body: &self.buf[range.clone()],
            range,
        }))
    }
}

/// An element header read from the I/O stream.
#[derive(Debug, Clone, Copy)]
struct ElementHeader {
    id: u32,
    size: Option<u64>,
    start: u64,
    data_start: u64,
}

impl ElementHeader {
    fn data_end(&self) -> Option<u64> {
        self.size.and_then(|s| self.data_start.checked_add(s))
    }
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

/// Read the element header at `pos`.
///
/// `Ok(None)` means end of input (including a header cut off by EOF);
/// malformed VINT prefixes are `Err(InvalidData)`.
fn read_element_header(io: &mut IOContext, pos: u64) -> RsResult<Option<ElementHeader>> {
    io.seek(SeekFrom::Start(pos))?;
    let mut buf = [0u8; 12];
    let n = read_up_to(io, &mut buf)?;
    if n == 0 {
        return Ok(None);
    }
    let id_len = vint_length(buf[0])
        .filter(|&l| l <= 4)
        .ok_or_else(|| bad_owned(format!("MKV: invalid element ID at offset {pos}")))?;
    if n <= id_len {
        return Ok(None);
    }
    let size_len = vint_length(buf[id_len])
        .ok_or_else(|| bad_owned(format!("MKV: invalid element size at offset {pos}")))?;
    if n < id_len + size_len {
        return Ok(None);
    }
    let (id, _) = parse_element_id(&buf[..n])?;
    let (size, _) = parse_element_size(&buf[id_len..n])?;
    let data_start = pos
        .checked_add((id_len + size_len) as u64)
        .ok_or_else(|| bad("MKV: element offset overflow"))?;
    Ok(Some(ElementHeader {
        id,
        size,
        start: pos,
        data_start,
    }))
}

/// Read a sized element body, refusing anything above `cap` or past EOF.
fn read_body(
    io: &mut IOContext,
    h: &ElementHeader,
    cap: usize,
    file_len: u64,
) -> RsResult<Vec<u8>> {
    let size = h
        .size
        .ok_or_else(|| bad_owned(format!("MKV: element {:#X} has unknown size", h.id)))?;
    if size > cap as u64 {
        return Err(bad_owned(format!(
            "MKV: element {:#X} of {size} bytes exceeds the {cap}-byte cap",
            h.id
        )));
    }
    if h.data_start
        .checked_add(size)
        .map_or(true, |e| e > file_len)
    {
        return Err(bad_owned(format!("MKV: element {:#X} is truncated", h.id)));
    }
    io.seek(SeekFrom::Start(h.data_start))?;
    io.read_bytes(size as usize)
}

/// Level-1 elements: inside an unknown-size Cluster they end the cluster.
fn is_top_level_id(id: u32) -> bool {
    matches!(
        id,
        ID_CLUSTER
            | ID_CUES
            | ID_TAGS
            | ID_CHAPTERS
            | ID_ATTACHMENTS
            | ID_SEEKHEAD
            | ID_INFO
            | ID_TRACKS
            | ID_SEGMENT
            | ID_EBML
    )
}

/// Split a block payload (after the 4+ byte block header) into frames.
///
/// `lacing`: 0 = none, 1 = Xiph, 2 = fixed-size, 3 = EBML.
fn split_lace(payload: &[u8], lacing: u8) -> RsResult<Vec<Range<usize>>> {
    if lacing == 0 {
        return Ok(std::iter::once(0..payload.len()).collect());
    }
    let count = *payload
        .first()
        .ok_or_else(|| bad("MKV: laced block without frame count"))? as usize
        + 1;
    if count > MAX_LACE_FRAMES {
        return Err(bad("MKV: too many laced frames"));
    }
    let mut off = 1usize;
    let mut sizes: Vec<usize> = Vec::with_capacity(count);
    match lacing {
        1 => {
            for _ in 0..count - 1 {
                let mut size = 0usize;
                loop {
                    let b = *payload
                        .get(off)
                        .ok_or_else(|| bad("MKV: truncated Xiph lace sizes"))?;
                    off += 1;
                    size = size
                        .checked_add(b as usize)
                        .ok_or_else(|| bad("MKV: Xiph lace size overflow"))?;
                    if b != 0xFF {
                        break;
                    }
                }
                sizes.push(size);
            }
        }
        2 => {
            let rest = payload.len() - off;
            if rest == 0 || rest % count != 0 {
                return Err(bad("MKV: fixed-size lacing does not divide the payload"));
            }
            sizes.resize(count - 1, rest / count);
        }
        3 if count > 1 => {
            let (first, l) = parse_vint_raw(&payload[off..])?;
            off += l;
            let first = i64::try_from(first).map_err(|_| bad("MKV: EBML lace size overflow"))?;
            sizes.push(usize::try_from(first).map_err(|_| bad("MKV: EBML lace size overflow"))?);
            let mut prev = first;
            for _ in 1..count - 1 {
                let (raw, l) = parse_vint_raw(&payload[off..])?;
                off += l;
                let bias = (1i64 << (7 * l - 1)) - 1;
                let delta = raw as i64 - bias;
                let size = prev
                    .checked_add(delta)
                    .filter(|&s| s >= 0)
                    .ok_or_else(|| bad("MKV: invalid EBML lace size"))?;
                sizes.push(usize::try_from(size).map_err(|_| bad("MKV: EBML lace size overflow"))?);
                prev = size;
            }
        }
        _ => {}
    }
    let avail = payload.len() - off;
    let used = sizes
        .iter()
        .try_fold(0usize, |acc, &s| acc.checked_add(s))
        .filter(|&u| u <= avail)
        .ok_or_else(|| bad("MKV: lace sizes exceed the block payload"))?;
    sizes.push(avail - used);
    let mut ranges = Vec::with_capacity(sizes.len());
    for s in sizes {
        ranges.push(off..off + s);
        off += s;
    }
    Ok(ranges)
}

/// Integer nanoseconds → milliseconds, saturating into `i64`.
fn ns_to_ms(ns: i128) -> i64 {
    (ns / 1_000_000).clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

/// Frame rate from a `DefaultDuration` in nanoseconds.
fn frame_rate_from_ns(ns: u64) -> Option<Rational> {
    if ns == 0 {
        return None;
    }
    let g = gcd(1_000_000_000, ns);
    let (num, den) = (1_000_000_000 / g, ns / g);
    if num <= i32::MAX as u64 && den <= i32::MAX as u64 {
        Some(Rational::new(num as i32, den as i32))
    } else {
        None
    }
}

// ───────────────────────────── header model ─────────────────────────────

#[derive(Debug, Default, Clone)]
struct TrackEntry {
    number: u64,
    track_type: u64,
    codec_id: String,
    codec_private: Option<Vec<u8>>,
    language: Option<String>,
    language_bcp47: Option<String>,
    name: Option<String>,
    default_duration_ns: Option<u64>,
    pixel_width: Option<u64>,
    pixel_height: Option<u64>,
    colour_space: Option<Vec<u8>>,
    sampling_frequency: Option<f64>,
    channels: Option<u64>,
    bit_depth: Option<u64>,
    /// ContentCompAlgo 3 (header stripping) prefix to restore on every frame.
    strip_header: Option<Vec<u8>>,
}

#[derive(Debug, Default)]
struct HeaderState {
    seek_entries: Vec<(u32, u64)>,
    info_seen: bool,
    tracks_seen: bool,
    cues_seen: bool,
    timestamp_scale: Option<u64>,
    duration: Option<f64>,
    title: Option<String>,
    tracks: Vec<TrackEntry>,
    /// `(CueTime ticks, CueClusterPosition)`.
    cues: Vec<(u64, u64)>,
    /// Element start offsets already parsed (SeekHead cycle guard).
    visited: HashSet<u64>,
}

fn parse_seek_head(body: &[u8], hs: &mut HeaderState) -> RsResult<()> {
    let mut it = EbmlSlice::new(body);
    while let Some(seek) = it.next_child()? {
        if seek.id != ID_SEEK {
            continue;
        }
        let (mut id, mut pos) = (None, None);
        let mut inner = EbmlSlice::new(seek.body);
        while let Some(c) = inner.next_child()? {
            match c.id {
                ID_SEEKID if (1..=4).contains(&c.body.len()) => {
                    id = Some(c.body.iter().fold(0u32, |a, &b| (a << 8) | b as u32));
                }
                ID_SEEKPOSITION => pos = Some(read_uint(c.body)?),
                _ => {}
            }
        }
        if let (Some(id), Some(pos)) = (id, pos) {
            if hs.seek_entries.len() >= MAX_SEEK_ENTRIES {
                break;
            }
            hs.seek_entries.push((id, pos));
        }
    }
    Ok(())
}

fn parse_info(body: &[u8], hs: &mut HeaderState) -> RsResult<()> {
    let mut it = EbmlSlice::new(body);
    while let Some(c) = it.next_child()? {
        match c.id {
            ID_TIMESTAMPSCALE => hs.timestamp_scale = Some(read_uint(c.body)?),
            ID_DURATION => hs.duration = Some(read_float(c.body)?),
            ID_TITLE => hs.title = Some(read_string(c.body)),
            _ => {}
        }
    }
    hs.info_seen = true;
    Ok(())
}

fn parse_video(body: &[u8], t: &mut TrackEntry) -> RsResult<()> {
    let mut it = EbmlSlice::new(body);
    while let Some(c) = it.next_child()? {
        match c.id {
            ID_PIXELWIDTH => t.pixel_width = Some(read_uint(c.body)?),
            ID_PIXELHEIGHT => t.pixel_height = Some(read_uint(c.body)?),
            ID_COLOURSPACE => t.colour_space = Some(c.body.to_vec()),
            _ => {}
        }
    }
    Ok(())
}

fn parse_audio(body: &[u8], t: &mut TrackEntry) -> RsResult<()> {
    let mut it = EbmlSlice::new(body);
    while let Some(c) = it.next_child()? {
        match c.id {
            ID_SAMPLINGFREQUENCY => t.sampling_frequency = Some(read_float(c.body)?),
            ID_CHANNELS => t.channels = Some(read_uint(c.body)?),
            ID_BITDEPTH => t.bit_depth = Some(read_uint(c.body)?),
            _ => {}
        }
    }
    Ok(())
}

fn parse_content_encodings(body: &[u8], t: &mut TrackEntry) -> RsResult<()> {
    let mut it = EbmlSlice::new(body);
    while let Some(enc) = it.next_child()? {
        if enc.id != ID_CONTENTENCODING {
            continue;
        }
        let (mut scope, mut kind) = (1u64, 0u64);
        let (mut algo, mut settings) = (None, None);
        let mut inner = EbmlSlice::new(enc.body);
        while let Some(c) = inner.next_child()? {
            match c.id {
                ID_CONTENTENCODINGSCOPE => scope = read_uint(c.body)?,
                ID_CONTENTENCODINGTYPE => kind = read_uint(c.body)?,
                ID_CONTENTCOMPRESSION => {
                    algo = Some(0); // spec default: zlib
                    let mut comp = EbmlSlice::new(c.body);
                    while let Some(cc) = comp.next_child()? {
                        match cc.id {
                            ID_CONTENTCOMPALGO => algo = Some(read_uint(cc.body)?),
                            ID_CONTENTCOMPSETTINGS => settings = Some(cc.body.to_vec()),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        if kind != 0 {
            tracing::warn!(
                "MKV: track {} is encrypted; packets are passed through",
                t.number
            );
        } else if scope & 1 != 0 {
            match algo {
                Some(3) => t.strip_header = settings.filter(|s| !s.is_empty()),
                Some(a) => tracing::warn!(
                    "MKV: track {} uses unsupported ContentCompAlgo {a}; packets are passed through",
                    t.number
                ),
                None => {}
            }
        }
    }
    Ok(())
}

fn parse_track_entry(body: &[u8]) -> RsResult<TrackEntry> {
    let mut t = TrackEntry::default();
    let mut it = EbmlSlice::new(body);
    while let Some(c) = it.next_child()? {
        let b = c.body;
        match c.id {
            ID_TRACKNUMBER => t.number = read_uint(b)?,
            ID_TRACKTYPE => t.track_type = read_uint(b)?,
            ID_CODECID => t.codec_id = read_string(b),
            ID_CODECPRIVATE => t.codec_private = Some(b.to_vec()),
            ID_LANGUAGE => t.language = Some(read_string(b)),
            ID_LANGUAGE_BCP47 => t.language_bcp47 = Some(read_string(b)),
            ID_NAME => t.name = Some(read_string(b)),
            ID_DEFAULTDURATION => t.default_duration_ns = Some(read_uint(b)?),
            ID_VIDEO => parse_video(b, &mut t)?,
            ID_AUDIO => parse_audio(b, &mut t)?,
            ID_CONTENTENCODINGS => parse_content_encodings(b, &mut t)?,
            _ => {}
        }
    }
    Ok(t)
}

fn parse_tracks(body: &[u8], hs: &mut HeaderState) -> RsResult<()> {
    let mut it = EbmlSlice::new(body);
    while let Some(c) = it.next_child()? {
        if c.id != ID_TRACKENTRY {
            continue;
        }
        if hs.tracks.len() >= MAX_TRACKS {
            tracing::warn!("MKV: more than {MAX_TRACKS} tracks; ignoring the rest");
            break;
        }
        hs.tracks.push(parse_track_entry(c.body)?);
    }
    hs.tracks_seen = true;
    Ok(())
}

fn parse_cues(body: &[u8], hs: &mut HeaderState) -> RsResult<()> {
    let mut it = EbmlSlice::new(body);
    while let Some(cp) = it.next_child()? {
        if cp.id != ID_CUEPOINT {
            continue;
        }
        let (mut time, mut pos) = (None, None);
        let mut inner = EbmlSlice::new(cp.body);
        while let Some(c) = inner.next_child()? {
            match c.id {
                ID_CUETIME => time = Some(read_uint(c.body)?),
                ID_CUETRACKPOSITIONS if pos.is_none() => {
                    let mut tp = EbmlSlice::new(c.body);
                    while let Some(p) = tp.next_child()? {
                        if p.id == ID_CUECLUSTERPOSITION {
                            pos = Some(read_uint(p.body)?);
                        }
                    }
                }
                _ => {}
            }
        }
        if let (Some(t), Some(p)) = (time, pos) {
            if hs.cues.len() >= MAX_CUE_POINTS {
                break;
            }
            hs.cues.push((t, p));
        }
    }
    hs.cues_seen = true;
    Ok(())
}

/// Parse one header-relevant top-level element. `Cues` failures are
/// non-fatal (seeking falls back to a cluster scan).
fn parse_top_level(
    io: &mut IOContext,
    h: &ElementHeader,
    hs: &mut HeaderState,
    file_len: u64,
) -> RsResult<()> {
    match h.id {
        ID_SEEKHEAD => {
            let body = read_body(io, h, MAX_HEADER_MASTER_BYTES, file_len)?;
            parse_seek_head(&body, hs)?;
        }
        ID_INFO => {
            let body = read_body(io, h, MAX_HEADER_MASTER_BYTES, file_len)?;
            parse_info(&body, hs)?;
        }
        ID_TRACKS => {
            let body = read_body(io, h, MAX_HEADER_MASTER_BYTES, file_len)?;
            parse_tracks(&body, hs)?;
        }
        ID_CUES => {
            let parsed = read_body(io, h, MAX_CUES_BYTES, file_len).and_then(|body| {
                let mut tmp = HeaderState::default();
                parse_cues(&body, &mut tmp).map(|_| tmp.cues)
            });
            match parsed {
                Ok(cues) => {
                    hs.cues = cues;
                    hs.cues_seen = true;
                }
                Err(e) => {
                    tracing::warn!("MKV: ignoring unusable Cues: {e}");
                    hs.cues_seen = true;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// Codec mapping result for one track.
struct CodecMapping {
    codec_id: CodecId,
    extradata: Option<Vec<u8>>,
}

fn build_stream(index: usize, t: &TrackEntry, duration_ms: i64) -> Stream {
    let cid = t.codec_id.as_str();
    let private = t.codec_private.as_deref().filter(|p| !p.is_empty());
    let mut map = CodecMapping {
        codec_id: CodecId::Unknown,
        extradata: private.map(<[u8]>::to_vec),
    };
    let mut pixel_format = None;
    let mut sample_format = None;
    let mut h264_format = H264BitstreamFormat::Unknown;
    let mut width = t
        .pixel_width
        .and_then(|w| usize::try_from(w).ok())
        .filter(|&w| w > 0);
    let mut height = t
        .pixel_height
        .and_then(|h| usize::try_from(h).ok())
        .filter(|&h| h > 0);
    let mut sample_rate = t
        .sampling_frequency
        .filter(|f| f.is_finite() && *f >= 1.0 && *f <= u32::MAX as f64)
        .map(|f| f.round() as u32);
    let mut channels = t.channels.map(|c| c.min(u16::MAX as u64) as u16);
    let mut bit_rate = None;
    let bit_depth = t.bit_depth.unwrap_or(0);

    match cid {
        "V_MPEG4/ISO/AVC" => {
            map.codec_id = CodecId::H264;
            if let Some(p) = private.filter(|p| p.len() >= 5) {
                h264_format = H264BitstreamFormat::Avcc {
                    nal_length_size: (p[4] & 0x03) + 1,
                };
            }
        }
        "V_MJPEG" => map.codec_id = CodecId::Mjpeg,
        "V_MS/VFW/FOURCC" => {
            map.extradata = None;
            if let Some(bih) = private.and_then(parse_bitmap_info_header) {
                map.codec_id = fourcc_to_codec_id(&bih.compression);
                if map.codec_id == CodecId::RawVideo {
                    pixel_format = raw_pixel_format(&bih.compression, bih.bit_count);
                }
                if width.is_none() && bih.width != 0 {
                    width = Some(bih.width.unsigned_abs() as usize);
                }
                if height.is_none() && bih.height != 0 {
                    height = Some(bih.height.unsigned_abs() as usize);
                }
                if !bih.extradata.is_empty() {
                    map.extradata = Some(bih.extradata);
                }
                if map.codec_id == CodecId::H264 {
                    h264_format = h264_format_from_extradata(map.extradata.as_deref());
                }
            } else {
                tracing::warn!("MKV: track {} has no usable BITMAPINFOHEADER", t.number);
            }
        }
        "V_UNCOMPRESSED" => {
            map.codec_id = CodecId::RawVideo;
            pixel_format = t
                .colour_space
                .as_deref()
                .and_then(|cs| raw_pixel_format(cs, bit_depth.min(u16::MAX as u64) as u16));
        }
        "V_MPEGH/ISO/HEVC" => map.codec_id = CodecId::Hevc,
        "V_VP8" => map.codec_id = CodecId::Vp8,
        "V_VP9" => map.codec_id = CodecId::Vp9,
        "V_AV1" => map.codec_id = CodecId::Av1,
        "V_THEORA" => map.codec_id = CodecId::Theora,
        "V_MPEG4/ISO/SP" | "V_MPEG4/ISO/ASP" | "V_MPEG4/ISO/AP" => map.codec_id = CodecId::Mpeg4,
        "A_FLAC" => map.codec_id = CodecId::Flac,
        "A_MPEG/L3" | "A_MPEG/L2" => map.codec_id = CodecId::Mp3,
        "A_PCM/INT/LIT" => {
            map.codec_id = CodecId::Pcm;
            sample_format = match bit_depth {
                8 => Some(SampleFormat::U8),
                16 => Some(SampleFormat::S16),
                24 | 32 => Some(SampleFormat::S32),
                _ => None,
            };
        }
        "A_PCM/FLOAT/IEEE" => {
            map.codec_id = CodecId::Pcm;
            sample_format = match bit_depth {
                32 => Some(SampleFormat::F32),
                64 => Some(SampleFormat::F64),
                _ => None,
            };
        }
        "A_VORBIS" => map.codec_id = CodecId::Vorbis,
        "A_OPUS" => map.codec_id = CodecId::Opus,
        "A_ALAC" => map.codec_id = CodecId::Alac,
        "A_MS/ACM" => {
            map.extradata = None;
            if let Some(wf) = private.and_then(parse_wave_format_ex) {
                let (codec, sf) = wave_format_codec(wf.format_tag, wf.bits_per_sample);
                map.codec_id = codec;
                sample_format = sf;
                if sample_rate.is_none() && wf.sample_rate > 0 {
                    sample_rate = Some(wf.sample_rate);
                }
                if channels.is_none() && wf.channels > 0 {
                    channels = Some(wf.channels);
                }
                if wf.avg_bytes_per_sec > 0 {
                    bit_rate = Some(wf.avg_bytes_per_sec as u64 * 8);
                }
                if !wf.extradata.is_empty() {
                    map.extradata = Some(wf.extradata);
                }
            }
        }
        "S_TEXT/UTF8" => map.codec_id = CodecId::Srt,
        "S_TEXT/WEBVTT" => map.codec_id = CodecId::WebVtt,
        _ if cid.starts_with("A_AAC") => map.codec_id = CodecId::Aac,
        _ => tracing::warn!("MKV: track {} has unsupported CodecID {cid:?}", t.number),
    }

    let media_type = match t.track_type {
        1 => MediaType::Video,
        2 => MediaType::Audio,
        17 => MediaType::Subtitle,
        _ if map.codec_id != CodecId::Unknown => map.codec_id.media_type(),
        _ if cid.starts_with("V_") => MediaType::Video,
        _ if cid.starts_with("A_") => MediaType::Audio,
        _ if cid.starts_with("S_") => MediaType::Subtitle,
        _ => MediaType::Data,
    };

    let mut stream = Stream::new(index, map.codec_id);
    stream.media_type = media_type;
    let p = &mut stream.codec_params;
    p.media_type = media_type;
    p.extradata = map.extradata;
    p.h264_bitstream_format = h264_format;
    p.bit_rate = bit_rate;
    match media_type {
        MediaType::Video => {
            p.width = width;
            p.height = height;
            p.pixel_format = pixel_format;
        }
        MediaType::Audio => {
            p.sample_rate = sample_rate.or(Some(8000));
            p.channels = channels.filter(|&c| c > 0).or(Some(1));
            p.sample_format = sample_format;
        }
        _ => {}
    }
    if media_type == MediaType::Video {
        if let Some(fps) = t.default_duration_ns.and_then(frame_rate_from_ns) {
            stream.avg_frame_rate = fps;
            stream.r_frame_rate = fps;
        }
    }
    if let Some(lang) = t.language_bcp47.as_ref().filter(|l| !l.is_empty()) {
        stream.metadata.insert("language", lang.clone());
        stream.metadata.insert("language_bcp47", lang.clone());
    } else if let Some(lang) = t.language.as_ref().filter(|l| !l.is_empty()) {
        stream.metadata.insert("language", lang.clone());
    }
    if let Some(name) = t.name.as_ref().filter(|n| !n.is_empty()) {
        stream.metadata.insert("title", name.clone());
    }
    stream.time_base = Rational::new(1, 1000);
    stream.set_duration_ms(duration_ms);
    stream
}

// ───────────────────────────── demuxer ─────────────────────────────

#[derive(Debug, Clone)]
struct TrackRuntime {
    stream_index: usize,
    default_duration_ns: Option<u64>,
    strip_header: Option<Bytes>,
}

#[derive(Debug, Clone, Copy)]
struct CuePoint {
    time_ms: i64,
    /// Relative to `segment_data_start`.
    cluster_pos: u64,
}

#[derive(Debug, Clone, Copy)]
struct ClusterCursor {
    /// `None` for unknown-size clusters (they end at the next level-1 ID).
    end: Option<u64>,
}

/// Matroska / WebM demuxer.
#[derive(Debug, Default)]
pub struct MKVDemuxer {
    header_parsed: bool,
    file_len: u64,
    doc_type: String,
    timestamp_scale: u64,
    segment_data_start: u64,
    segment_end: u64,
    first_cluster_offset: Option<u64>,
    /// Matroska TrackNumber → stream mapping.
    tracks: HashMap<u64, TrackRuntime>,
    cues: Vec<CuePoint>,
    /// File offset of the next element to parse.
    pos: u64,
    cluster: Option<ClusterCursor>,
    current_cluster_ts: u64,
    pending_packets: VecDeque<Packet>,
}

impl InputFormat for MKVDemuxer {
    fn name(&self) -> &'static str {
        "matroska"
    }

    fn description(&self) -> &'static str {
        "Matroska/WebM"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["mkv", "mka", "mks", "webm"]
    }

    fn probe(&self, buf: &[u8]) -> ProbeScore {
        if buf.len() >= 4 && buf[0..4] == [0x1A, 0x45, 0xDF, 0xA3] {
            ProbeScore::Certain
        } else {
            ProbeScore::NoMatch
        }
    }

    fn read_header(&mut self, ctx: &mut FormatContext) -> RsResult<()> {
        *self = MKVDemuxer::default();
        self.timestamp_scale = DEFAULT_TIMESTAMP_SCALE;
        let io = ctx
            .io
            .as_mut()
            .ok_or_else(|| RsError::InvalidData("No IO context".into()))?;
        let file_len = io.seek(SeekFrom::End(0))?;
        self.file_len = file_len;

        // ── EBML header ──
        let ebml = read_element_header(io, 0)?.ok_or_else(|| bad("MKV: empty input"))?;
        if ebml.id != ID_EBML {
            return Err(bad("MKV: missing EBML header"));
        }
        let body = read_body(io, &ebml, MAX_EBML_HEADER_BYTES, file_len)?;
        let mut doc_type = String::from("matroska");
        let mut it = EbmlSlice::new(&body);
        while let Some(c) = it.next_child()? {
            if c.id == ID_DOCTYPE {
                doc_type = read_string(c.body);
            }
        }
        if doc_type != "matroska" && doc_type != "webm" {
            tracing::warn!("MKV: unexpected DocType {doc_type:?}; trying anyway");
        }
        self.doc_type = doc_type;

        // ── Segment ──
        let mut pos = ebml
            .data_end()
            .ok_or_else(|| bad("MKV: bad EBML header size"))?;
        let mut segment = None;
        for _ in 0..MAX_TOP_LEVEL_ELEMENTS {
            let h = read_element_header(io, pos)?.ok_or_else(|| bad("MKV: no Segment element"))?;
            if h.id == ID_SEGMENT {
                segment = Some(h);
                break;
            }
            pos = h
                .data_end()
                .filter(|&e| e > pos)
                .ok_or_else(|| bad("MKV: unknown-size element before Segment"))?;
        }
        let segment = segment.ok_or_else(|| bad("MKV: no Segment element"))?;
        self.segment_data_start = segment.data_start;
        self.segment_end = segment.data_end().map_or(file_len, |e| e.min(file_len));

        // ── Level-1 scan up to the first Cluster ──
        let mut hs = HeaderState::default();
        let mut pos = self.segment_data_start;
        for _ in 0..MAX_TOP_LEVEL_ELEMENTS {
            if pos >= self.segment_end {
                break;
            }
            let Some(h) = read_element_header(io, pos)? else {
                break;
            };
            if h.id == ID_CLUSTER {
                self.first_cluster_offset = Some(pos);
                break;
            }
            let Some(end) = h
                .data_end()
                .filter(|_| h.size.unwrap_or(0) <= MAX_SKIP_SIZE)
            else {
                tracing::warn!(
                    "MKV: unskippable level-1 element {:#X} at {pos}; stopping header scan",
                    h.id
                );
                break;
            };
            hs.visited.insert(pos);
            parse_top_level(io, &h, &mut hs, file_len)?;
            pos = end;
        }

        self.follow_seek_head(io, &mut hs);

        // ── Publish ──
        self.timestamp_scale = hs
            .timestamp_scale
            .filter(|&s| s > 0)
            .unwrap_or(DEFAULT_TIMESTAMP_SCALE);
        let scale = self.timestamp_scale as f64;
        let duration_ms = hs
            .duration
            .map(|d| d * scale / 1_000_000.0)
            .filter(|v| v.is_finite() && *v >= 0.0)
            .map_or(0, |v| v.min(i64::MAX as f64) as i64);
        ctx.duration = duration_ms;
        if let Some(title) = hs.title.filter(|t| !t.is_empty()) {
            ctx.metadata.insert("title", title);
        }

        for t in &hs.tracks {
            if t.number == 0 || self.tracks.contains_key(&t.number) {
                tracing::warn!(
                    "MKV: skipping track with invalid/duplicate number {}",
                    t.number
                );
                continue;
            }
            let index = ctx.streams.len();
            ctx.streams.push(build_stream(index, t, duration_ms));
            self.tracks.insert(
                t.number,
                TrackRuntime {
                    stream_index: index,
                    default_duration_ns: t.default_duration_ns.filter(|&d| d > 0),
                    strip_header: t.strip_header.clone().map(Bytes::from),
                },
            );
        }
        if self.tracks.is_empty() {
            tracing::warn!("MKV: no usable tracks found");
        }

        let scale_i = self.timestamp_scale as i128;
        self.cues = hs
            .cues
            .iter()
            .map(|&(t, p)| CuePoint {
                time_ms: ns_to_ms((t as i128).saturating_mul(scale_i)),
                cluster_pos: p,
            })
            .collect();
        self.cues.sort_by(|a, b| {
            a.time_ms
                .cmp(&b.time_ms)
                .then(a.cluster_pos.cmp(&b.cluster_pos))
        });

        self.pos = self.first_cluster_offset.unwrap_or(self.segment_end);
        io.seek(SeekFrom::Start(self.pos))?;
        self.header_parsed = true;
        tracing::info!(
            "MKV: DocType {:?}, {} track(s), {} cue(s), duration {} ms",
            self.doc_type,
            self.tracks.len(),
            self.cues.len(),
            ctx.duration
        );
        Ok(())
    }

    fn read_frame(&mut self, ctx: &mut FormatContext) -> RsResult<Option<Packet>> {
        if let Some(p) = self.pending_packets.pop_front() {
            return Ok(Some(p));
        }
        if !self.header_parsed {
            return Ok(None);
        }
        let io = ctx
            .io
            .as_mut()
            .ok_or_else(|| RsError::InvalidData("No IO context".into()))?;
        let file_len = self.file_len;

        for _ in 0..MAX_ELEMENTS_PER_READ {
            if let Some(c) = self.cluster {
                if c.end.is_some_and(|e| self.pos >= e) {
                    self.cluster = None;
                }
            }
            if self.pos >= self.segment_end {
                self.cluster = None;
                return Ok(None);
            }
            let Some(h) = read_element_header(io, self.pos)? else {
                self.pos = self.segment_end;
                return Ok(None);
            };

            let Some(cluster) = self.cluster else {
                // Level 1.
                match h.id {
                    ID_CLUSTER => {
                        self.cluster = Some(ClusterCursor {
                            end: h.data_end().map(|e| e.min(self.segment_end)),
                        });
                        self.current_cluster_ts = 0;
                        self.pos = h.data_start;
                    }
                    ID_EBML | ID_SEGMENT => {
                        self.pos = self.segment_end;
                        return Ok(None);
                    }
                    _ => match h.data_end() {
                        Some(e) => self.pos = e,
                        None => {
                            self.pos = self.segment_end;
                            return Ok(None);
                        }
                    },
                }
                continue;
            };

            // Inside a Cluster.
            if cluster.end.is_none() && is_top_level_id(h.id) {
                // Sibling of an unknown-size cluster: leave without consuming it.
                self.cluster = None;
                continue;
            }
            let Some(data_end) = h.data_end() else {
                self.pos = self.segment_end;
                return Err(bad_owned(format!(
                    "MKV: unknown-size element {:#X} inside a Cluster",
                    h.id
                )));
            };
            let limit = cluster.end.unwrap_or(self.segment_end);
            if data_end > limit {
                // Overruns its cluster (corrupt) or the file (truncated).
                self.pos = limit;
                self.cluster = None;
                continue;
            }
            self.pos = data_end;
            match h.id {
                ID_TIMESTAMP => {
                    let b = read_body(io, &h, 8, file_len)?;
                    self.current_cluster_ts = read_uint(&b)?;
                }
                ID_SIMPLEBLOCK => {
                    let b = read_body(io, &h, MAX_BLOCK_BYTES, file_len)?;
                    self.queue_block(Bytes::from(b), h.start, None, None)?;
                }
                ID_BLOCKGROUP => {
                    let b = read_body(io, &h, MAX_BLOCKGROUP_BYTES, file_len)?;
                    self.queue_block_group(Bytes::from(b), h.start)?;
                }
                _ => {}
            }
            if let Some(p) = self.pending_packets.pop_front() {
                return Ok(Some(p));
            }
        }
        Err(bad("MKV: too many elements without a packet"))
    }

    fn seek(&mut self, ctx: &mut FormatContext, timestamp: i64) -> RsResult<()> {
        self.pending_packets.clear();
        self.cluster = None;
        if !self.header_parsed {
            return Err(bad("MKV: seek before read_header"));
        }
        let target = timestamp.max(0);
        let Some(first) = self.first_cluster_offset else {
            self.pos = self.segment_end;
            return Ok(());
        };
        if target == 0 {
            self.pos = first;
            return Ok(());
        }
        let io = ctx
            .io
            .as_mut()
            .ok_or_else(|| RsError::InvalidData("No IO context".into()))?;

        if !self.cues.is_empty() {
            let idx = self.cues.partition_point(|c| c.time_ms <= target);
            let candidate = if idx == 0 {
                Some(first)
            } else {
                self.segment_data_start
                    .checked_add(self.cues[idx - 1].cluster_pos)
            };
            if let Some(p) = candidate.filter(|&p| p < self.segment_end) {
                if matches!(read_element_header(io, p), Ok(Some(h)) if h.id == ID_CLUSTER) {
                    self.pos = p;
                    return Ok(());
                }
            }
            tracing::warn!("MKV: cue for {target} ms does not point at a Cluster; scanning");
        }
        self.pos = self.scan_clusters_for(io, first, target);
        Ok(())
    }
}

impl MKVDemuxer {
    /// Jump through `SeekHead` entries for level-1 elements not seen yet.
    fn follow_seek_head(&self, io: &mut IOContext, hs: &mut HeaderState) {
        let mut i = 0usize;
        let mut hops = 0usize;
        while i < hs.seek_entries.len() {
            let (id, rel) = hs.seek_entries[i];
            i += 1;
            let wanted = match id {
                ID_INFO => !hs.info_seen,
                ID_TRACKS => !hs.tracks_seen,
                ID_CUES => !hs.cues_seen,
                ID_SEEKHEAD => true,
                _ => false,
            };
            if !wanted {
                continue;
            }
            let Some(abs) = self.segment_data_start.checked_add(rel) else {
                continue;
            };
            if abs >= self.segment_end || hs.visited.contains(&abs) {
                continue;
            }
            if hops >= MAX_SEEKHEAD_HOPS {
                tracing::warn!("MKV: SeekHead hop limit reached");
                break;
            }
            hops += 1;
            hs.visited.insert(abs);
            match read_element_header(io, abs) {
                Ok(Some(h)) if h.id == id => {
                    if let Err(e) = parse_top_level(io, &h, hs, self.file_len) {
                        tracing::warn!("MKV: SeekHead target {id:#X} at {abs} unusable: {e}");
                    }
                }
                _ => tracing::warn!("MKV: SeekHead entry {id:#X} at {abs} is invalid"),
            }
        }
    }

    /// Parse a block (SimpleBlock or Block) and queue one packet per frame.
    ///
    /// `group_key` is `Some(..)` for `BlockGroup` blocks (key = no
    /// `ReferenceBlock`); SimpleBlocks use their own keyframe flag.
    fn queue_block(
        &mut self,
        data: Bytes,
        pos: u64,
        group_key: Option<bool>,
        block_duration: Option<u64>,
    ) -> RsResult<()> {
        let (track, tl) = parse_vint_raw(&data)?;
        let header_end = tl + 3;
        if data.len() < header_end {
            return Err(bad("MKV: truncated block header"));
        }
        let rel_ts = i16::from_be_bytes([data[tl], data[tl + 1]]);
        let flags = data[tl + 2];
        let Some(rt) = self.tracks.get(&track).cloned() else {
            return Ok(()); // not a published track
        };
        let key = group_key.unwrap_or(flags & 0x80 != 0);
        let lacing = (flags >> 1) & 0x03;
        let frames = split_lace(&data[header_end..], lacing)?;

        let scale = self.timestamp_scale as i128;
        let base_ns = (self.current_cluster_ts as i128 + rel_ts as i128).saturating_mul(scale);
        let n = frames.len().max(1) as i128;
        let frame_ns: Option<i128> = block_duration
            .map(|d| (d as i128).saturating_mul(scale) / n)
            .or(rt.default_duration_ns.map(|d| d as i128));
        let pkt_pos = i64::try_from(pos).unwrap_or(i64::MAX);

        for (i, r) in frames.into_iter().enumerate() {
            let ts_ns = match frame_ns {
                Some(f) => base_ns.saturating_add(f.saturating_mul(i as i128)),
                None => base_ns,
            };
            let ts_ms = ns_to_ms(ts_ns);
            let frame = data.slice(header_end + r.start..header_end + r.end);
            let payload = match &rt.strip_header {
                Some(prefix) => {
                    let mut v = Vec::with_capacity(prefix.len() + frame.len());
                    v.extend_from_slice(prefix);
                    v.extend_from_slice(&frame);
                    Bytes::from(v)
                }
                None => frame,
            };
            let mut pkt = Packet::new(payload, rt.stream_index);
            pkt.pts = Some(ts_ms);
            pkt.dts = Some(ts_ms);
            pkt.duration = frame_ns.map_or(0, ns_to_ms);
            pkt.time_base = Rational::new(1, 1000);
            pkt.pos = pkt_pos;
            if key {
                pkt.flags |= PacketFlags::KEY;
            }
            self.pending_packets.push_back(pkt);
        }
        Ok(())
    }

    fn queue_block_group(&mut self, body: Bytes, pos: u64) -> RsResult<()> {
        let mut block: Option<Range<usize>> = None;
        let mut duration = None;
        let mut has_reference = false;
        let mut it = EbmlSlice::new(&body);
        while let Some(c) = it.next_child()? {
            match c.id {
                ID_BLOCK => block = Some(c.range),
                ID_BLOCKDURATION => duration = Some(read_uint(c.body)?),
                ID_REFERENCEBLOCK => has_reference = true,
                _ => {}
            }
        }
        match block {
            Some(r) => self.queue_block(body.slice(r), pos, Some(!has_reference), duration),
            None => Ok(()),
        }
    }

    fn ticks_to_ms(&self, ticks: u64) -> i64 {
        ns_to_ms((ticks as i128).saturating_mul(self.timestamp_scale as i128))
    }

    /// Cue-less seek: last Cluster whose Timestamp ≤ `target` (or `first`).
    fn scan_clusters_for(&self, io: &mut IOContext, first: u64, target: i64) -> u64 {
        let mut best = first;
        let mut pos = first;
        for _ in 0..MAX_SEEK_SCAN_ELEMENTS {
            if pos >= self.segment_end {
                break;
            }
            let h = match read_element_header(io, pos) {
                Ok(Some(h)) => h,
                _ => break,
            };
            let next = if h.id == ID_CLUSTER {
                if let Some(ts) = self.cluster_timestamp(io, &h) {
                    if self.ticks_to_ms(ts) > target {
                        break;
                    }
                    best = pos;
                }
                h.data_end().or_else(|| self.unknown_cluster_end(io, &h))
            } else if h.id == ID_EBML || h.id == ID_SEGMENT {
                break;
            } else {
                h.data_end()
            };
            match next {
                Some(n) if n > pos => pos = n,
                _ => break,
            }
        }
        best
    }

    fn cluster_timestamp(&self, io: &mut IOContext, h: &ElementHeader) -> Option<u64> {
        let end = h
            .data_end()
            .unwrap_or(self.segment_end)
            .min(self.segment_end);
        let mut p = h.data_start;
        for _ in 0..MAX_TIMESTAMP_PROBE_CHILDREN {
            if p >= end {
                return None;
            }
            let c = read_element_header(io, p).ok()??;
            if c.id == ID_TIMESTAMP {
                let b = read_body(io, &c, 8, self.file_len).ok()?;
                return read_uint(&b).ok();
            }
            if is_top_level_id(c.id) {
                return None;
            }
            p = c.data_end().filter(|&e| e > p)?;
        }
        None
    }

    /// End of an unknown-size cluster: the next level-1 element (or EOF).
    fn unknown_cluster_end(&self, io: &mut IOContext, h: &ElementHeader) -> Option<u64> {
        let mut p = h.data_start;
        for _ in 0..MAX_SEEK_SCAN_ELEMENTS {
            if p >= self.segment_end {
                return Some(self.segment_end);
            }
            match read_element_header(io, p) {
                Ok(Some(c)) => {
                    if is_top_level_id(c.id) {
                        return Some(p);
                    }
                    p = c.data_end().filter(|&e| e > p)?;
                }
                Ok(None) => return Some(self.segment_end),
                Err(_) => return None,
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io_context::IOContext;
    use rsmpeg_util::PixelFormat;

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
            format_name: Some("matroska".into()),
        }
    }

    // ── EBML writers ──

    fn id_bytes(id: u32) -> Vec<u8> {
        let b = id.to_be_bytes();
        let skip = b.iter().position(|&x| x != 0).unwrap_or(3);
        b[skip..].to_vec()
    }

    fn vint(n: u64) -> Vec<u8> {
        for len in 1..=8usize {
            if n < (1u64 << (7 * len)) - 1 {
                let marked = n | (1u64 << (7 * len));
                return (0..len).rev().map(|i| (marked >> (8 * i)) as u8).collect();
            }
        }
        panic!("vint too large");
    }

    fn el(id: u32, body: &[u8]) -> Vec<u8> {
        let mut v = id_bytes(id);
        v.extend(vint(body.len() as u64));
        v.extend_from_slice(body);
        v
    }

    fn el_unknown(id: u32, body: &[u8]) -> Vec<u8> {
        let mut v = id_bytes(id);
        v.extend([0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
        v.extend_from_slice(body);
        v
    }

    fn uint(id: u32, v: u64) -> Vec<u8> {
        let b = v.to_be_bytes();
        let skip = b.iter().position(|&x| x != 0).unwrap_or(7);
        el(id, &b[skip..])
    }

    fn uint8(id: u32, v: u64) -> Vec<u8> {
        el(id, &v.to_be_bytes())
    }

    fn string(id: u32, s: &str) -> Vec<u8> {
        el(id, s.as_bytes())
    }

    fn float8(id: u32, f: f64) -> Vec<u8> {
        el(id, &f.to_be_bytes())
    }

    fn float4(id: u32, f: f32) -> Vec<u8> {
        el(id, &f.to_be_bytes())
    }

    fn ebml_header(doc: &str) -> Vec<u8> {
        el(
            ID_EBML,
            &[uint(0x4286, 1), string(ID_DOCTYPE, doc)].concat(),
        )
    }

    fn info(scale: u64, duration: f64) -> Vec<u8> {
        el(
            ID_INFO,
            &[
                uint(ID_TIMESTAMPSCALE, scale),
                float8(ID_DURATION, duration),
                string(ID_TITLE, "demo"),
            ]
            .concat(),
        )
    }

    fn block_body(track: u64, rel: i16, flags: u8, payload: &[u8]) -> Vec<u8> {
        let mut b = vint(track);
        b.extend(rel.to_be_bytes());
        b.push(flags);
        b.extend_from_slice(payload);
        b
    }

    fn simple_block(track: u64, rel: i16, flags: u8, payload: &[u8]) -> Vec<u8> {
        el(ID_SIMPLEBLOCK, &block_body(track, rel, flags, payload))
    }

    fn block_group(track: u64, rel: i16, payload: &[u8], extra: &[u8]) -> Vec<u8> {
        el(
            ID_BLOCKGROUP,
            &[
                el(ID_BLOCK, &block_body(track, rel, 0, payload)),
                extra.to_vec(),
            ]
            .concat(),
        )
    }

    fn cluster(ts: u64, blocks: &[Vec<u8>]) -> Vec<u8> {
        let mut body = uint(ID_TIMESTAMP, ts);
        for b in blocks {
            body.extend_from_slice(b);
        }
        el(ID_CLUSTER, &body)
    }

    fn cluster_unknown(ts: u64, blocks: &[Vec<u8>]) -> Vec<u8> {
        let mut body = uint(ID_TIMESTAMP, ts);
        for b in blocks {
            body.extend_from_slice(b);
        }
        el_unknown(ID_CLUSTER, &body)
    }

    fn file(segment_children: &[Vec<u8>]) -> Vec<u8> {
        [
            ebml_header("matroska"),
            el(ID_SEGMENT, &segment_children.concat()),
        ]
        .concat()
    }

    fn h264_aac_tracks() -> Vec<u8> {
        let avcc = [1u8, 0x64, 0x00, 0x1F, 0xFF, 0xE1, 0x00, 0x00];
        let video = el(
            ID_TRACKENTRY,
            &[
                uint(ID_TRACKNUMBER, 5),
                uint(ID_TRACKTYPE, 1),
                string(ID_CODECID, "V_MPEG4/ISO/AVC"),
                el(ID_CODECPRIVATE, &avcc),
                uint(ID_DEFAULTDURATION, 40_000_000),
                string(ID_LANGUAGE, "und"),
                string(ID_NAME, "Main"),
                el(
                    ID_VIDEO,
                    &[uint(ID_PIXELWIDTH, 1280), uint(ID_PIXELHEIGHT, 720)].concat(),
                ),
            ]
            .concat(),
        );
        let audio = el(
            ID_TRACKENTRY,
            &[
                uint(ID_TRACKNUMBER, 9),
                uint(ID_TRACKTYPE, 2),
                string(ID_CODECID, "A_AAC"),
                el(ID_CODECPRIVATE, &[0x11, 0x90]),
                string(ID_LANGUAGE, "jpn"),
                string(ID_LANGUAGE_BCP47, "ja-JP"),
                el(
                    ID_AUDIO,
                    &[float8(ID_SAMPLINGFREQUENCY, 48_000.0), uint(ID_CHANNELS, 2)].concat(),
                ),
            ]
            .concat(),
        );
        el(ID_TRACKS, &[video, audio].concat())
    }

    fn read_all(ctx: &mut FormatContext, d: &mut MKVDemuxer) -> Vec<Packet> {
        let mut out = Vec::new();
        for _ in 0..10_000 {
            match d.read_frame(ctx) {
                Ok(Some(p)) => out.push(p),
                _ => break,
            }
        }
        out
    }

    fn summary(pkts: &[Packet]) -> Vec<(usize, i64, bool, Vec<u8>)> {
        pkts.iter()
            .map(|p| (p.stream_index, p.pts.unwrap(), p.is_key(), p.data.to_vec()))
            .collect()
    }

    // ── VINT tests ──

    #[test]
    fn element_ids_keep_marker_bits() {
        assert_eq!(parse_element_id(&[0xA3]).unwrap(), (0xA3, 1));
        assert_eq!(parse_element_id(&[0x42, 0x82, 0xFF]).unwrap(), (0x4282, 2));
        assert_eq!(
            parse_element_id(&[0x2A, 0xD7, 0xB1]).unwrap(),
            (0x2A_D7B1, 3)
        );
        assert_eq!(
            parse_element_id(&[0x1A, 0x45, 0xDF, 0xA3]).unwrap(),
            (ID_EBML, 4)
        );
        assert_eq!(
            parse_element_id(&[0x18, 0x53, 0x80, 0x67]).unwrap(),
            (ID_SEGMENT, 4)
        );
        assert_eq!(
            parse_element_id(&[0x1F, 0x43, 0xB6, 0x75]).unwrap(),
            (ID_CLUSTER, 4)
        );
        assert!(parse_element_id(&[0x00, 0x81]).is_err(), "0x00 prefix");
        assert!(parse_element_id(&[0x08, 1, 2, 3, 4]).is_err(), "5-byte ID");
        assert!(parse_element_id(&[0x1A, 0x45]).is_err(), "truncated");
        assert!(parse_element_id(&[]).is_err());
    }

    #[test]
    fn element_sizes_strip_marker_and_detect_unknown() {
        assert_eq!(parse_element_size(&[0x81]).unwrap(), (Some(1), 1));
        assert_eq!(parse_element_size(&[0x40, 0x02]).unwrap(), (Some(2), 2));
        assert_eq!(
            parse_element_size(&[0x20, 0x01, 0x00]).unwrap(),
            (Some(256), 3)
        );
        assert_eq!(
            parse_element_size(&[0x01, 0, 0, 0, 0, 0, 0, 5]).unwrap(),
            (Some(5), 8)
        );
        for len in 1..=8usize {
            let mut b = vec![0xFFu8; len];
            b[0] = 0xFF >> (len - 1);
            assert_eq!(parse_element_size(&b).unwrap(), (None, len), "len {len}");
            // one below the sentinel is a real size
            b[len - 1] = 0xFE;
            if len == 1 {
                b[0] = 0xFE;
            }
            assert!(parse_element_size(&b).unwrap().0.is_some(), "len {len}");
        }
        assert!(parse_element_size(&[0x00, 0xFF]).is_err());
        assert!(parse_element_size(&[0x40]).is_err(), "truncated");
        assert!(parse_element_size(&[]).is_err());
        assert_eq!(vint(300), vec![0x41, 0x2C]);
        assert_eq!(parse_element_size(&vint(300)).unwrap(), (Some(300), 2));
    }

    #[test]
    fn io_header_reader_handles_eof_and_garbage() {
        let mut io = IOContext::from_buffer(vec![0x1A, 0x45, 0xDF, 0xA3, 0x84]);
        let h = read_element_header(&mut io, 0).unwrap().unwrap();
        assert_eq!((h.id, h.size, h.data_start), (ID_EBML, Some(4), 5));
        assert!(read_element_header(&mut io, 5).unwrap().is_none(), "EOF");
        let mut io = IOContext::from_buffer(vec![0x1A, 0x45]);
        assert!(
            read_element_header(&mut io, 0).unwrap().is_none(),
            "cut header"
        );
        let mut io = IOContext::from_buffer(vec![0x00, 0x81]);
        assert!(read_element_header(&mut io, 0).is_err());
        let mut io = IOContext::from_buffer(vec![0xA3, 0x00, 0x00]);
        assert!(read_element_header(&mut io, 0).is_err());
    }

    // ── Lacing ──

    #[test]
    fn lacing_modes_split_correctly() {
        assert_eq!(split_lace(&[1, 2, 3], 0).unwrap(), vec![0..3]);

        // Xiph: sizes 300, 2, rest(3)
        let mut x = vec![2u8, 0xFF, 45, 2];
        x.extend(vec![7u8; 305]);
        let r = split_lace(&x, 1).unwrap();
        assert_eq!(r, vec![4..304, 304..306, 306..309]);

        // Fixed: 3 × 4
        let mut f = vec![2u8];
        f.extend([1u8; 12]);
        assert_eq!(split_lace(&f, 2).unwrap(), vec![1..5, 5..9, 9..13]);
        let mut bad_fixed = vec![2u8];
        bad_fixed.extend([1u8; 11]);
        assert!(split_lace(&bad_fixed, 2).is_err());

        // EBML: sizes 3, 5 (+2), 2 (-3), rest 4
        let mut e = vec![3u8, 0x83, 0x80 | (2 + 63), 0x80 | (63 - 3)];
        e.extend([9u8; 14]);
        assert_eq!(
            split_lace(&e, 3).unwrap(),
            vec![4..7, 7..12, 12..14, 14..18]
        );
        // 2-byte signed delta: +200 → raw = 200 + 8191
        let raw = 200u16 + 8191;
        let mut e2 = vec![2u8, 0x81, 0x40 | (raw >> 8) as u8, raw as u8];
        e2.extend(vec![0u8; 1 + 201 + 5]);
        assert_eq!(split_lace(&e2, 3).unwrap(), vec![4..5, 5..206, 206..211]);
    }

    #[test]
    fn hostile_lacing_is_rejected() {
        assert!(split_lace(&[], 1).is_err(), "no count byte");
        assert!(
            split_lace(&[3, 0xFF, 0xFF], 1).is_err(),
            "truncated Xiph sizes"
        );
        assert!(
            split_lace(&[1, 200, 1, 2], 1).is_err(),
            "Xiph sizes > payload"
        );
        assert!(split_lace(&[1, 0x00], 3).is_err(), "bad EBML VINT");
        assert!(
            split_lace(&[2, 0x83, 0x80, 0, 0, 0], 3).is_err(),
            "negative EBML size"
        );
        assert!(
            split_lace(&[1, 0xFE, 1, 2], 3).is_err(),
            "EBML size > payload"
        );
        assert!(
            split_lace(&[255], 2).is_err(),
            "256 fixed frames of nothing ok? remainder 0"
        );
    }

    // ── Full files ──

    #[test]
    fn h264_aac_header_and_packets() {
        let data = file(&[
            info(1_000_000, 2000.0),
            h264_aac_tracks(),
            cluster(
                0,
                &[
                    simple_block(5, 0, 0x80, b"v0"),
                    simple_block(9, 0, 0x80, b"a0"),
                    simple_block(5, 40, 0x00, b"v1"),
                    simple_block(77, 0, 0x80, b"unmapped"),
                ],
            ),
            el(0xEC, &[0; 3]), // Void between clusters
            cluster(
                1000,
                &[
                    simple_block(5, 0, 0x80, b"v2"),
                    simple_block(9, -5, 0x80, b"a1"),
                ],
            ),
        ]);
        let mut ctx = ctx_with(data);
        let mut d = MKVDemuxer::default();
        d.read_header(&mut ctx).unwrap();
        assert_eq!(ctx.duration, 2000);
        assert_eq!(ctx.metadata.get("title"), Some("demo"));
        assert_eq!(ctx.streams.len(), 2);

        let v = &ctx.streams[0];
        assert_eq!(v.codec_id, CodecId::H264);
        assert_eq!(v.media_type, MediaType::Video);
        assert_eq!(
            (v.codec_params.width, v.codec_params.height),
            (Some(1280), Some(720))
        );
        assert_eq!(
            v.codec_params.h264_bitstream_format,
            H264BitstreamFormat::Avcc { nal_length_size: 4 }
        );
        assert_eq!(
            v.codec_params.extradata.as_deref().map(<[u8]>::len),
            Some(8)
        );
        assert_eq!(v.avg_frame_rate, Rational::new(25, 1));
        assert_eq!(v.time_base, Rational::new(1, 1000));
        assert_eq!(v.duration, 2000);
        assert_eq!(v.metadata.get("language"), Some("und"));
        assert_eq!(v.metadata.get("title"), Some("Main"));

        let a = &ctx.streams[1];
        assert_eq!(a.codec_id, CodecId::Aac);
        assert_eq!(a.codec_params.sample_rate, Some(48_000));
        assert_eq!(a.codec_params.channels, Some(2));
        assert_eq!(a.codec_params.extradata.as_deref(), Some(&[0x11, 0x90][..]));
        assert_eq!(a.metadata.get("language"), Some("ja-JP"));

        let pkts = read_all(&mut ctx, &mut d);
        assert_eq!(
            summary(&pkts),
            vec![
                (0, 0, true, b"v0".to_vec()),
                (1, 0, true, b"a0".to_vec()),
                (0, 40, false, b"v1".to_vec()),
                (0, 1000, true, b"v2".to_vec()),
                (1, 995, true, b"a1".to_vec()),
            ]
        );
        assert_eq!(pkts[0].duration, 40, "DefaultDuration → packet duration");
        assert_eq!(pkts[0].time_base, Rational::new(1, 1000));
        assert!(d.read_frame(&mut ctx).unwrap().is_none());
        assert!(d.read_frame(&mut ctx).unwrap().is_none());
    }

    fn mjpeg_vfw_pcm_tracks() -> Vec<u8> {
        let mut bih = Vec::new();
        bih.extend(40u32.to_le_bytes());
        bih.extend(320i32.to_le_bytes());
        bih.extend(240i32.to_le_bytes());
        bih.extend(1u16.to_le_bytes());
        bih.extend(24u16.to_le_bytes());
        bih.extend_from_slice(b"MJPG");
        bih.extend([0u8; 20]);
        let video = el(
            ID_TRACKENTRY,
            &[
                uint(ID_TRACKNUMBER, 1),
                uint(ID_TRACKTYPE, 1),
                string(ID_CODECID, "V_MS/VFW/FOURCC"),
                el(ID_CODECPRIVATE, &bih),
            ]
            .concat(),
        );
        let audio = el(
            ID_TRACKENTRY,
            &[
                uint(ID_TRACKNUMBER, 2),
                uint(ID_TRACKTYPE, 2),
                string(ID_CODECID, "A_PCM/INT/LIT"),
                el(
                    ID_AUDIO,
                    &[
                        float4(ID_SAMPLINGFREQUENCY, 44_100.0),
                        uint(ID_CHANNELS, 2),
                        uint(ID_BITDEPTH, 16),
                    ]
                    .concat(),
                ),
            ]
            .concat(),
        );
        el(ID_TRACKS, &[video, audio].concat())
    }

    #[test]
    fn mjpeg_vfw_pcm_unknown_size_clusters_and_block_groups() {
        let data = file(&[
            info(1_000_000, 100.0),
            mjpeg_vfw_pcm_tracks(),
            cluster_unknown(
                0,
                &[
                    block_group(1, 0, b"j0", &[]),
                    block_group(2, 0, b"p0", &uint(ID_BLOCKDURATION, 20)),
                    el(0xEC, &[0; 2]), // Void inside a cluster
                    block_group(1, 33, b"j1", &el(ID_REFERENCEBLOCK, &[0xDF])),
                ],
            ),
            cluster_unknown(
                66,
                &[
                    simple_block(1, 0, 0x80, b"j2"),
                    simple_block(2, 0, 0x80, b"p1"),
                ],
            ),
            el(ID_TAGS, &[0; 4]),
        ]);
        let mut ctx = ctx_with(data);
        let mut d = MKVDemuxer::default();
        d.read_header(&mut ctx).unwrap();
        let v = &ctx.streams[0];
        assert_eq!(v.codec_id, CodecId::Mjpeg, "V_MS/VFW/FOURCC MJPG");
        assert_eq!(
            (v.codec_params.width, v.codec_params.height),
            (Some(320), Some(240))
        );
        assert_eq!(v.codec_params.extradata, None);
        let a = &ctx.streams[1];
        assert_eq!(a.codec_id, CodecId::Pcm);
        assert_eq!(a.codec_params.sample_format, Some(SampleFormat::S16));
        assert_eq!(a.codec_params.sample_rate, Some(44_100));

        let pkts = read_all(&mut ctx, &mut d);
        assert_eq!(
            summary(&pkts),
            vec![
                (0, 0, true, b"j0".to_vec()),
                (1, 0, true, b"p0".to_vec()),
                (0, 33, false, b"j1".to_vec()),
                (0, 66, true, b"j2".to_vec()),
                (1, 66, true, b"p1".to_vec()),
            ]
        );
        assert_eq!(pkts[1].duration, 20, "BlockDuration");
        // Seek on unknown-size clusters without cues.
        d.seek(&mut ctx, 50).unwrap();
        assert_eq!(d.read_frame(&mut ctx).unwrap().unwrap().pts, Some(0));
        d.seek(&mut ctx, 70).unwrap();
        assert_eq!(d.read_frame(&mut ctx).unwrap().unwrap().pts, Some(66));
    }

    #[test]
    fn vfw_fourcc_variants_map_codecs_and_pixel_formats() {
        let make = |fourcc: &[u8; 4], bits: u16, extra: &[u8]| {
            let mut bih = Vec::new();
            bih.extend(40u32.to_le_bytes());
            bih.extend(8i32.to_le_bytes());
            bih.extend((-4i32).to_le_bytes());
            bih.extend(1u16.to_le_bytes());
            bih.extend(bits.to_le_bytes());
            bih.extend_from_slice(fourcc);
            bih.extend([0u8; 20]);
            bih.extend_from_slice(extra);
            TrackEntry {
                number: 1,
                track_type: 1,
                codec_id: "V_MS/VFW/FOURCC".into(),
                codec_private: Some(bih),
                ..TrackEntry::default()
            }
        };
        let s = build_stream(0, &make(&[0; 4], 24, &[]), 0);
        assert_eq!(s.codec_id, CodecId::RawVideo);
        assert_eq!(s.codec_params.pixel_format, Some(PixelFormat::Bgr24));
        assert_eq!(s.codec_params.height, Some(4), "top-down height is abs()");
        let s = build_stream(0, &make(b"RGB ", 32, &[]), 0);
        assert_eq!(s.codec_params.pixel_format, Some(PixelFormat::Bgra));
        let s = build_stream(0, &make(b"DIB ", 8, &[]), 0);
        assert_eq!(s.codec_params.pixel_format, Some(PixelFormat::Gray8));
        let s = build_stream(0, &make(b"I420", 12, &[]), 0);
        assert_eq!(s.codec_params.pixel_format, Some(PixelFormat::Yuv420P));
        assert_eq!(
            build_stream(0, &make(b"XVID", 24, &[]), 0).codec_id,
            CodecId::Mpeg4
        );
        assert_eq!(
            build_stream(0, &make(b"DX50", 24, &[]), 0).codec_id,
            CodecId::Mpeg4
        );
        let s = build_stream(0, &make(b"avc1", 24, &[1, 0x42, 0, 0x1E, 0xFD]), 0);
        assert_eq!(s.codec_id, CodecId::H264);
        assert_eq!(
            s.codec_params.h264_bitstream_format,
            H264BitstreamFormat::Avcc { nal_length_size: 2 }
        );
        let s = build_stream(0, &make(b"H264", 24, &[]), 0);
        assert_eq!(
            s.codec_params.h264_bitstream_format,
            H264BitstreamFormat::AnnexB
        );

        let simple = |codec: &str, bits: u64| TrackEntry {
            number: 1,
            codec_id: codec.into(),
            bit_depth: Some(bits),
            ..TrackEntry::default()
        };
        for (codec, id) in [
            ("V_MPEGH/ISO/HEVC", CodecId::Hevc),
            ("V_VP8", CodecId::Vp8),
            ("V_VP9", CodecId::Vp9),
            ("V_AV1", CodecId::Av1),
            ("V_MJPEG", CodecId::Mjpeg),
            ("V_UNCOMPRESSED", CodecId::RawVideo),
            ("V_MPEG4/ISO/ASP", CodecId::Mpeg4),
            ("A_AAC/MPEG4/LC", CodecId::Aac),
            ("A_FLAC", CodecId::Flac),
            ("A_MPEG/L3", CodecId::Mp3),
            ("A_MPEG/L2", CodecId::Mp3),
            ("A_VORBIS", CodecId::Vorbis),
            ("A_OPUS", CodecId::Opus),
            ("A_ALAC", CodecId::Alac),
            ("S_TEXT/UTF8", CodecId::Srt),
            ("S_TEXT/WEBVTT", CodecId::WebVtt),
            ("X_WHATEVER", CodecId::Unknown),
        ] {
            assert_eq!(
                build_stream(0, &simple(codec, 0), 0).codec_id,
                id,
                "{codec}"
            );
        }
        assert_eq!(
            build_stream(0, &simple("S_TEXT/UTF8", 0), 0).media_type,
            MediaType::Subtitle
        );
        let sf = |c: &str, b: u64| build_stream(0, &simple(c, b), 0).codec_params.sample_format;
        assert_eq!(sf("A_PCM/INT/LIT", 8), Some(SampleFormat::U8));
        assert_eq!(sf("A_PCM/INT/LIT", 24), Some(SampleFormat::S32));
        assert_eq!(sf("A_PCM/INT/LIT", 32), Some(SampleFormat::S32));
        assert_eq!(sf("A_PCM/FLOAT/IEEE", 32), Some(SampleFormat::F32));
        assert_eq!(sf("A_PCM/FLOAT/IEEE", 64), Some(SampleFormat::F64));
    }

    /// Segment: SeekHead, Void, 3 clusters, then Info/Tracks/Cues at EOF.
    fn cues_at_eof_file() -> Vec<u8> {
        let c0 = cluster(
            0,
            &[
                simple_block(1, 0, 0x80, b"f0"),
                simple_block(1, 500, 0, b"f0b"),
            ],
        );
        let c1 = cluster(1000, &[simple_block(1, 0, 0x80, b"f1")]);
        let c2 = cluster(2000, &[simple_block(1, 0, 0x80, b"f2")]);
        let tracks = el(
            ID_TRACKS,
            &el(
                ID_TRACKENTRY,
                &[
                    uint(ID_TRACKNUMBER, 1),
                    uint(ID_TRACKTYPE, 1),
                    string(ID_CODECID, "V_VP9"),
                ]
                .concat(),
            ),
        );
        let info = info(1_000_000, 3000.0);
        let seek = |id: u32, pos: u64| {
            el(
                ID_SEEK,
                &[el(ID_SEEKID, &id_bytes(id)), uint8(ID_SEEKPOSITION, pos)].concat(),
            )
        };
        let seekhead_len = el(
            ID_SEEKHEAD,
            &[seek(ID_INFO, 0), seek(ID_TRACKS, 0), seek(ID_CUES, 0)].concat(),
        )
        .len();
        let void = el(0xEC, &[0; 10]);
        let base = (seekhead_len + void.len()) as u64;
        let p0 = base;
        let p1 = p0 + c0.len() as u64;
        let p2 = p1 + c1.len() as u64;
        let p_info = p2 + c2.len() as u64;
        let p_tracks = p_info + info.len() as u64;
        let p_cues = p_tracks + tracks.len() as u64;
        let cue = |t: u64, p: u64| {
            el(
                ID_CUEPOINT,
                &[
                    uint(ID_CUETIME, t),
                    el(
                        ID_CUETRACKPOSITIONS,
                        &[uint(0xF7, 1), uint(ID_CUECLUSTERPOSITION, p)].concat(),
                    ),
                ]
                .concat(),
            )
        };
        let cues = el(
            ID_CUES,
            &[cue(0, p0), cue(1000, p1), cue(2000, p2)].concat(),
        );
        let seekhead = el(
            ID_SEEKHEAD,
            &[
                seek(ID_INFO, p_info),
                seek(ID_TRACKS, p_tracks),
                seek(ID_CUES, p_cues),
            ]
            .concat(),
        );
        assert_eq!(seekhead.len(), seekhead_len);
        file(&[seekhead, void, c0, c1, c2, info, tracks, cues])
    }

    #[test]
    fn cues_and_tracks_at_eof_found_via_seekhead() {
        let mut ctx = ctx_with(cues_at_eof_file());
        let mut d = MKVDemuxer::default();
        d.read_header(&mut ctx).unwrap();
        assert_eq!(ctx.streams.len(), 1);
        assert_eq!(ctx.streams[0].codec_id, CodecId::Vp9);
        assert_eq!(ctx.duration, 3000);
        assert_eq!(d.cues.len(), 3);
        assert_eq!(
            d.read_frame(&mut ctx).unwrap().unwrap().data.as_ref(),
            b"f0"
        );

        d.seek(&mut ctx, 1500).unwrap();
        assert_eq!(d.read_frame(&mut ctx).unwrap().unwrap().pts, Some(1000));
        d.seek(&mut ctx, 2500).unwrap();
        assert_eq!(d.read_frame(&mut ctx).unwrap().unwrap().pts, Some(2000));
        // Packets stop at the trailing Info/Tracks/Cues elements.
        assert!(d.read_frame(&mut ctx).unwrap().is_none());
        d.seek(&mut ctx, -10).unwrap();
        assert_eq!(d.read_frame(&mut ctx).unwrap().unwrap().pts, Some(0));
        d.seek(&mut ctx, 999).unwrap();
        let p = d.read_frame(&mut ctx).unwrap().unwrap();
        assert_eq!(p.pts, Some(0));
        assert_eq!(d.read_frame(&mut ctx).unwrap().unwrap().pts, Some(500));
    }

    #[test]
    fn cueless_seek_scans_cluster_timestamps() {
        let tracks = el(
            ID_TRACKS,
            &el(
                ID_TRACKENTRY,
                &[uint(ID_TRACKNUMBER, 1), string(ID_CODECID, "A_OPUS")].concat(),
            ),
        );
        let data = file(&[
            info(1_000_000, 3000.0),
            tracks,
            cluster(0, &[simple_block(1, 0, 0x80, b"c0")]),
            cluster_unknown(1000, &[simple_block(1, 0, 0x80, b"c1")]),
            cluster(2000, &[simple_block(1, 0, 0x80, b"c2")]),
        ]);
        let mut ctx = ctx_with(data);
        let mut d = MKVDemuxer::default();
        d.read_header(&mut ctx).unwrap();
        assert!(d.cues.is_empty());
        assert_eq!(ctx.streams[0].media_type, MediaType::Audio);
        for (target, expect) in [
            (1500, 1000),
            (0, 0),
            (5000, 2000),
            (999, 0),
            (2000, 2000),
            (1000, 1000),
        ] {
            d.seek(&mut ctx, target).unwrap();
            assert_eq!(
                d.read_frame(&mut ctx).unwrap().unwrap().pts,
                Some(expect),
                "seek {target}"
            );
        }
    }

    #[test]
    fn laced_blocks_produce_one_packet_per_frame() {
        let tracks = el(
            ID_TRACKS,
            &el(
                ID_TRACKENTRY,
                &[
                    uint(ID_TRACKNUMBER, 1),
                    uint(ID_TRACKTYPE, 2),
                    string(ID_CODECID, "A_VORBIS"),
                    uint(ID_DEFAULTDURATION, 10_000_000),
                ]
                .concat(),
            ),
        );
        let mut xiph = vec![2u8, 2, 3];
        xiph.extend(b"aabbbcccc");
        let mut fixed = vec![1u8];
        fixed.extend(b"ddee");
        let mut ebml = vec![2u8, 0x81, 0x80 | 64];
        ebml.extend(b"fgghhh");
        let data = file(&[
            tracks,
            cluster(
                100,
                &[
                    simple_block(1, 0, 0x80 | (1 << 1), &xiph),
                    simple_block(1, 30, 0x80 | (2 << 1), &fixed),
                    simple_block(1, 50, 0x80 | (3 << 1), &ebml),
                    simple_block(1, 80, 0x80, b"none"),
                ],
            ),
        ]);
        let mut ctx = ctx_with(data);
        let mut d = MKVDemuxer::default();
        d.read_header(&mut ctx).unwrap();
        let pkts = read_all(&mut ctx, &mut d);
        let got: Vec<(i64, Vec<u8>)> = pkts
            .iter()
            .map(|p| (p.pts.unwrap(), p.data.to_vec()))
            .collect();
        assert_eq!(
            got,
            vec![
                (100, b"aa".to_vec()),
                (110, b"bbb".to_vec()),
                (120, b"cccc".to_vec()),
                (130, b"dd".to_vec()),
                (140, b"ee".to_vec()),
                (150, b"f".to_vec()),
                (160, b"gg".to_vec()),
                (170, b"hhh".to_vec()),
                (180, b"none".to_vec()),
            ]
        );
        assert!(pkts.iter().all(|p| p.duration == 10 && p.is_key()));
    }

    #[test]
    fn header_stripping_restores_prefix() {
        let enc = el(
            ID_CONTENTENCODINGS,
            &el(
                ID_CONTENTENCODING,
                &el(
                    ID_CONTENTCOMPRESSION,
                    &[
                        uint(ID_CONTENTCOMPALGO, 3),
                        el(ID_CONTENTCOMPSETTINGS, &[0xFF, 0xFB]),
                    ]
                    .concat(),
                ),
            ),
        );
        let tracks = el(
            ID_TRACKS,
            &el(
                ID_TRACKENTRY,
                &[
                    uint(ID_TRACKNUMBER, 1),
                    string(ID_CODECID, "A_MPEG/L3"),
                    enc,
                ]
                .concat(),
            ),
        );
        let data = file(&[
            tracks,
            cluster(0, &[simple_block(1, 0, 0x80, &[0x90, 0x00])]),
        ]);
        let mut ctx = ctx_with(data);
        let mut d = MKVDemuxer::default();
        d.read_header(&mut ctx).unwrap();
        let p = d.read_frame(&mut ctx).unwrap().unwrap();
        assert_eq!(p.data.as_ref(), &[0xFF, 0xFB, 0x90, 0x00]);
    }

    #[test]
    fn custom_timestamp_scale_and_f32_duration() {
        let info = el(
            ID_INFO,
            &[
                uint(ID_TIMESTAMPSCALE, 100_000),
                float4(ID_DURATION, 25_000.0),
            ]
            .concat(),
        );
        let tracks = el(
            ID_TRACKS,
            &el(
                ID_TRACKENTRY,
                &[uint(ID_TRACKNUMBER, 1), string(ID_CODECID, "V_AV1")].concat(),
            ),
        );
        let data = file(&[
            info,
            tracks,
            cluster(10_000, &[simple_block(1, 5, 0x80, b"x")]),
        ]);
        let mut ctx = ctx_with(data);
        let mut d = MKVDemuxer::default();
        d.read_header(&mut ctx).unwrap();
        assert_eq!(ctx.duration, 2500);
        // (10000 + 5) * 100000 ns = 1000.5 ms → 1000
        assert_eq!(d.read_frame(&mut ctx).unwrap().unwrap().pts, Some(1000));
    }

    #[test]
    fn webm_with_unknown_size_segment_and_no_clusters() {
        let tracks = el(
            ID_TRACKS,
            &el(
                ID_TRACKENTRY,
                &[uint(ID_TRACKNUMBER, 1), string(ID_CODECID, "V_VP8")].concat(),
            ),
        );
        let data = [
            ebml_header("webm"),
            el_unknown(ID_SEGMENT, &[info(1_000_000, 0.0), tracks].concat()),
        ]
        .concat();
        let mut ctx = ctx_with(data);
        let mut d = MKVDemuxer::default();
        d.read_header(&mut ctx).unwrap();
        assert_eq!(d.doc_type, "webm");
        assert_eq!(ctx.streams.len(), 1);
        assert_eq!(ctx.streams[0].codec_id, CodecId::Vp8);
        assert!(d.read_frame(&mut ctx).unwrap().is_none());
        d.seek(&mut ctx, 1000).unwrap();
        assert!(d.read_frame(&mut ctx).unwrap().is_none());
    }

    // ── Hostile inputs ──

    #[test]
    fn rejects_garbage_headers() {
        let mut d = MKVDemuxer::default();
        assert!(d.read_header(&mut ctx_with(Vec::new())).is_err());
        assert!(d
            .read_header(&mut ctx_with(vec![0x1A, 0x45, 0xDF, 0xA3, 0x00, 0, 0]))
            .is_err());
        assert!(d
            .read_header(&mut ctx_with(b"RIFF....AVI ".to_vec()))
            .is_err());
        // EBML header claiming 1 GiB
        let huge = vec![0x1A, 0x45, 0xDF, 0xA3, 0x08, 0x40, 0, 0, 0, 0, 0, 0];
        assert!(d.read_header(&mut ctx_with(huge)).is_err());
        // No Segment after the EBML header
        assert!(d
            .read_header(&mut ctx_with(ebml_header("matroska")))
            .is_err());
        // Tracks claiming more bytes than the file holds
        let mut t = id_bytes(ID_TRACKS);
        t.extend([0x08, 0x40, 0, 0, 0, 0, 0, 0]);
        assert!(d.read_header(&mut ctx_with(file(&[t]))).is_err());
        // TrackEntry child overrunning its parent
        let bad_tracks = el(ID_TRACKS, &[0xAE, 0x85, 0xD7, 0x81]);
        assert!(d.read_header(&mut ctx_with(file(&[bad_tracks]))).is_err());
    }

    #[test]
    fn seekhead_cycles_terminate() {
        let seek = |id: u32, pos: u64| {
            el(
                ID_SEEK,
                &[el(ID_SEEKID, &id_bytes(id)), uint8(ID_SEEKPOSITION, pos)].concat(),
            )
        };
        let sh = el(
            ID_SEEKHEAD,
            &[
                seek(ID_SEEKHEAD, 0),
                seek(ID_CUES, 0),
                seek(ID_TRACKS, 1 << 60),
                seek(ID_INFO, u64::MAX),
            ]
            .concat(),
        );
        let mut ctx = ctx_with(file(&[sh]));
        let mut d = MKVDemuxer::default();
        d.read_header(&mut ctx).unwrap();
        assert!(ctx.streams.is_empty());
        assert!(d.read_frame(&mut ctx).unwrap().is_none());
    }

    #[test]
    fn oversized_and_corrupt_blocks_error_cleanly() {
        let tracks = el(
            ID_TRACKS,
            &el(
                ID_TRACKENTRY,
                &[uint(ID_TRACKNUMBER, 1), string(ID_CODECID, "V_VP9")].concat(),
            ),
        );
        // SimpleBlock claiming 1 GiB inside an unknown-size cluster
        let mut huge = uint(ID_TIMESTAMP, 0);
        huge.extend([0xA3, 0x08, 0x40, 0, 0, 0, 0, 0, 0]);
        huge.extend([0u8; 32]);
        let mut ctx = ctx_with(file(&[tracks.clone(), el_unknown(ID_CLUSTER, &huge)]));
        let mut d = MKVDemuxer::default();
        d.read_header(&mut ctx).unwrap();
        let r = d.read_frame(&mut ctx);
        assert!(
            matches!(r, Ok(None) | Err(RsError::InvalidData(_))),
            "{r:?}"
        );

        // Lace sizes exceeding the payload
        let bad_lace = simple_block(1, 0, 0x80 | (1 << 1), &[1, 0xFF, 0xFF, 9, 1, 2]);
        let mut ctx = ctx_with(file(&[
            tracks.clone(),
            cluster(0, &[bad_lace, simple_block(1, 1, 0x80, b"ok")]),
        ]));
        d.read_header(&mut ctx).unwrap();
        assert!(d.read_frame(&mut ctx).is_err());
        // the reader recovers with the next block
        assert_eq!(
            d.read_frame(&mut ctx).unwrap().unwrap().data.as_ref(),
            b"ok"
        );

        // Zero byte where an element ID should be
        let mut z = uint(ID_TIMESTAMP, 0);
        z.extend([0x00, 0x00, 0x00]);
        let mut ctx = ctx_with(file(&[tracks.clone(), el(ID_CLUSTER, &z)]));
        d.read_header(&mut ctx).unwrap();
        assert!(matches!(
            d.read_frame(&mut ctx),
            Err(RsError::InvalidData(_))
        ));

        // Truncated block header
        let mut ctx = ctx_with(file(&[
            tracks,
            cluster(0, &[el(ID_SIMPLEBLOCK, &[0x81, 0x00])]),
        ]));
        d.read_header(&mut ctx).unwrap();
        assert!(d.read_frame(&mut ctx).is_err());
    }

    #[test]
    fn truncated_files_never_panic() {
        let files = [
            cues_at_eof_file(),
            file(&[
                info(1_000_000, 100.0),
                mjpeg_vfw_pcm_tracks(),
                cluster_unknown(
                    0,
                    &[
                        block_group(1, 0, b"j0", &[]),
                        simple_block(2, 0, 0x80, b"p0"),
                    ],
                ),
            ]),
        ];
        for full in files {
            for len in 0..full.len() {
                let mut ctx = ctx_with(full[..len].to_vec());
                let mut d = MKVDemuxer::default();
                if d.read_header(&mut ctx).is_ok() {
                    let _ = read_all(&mut ctx, &mut d);
                    let _ = d.seek(&mut ctx, 1500);
                    let _ = read_all(&mut ctx, &mut d);
                }
            }
        }
    }

    #[test]
    fn corrupt_bytes_never_panic() {
        let full = cues_at_eof_file();
        let mut seed = 0x9E37_79B9u32;
        for _ in 0..300 {
            let mut data = full.clone();
            for _ in 0..6 {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let at = 4 + (seed as usize % (data.len() - 4));
                data[at] = (seed >> 24) as u8;
            }
            let mut ctx = ctx_with(data);
            let mut d = MKVDemuxer::default();
            if d.read_header(&mut ctx).is_ok() {
                let _ = read_all(&mut ctx, &mut d);
                let _ = d.seek(&mut ctx, 1200);
                let _ = read_all(&mut ctx, &mut d);
            }
        }
    }
}
