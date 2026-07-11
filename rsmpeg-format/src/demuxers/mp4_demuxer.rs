//! MP4 / ISOBMFF demuxer with sample-table indexing.
//!
//! Parses `moov` track boxes, builds a sample index from
//! `stts` / `ctts` / `stsc` / `stsz` / `stco|co64` / `stss`,
//! and streams real `Packet`s from `mdat` via `read_frame`.

use crate::format::InputFormat;
use crate::format_context::FormatContext;
use crate::probe::ProbeScore;
use crate::stream::Stream;
use rsmpeg_codec::{CodecId, H264BitstreamFormat, Packet, PacketFlags};
use rsmpeg_util::{MediaType, Rational, RsError, RsResult};
use std::io::SeekFrom;

/// One indexed sample ready for packet emission.
#[derive(Debug, Clone)]
struct SampleEntry {
    offset: u64,
    size: u32,
    dts: i64,
    pts: i64,
    duration: u32,
    is_key: bool,
}

/// Per-track sample table + cursor.
#[derive(Debug, Clone)]
struct TrackState {
    stream_index: usize,
    timescale: u32,
    samples: Vec<SampleEntry>,
    next_sample: usize,
}

/// Intermediate tables collected while parsing one `trak`.
struct TrackTables {
    timescale: u32,
    duration: u64,
    media_type: MediaType,
    codec_id: CodecId,
    width: u16,
    height: u16,
    sample_rate: u32,
    channels: u16,
    extradata: Option<Vec<u8>>,
    h264_format: H264BitstreamFormat,
    stts: Vec<(u32, u32)>,      // (count, delta)
    ctts: Vec<(u32, i32)>,      // (count, offset)
    stsc: Vec<(u32, u32, u32)>, // (first_chunk, samples_per_chunk, desc_idx)
    stsz_default: u32,
    stsz: Vec<u32>,
    stco: Vec<u64>,
    stss: Vec<u32>, // 1-based sample numbers
}

impl Default for TrackTables {
    fn default() -> Self {
        Self {
            timescale: 0,
            duration: 0,
            media_type: MediaType::Data,
            codec_id: CodecId::Unknown,
            width: 0,
            height: 0,
            sample_rate: 0,
            channels: 0,
            extradata: None,
            h264_format: H264BitstreamFormat::Unknown,
            stts: Vec::new(),
            ctts: Vec::new(),
            stsc: Vec::new(),
            stsz_default: 0,
            stsz: Vec::new(),
            stco: Vec::new(),
            stss: Vec::new(),
        }
    }
}

/// MP4/ISOBMFF demuxer with real sample-table packet output.
pub struct MP4Demuxer {
    tracks: Vec<TrackState>,
    /// True when moov lacked sample tables (e.g. fragmented MP4).
    fragmented_or_empty: bool,
}

impl Default for MP4Demuxer {
    fn default() -> Self {
        Self {
            tracks: Vec::new(),
            fragmented_or_empty: false,
        }
    }
}

impl InputFormat for MP4Demuxer {
    fn name(&self) -> &'static str {
        "mp4"
    }

    fn description(&self) -> &'static str {
        "MP4/ISOBMFF"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["mp4", "m4a", "m4v", "mov"]
    }

    fn probe(&self, buf: &[u8]) -> ProbeScore {
        if buf.len() >= 8 && &buf[4..8] == b"ftyp" {
            ProbeScore::Certain
        } else {
            ProbeScore::NoMatch
        }
    }

    fn read_header(&mut self, ctx: &mut FormatContext) -> RsResult<()> {
        self.tracks.clear();
        self.fragmented_or_empty = false;
        ctx.streams.clear();

        let io = ctx
            .io
            .as_mut()
            .ok_or_else(|| RsError::InvalidData("No IO context".into()))?;
        io.seek(SeekFrom::Start(0))?;

        let file_end = {
            let pos = io.tell()?;
            let end = io.seek(SeekFrom::End(0))?;
            io.seek(SeekFrom::Start(pos))?;
            end
        };

        let mut track_builds: Vec<TrackTables> = Vec::new();
        let mut found_moov = false;
        let mut cursor = 0u64;

        while cursor + 8 <= file_end {
            io.seek(SeekFrom::Start(cursor))?;
            let (box_size, box_type, header_len) = read_box_header_at(io, file_end)?;
            if box_size < header_len {
                break;
            }
            let payload_start = cursor + header_len;
            let payload_end = cursor.saturating_add(box_size);
            if payload_end > file_end || payload_end < payload_start {
                break;
            }

            match &box_type {
                b"moov" => {
                    found_moov = true;
                    parse_moov(io, payload_start, payload_end, &mut track_builds)?;
                }
                b"moof" => {
                    tracing::warn!(
                        "MP4: found moof (fragmented MP4); sample-table demux not yet supported"
                    );
                    self.fragmented_or_empty = true;
                }
                _ => {}
            }
            if box_size == 0 {
                break;
            }
            cursor = payload_end;
        }

        if !found_moov {
            tracing::warn!("MP4: no moov box found (fragmented or streaming file?)");
            self.fragmented_or_empty = true;
            return Ok(());
        }

        let mut max_duration_ms: i64 = 0;
        for (idx, tables) in track_builds.into_iter().enumerate() {
            let samples = build_sample_index(&tables);
            if samples.is_empty()
                && tables.media_type != MediaType::Data
                && tables.media_type != MediaType::Subtitle
            {
                tracing::warn!(
                    "MP4: track {} has no samples (fragmented or incomplete stbl)",
                    idx
                );
            }

            let timescale = tables.timescale.max(1);
            let duration_tb = if tables.duration > 0 {
                tables.duration as i64
            } else {
                samples
                    .last()
                    .map(|s| s.dts + s.duration as i64)
                    .unwrap_or(0)
            };
            let duration_ms = (duration_tb as i128 * 1000 / timescale as i128) as i64;
            max_duration_ms = max_duration_ms.max(duration_ms);

            let mut stream = Stream::new(idx, tables.codec_id);
            stream.media_type = tables.media_type;
            stream.codec_params.codec_id = tables.codec_id;
            stream.codec_params.media_type = tables.media_type;
            stream.codec_params.extradata = tables.extradata;
            stream.codec_params.h264_bitstream_format = tables.h264_format;
            if tables.width > 0 {
                stream.codec_params.width = Some(tables.width as usize);
            }
            if tables.height > 0 {
                stream.codec_params.height = Some(tables.height as usize);
            }
            if tables.sample_rate > 0 {
                stream.codec_params.sample_rate = Some(tables.sample_rate);
            }
            if tables.channels > 0 {
                stream.codec_params.channels = Some(tables.channels);
            }
            stream.time_base = Rational::new(1, timescale as i32);
            stream.duration = duration_tb;
            ctx.streams.push(stream);

            self.tracks.push(TrackState {
                stream_index: idx,
                timescale,
                samples,
                next_sample: 0,
            });
        }

        ctx.duration = max_duration_ms;
        if self.tracks.iter().all(|t| t.samples.is_empty()) {
            self.fragmented_or_empty = true;
            tracing::warn!(
                "MP4: moov present but no sample index built (fragmented MP4 not supported yet)"
            );
        }

        tracing::info!(
            "MP4: {} stream(s), duration ~{} ms, samples ready={}",
            ctx.streams.len(),
            max_duration_ms,
            !self.fragmented_or_empty
        );
        Ok(())
    }

    fn read_frame(&mut self, ctx: &mut FormatContext) -> RsResult<Option<Packet>> {
        if self.fragmented_or_empty {
            return Ok(None);
        }

        // Pick the track whose next sample has the earliest DTS in seconds.
        let mut best: Option<(usize, f64)> = None;
        for (ti, track) in self.tracks.iter().enumerate() {
            if track.next_sample >= track.samples.len() {
                continue;
            }
            let s = &track.samples[track.next_sample];
            let t = s.dts as f64 / track.timescale.max(1) as f64;
            match best {
                None => best = Some((ti, t)),
                Some((_, bt)) if t < bt => best = Some((ti, t)),
                _ => {}
            }
        }

        let Some((ti, _)) = best else {
            return Ok(None);
        };

        let track = &mut self.tracks[ti];
        let sample = track.samples[track.next_sample].clone();
        track.next_sample += 1;
        let stream_index = track.stream_index;
        let timescale = track.timescale;
        let time_base = Rational::new(1, timescale.max(1) as i32);

        let io = ctx
            .io
            .as_mut()
            .ok_or_else(|| RsError::InvalidData("No IO context".into()))?;
        io.seek(SeekFrom::Start(sample.offset))?;
        let data = io.read_bytes(sample.size as usize)?;

        let mut packet = Packet::new(bytes::Bytes::from(data), stream_index);
        packet.pts = Some(sample.pts);
        packet.dts = Some(sample.dts);
        packet.duration = sample.duration as i64;
        packet.pos = sample.offset as i64;
        packet.time_base = time_base;
        if sample.is_key {
            packet.flags.insert(PacketFlags::KEY);
        }
        Ok(Some(packet))
    }

    fn seek(&mut self, _ctx: &mut FormatContext, timestamp_ms: i64) -> RsResult<()> {
        // timestamp_ms is milliseconds wall-media time.
        for track in &mut self.tracks {
            let target = (timestamp_ms as i128 * track.timescale as i128 / 1000) as i64;
            // Prefer nearest keyframe at or before target; fall back to sample before target.
            let mut idx = 0usize;
            let mut key_idx = 0usize;
            for (i, s) in track.samples.iter().enumerate() {
                if s.dts <= target {
                    idx = i;
                    if s.is_key {
                        key_idx = i;
                    }
                } else {
                    break;
                }
            }
            // For video-like tracks with keyframes, snap to keyframe.
            let has_keys = track.samples.iter().any(|s| s.is_key);
            track.next_sample = if has_keys { key_idx } else { idx };
        }
        Ok(())
    }
}

// ── Sample index builder ─────────────────────────────────────────────

fn build_sample_index(t: &TrackTables) -> Vec<SampleEntry> {
    if t.stco.is_empty() {
        return Vec::new();
    }

    let sample_count = if !t.stsz.is_empty() {
        t.stsz.len()
    } else if t.stsz_default > 0 {
        // Estimate from stts
        t.stts.iter().map(|(c, _)| *c as usize).sum()
    } else {
        t.stts.iter().map(|(c, _)| *c as usize).sum()
    };

    if sample_count == 0 {
        return Vec::new();
    }

    // Durations from stts
    let mut durations = Vec::with_capacity(sample_count);
    for &(count, delta) in &t.stts {
        for _ in 0..count {
            if durations.len() >= sample_count {
                break;
            }
            durations.push(delta);
        }
    }
    while durations.len() < sample_count {
        durations.push(durations.last().copied().unwrap_or(0));
    }

    // DTS cumulative
    let mut dts_list = Vec::with_capacity(sample_count);
    let mut dts: i64 = 0;
    for &d in &durations {
        dts_list.push(dts);
        dts = dts.saturating_add(d as i64);
    }

    // CTTS offsets
    let mut ctts_offsets = vec![0i32; sample_count];
    if !t.ctts.is_empty() {
        let mut i = 0usize;
        for &(count, offset) in &t.ctts {
            for _ in 0..count {
                if i >= sample_count {
                    break;
                }
                ctts_offsets[i] = offset;
                i += 1;
            }
        }
    }

    // Sizes
    let sizes: Vec<u32> = if !t.stsz.is_empty() {
        t.stsz.clone()
    } else {
        vec![t.stsz_default; sample_count]
    };

    // Chunk → samples via stsc
    // stsc entries: first_chunk (1-based), samples_per_chunk, sample_description_index
    let chunk_count = t.stco.len();
    let mut samples_per_chunk = vec![0u32; chunk_count];
    if t.stsc.is_empty() {
        // Fallback: put all samples in first chunk
        if chunk_count > 0 {
            samples_per_chunk[0] = sample_count as u32;
        }
    } else {
        for (ei, entry) in t.stsc.iter().enumerate() {
            let first = entry.0.max(1) as usize - 1;
            let next_first = t
                .stsc
                .get(ei + 1)
                .map(|e| e.0.max(1) as usize - 1)
                .unwrap_or(chunk_count);
            let end = next_first.min(chunk_count);
            for c in first..end {
                samples_per_chunk[c] = entry.1;
            }
        }
    }

    // Assign file offsets
    let mut offsets = Vec::with_capacity(sample_count);
    let mut sample_i = 0usize;
    for (ci, &spc) in samples_per_chunk.iter().enumerate() {
        let mut off = t.stco.get(ci).copied().unwrap_or(0);
        for _ in 0..spc {
            if sample_i >= sample_count {
                break;
            }
            offsets.push(off);
            off = off.saturating_add(sizes.get(sample_i).copied().unwrap_or(0) as u64);
            sample_i += 1;
        }
    }
    while offsets.len() < sample_count {
        offsets.push(0);
    }

    // Keyframes
    let mut is_key = vec![false; sample_count];
    if t.stss.is_empty() {
        // Audio (or no stss): every sample is a keyframe
        is_key.fill(true);
    } else {
        for &n in &t.stss {
            let i = n.saturating_sub(1) as usize;
            if i < sample_count {
                is_key[i] = true;
            }
        }
    }

    let mut samples = Vec::with_capacity(sample_count);
    for i in 0..sample_count {
        let d = *durations.get(i).unwrap_or(&0);
        let dts_i = *dts_list.get(i).unwrap_or(&0);
        let pts_i = dts_i.saturating_add(*ctts_offsets.get(i).unwrap_or(&0) as i64);
        samples.push(SampleEntry {
            offset: offsets[i],
            size: sizes.get(i).copied().unwrap_or(0),
            dts: dts_i,
            pts: pts_i,
            duration: d,
            is_key: is_key[i],
        });
    }
    samples
}

// ── Box parsing ──────────────────────────────────────────────────────

fn read_box_header_at(
    io: &mut crate::io_context::IOContext,
    parent_end: u64,
) -> RsResult<(u64, [u8; 4], u64)> {
    let start = io.tell()?;
    if start + 8 > parent_end {
        return Err(RsError::InvalidData("box header past parent end".into()));
    }
    let size32 = io.read_u32_be()?;
    let mut btype = [0u8; 4];
    io.read_exact(&mut btype)?;
    let (size, header_len) = if size32 == 1 {
        if start + 16 > parent_end {
            return Err(RsError::InvalidData("extended size past parent".into()));
        }
        let size64 = io.read_u64_be()?;
        (size64, 16u64)
    } else if size32 == 0 {
        // Extends to end of parent
        (parent_end.saturating_sub(start), 8u64)
    } else {
        (size32 as u64, 8u64)
    };
    Ok((size, btype, header_len))
}

fn parse_moov(
    io: &mut crate::io_context::IOContext,
    start: u64,
    end: u64,
    tracks: &mut Vec<TrackTables>,
) -> RsResult<()> {
    let mut cursor = start;
    while cursor + 8 <= end {
        io.seek(SeekFrom::Start(cursor))?;
        let (box_size, box_type, header_len) = read_box_header_at(io, end)?;
        if box_size < header_len {
            break;
        }
        let payload_start = cursor + header_len;
        let payload_end = (cursor + box_size).min(end);
        match &box_type {
            b"trak" => {
                let mut tables = TrackTables::default();
                tables.media_type = MediaType::Data;
                tables.codec_id = CodecId::Unknown;
                parse_trak(io, payload_start, payload_end, &mut tables)?;
                tracks.push(tables);
            }
            _ => {}
        }
        if box_size == 0 {
            break;
        }
        cursor = payload_end;
    }
    Ok(())
}

fn parse_trak(
    io: &mut crate::io_context::IOContext,
    start: u64,
    end: u64,
    tables: &mut TrackTables,
) -> RsResult<()> {
    let mut cursor = start;
    while cursor + 8 <= end {
        io.seek(SeekFrom::Start(cursor))?;
        let (box_size, box_type, header_len) = read_box_header_at(io, end)?;
        if box_size < header_len {
            break;
        }
        let payload_start = cursor + header_len;
        let payload_end = (cursor + box_size).min(end);
        match &box_type {
            b"mdia" => parse_mdia(io, payload_start, payload_end, tables)?,
            _ => {}
        }
        if box_size == 0 {
            break;
        }
        cursor = payload_end;
    }
    Ok(())
}

fn parse_mdia(
    io: &mut crate::io_context::IOContext,
    start: u64,
    end: u64,
    tables: &mut TrackTables,
) -> RsResult<()> {
    let mut cursor = start;
    while cursor + 8 <= end {
        io.seek(SeekFrom::Start(cursor))?;
        let (box_size, box_type, header_len) = read_box_header_at(io, end)?;
        if box_size < header_len {
            break;
        }
        let payload_start = cursor + header_len;
        let payload_end = (cursor + box_size).min(end);
        match &box_type {
            b"mdhd" => parse_mdhd(io, payload_start, payload_end, tables)?,
            b"hdlr" => parse_hdlr(io, payload_start, payload_end, tables)?,
            b"minf" => parse_minf(io, payload_start, payload_end, tables)?,
            _ => {}
        }
        if box_size == 0 {
            break;
        }
        cursor = payload_end;
    }
    Ok(())
}

fn parse_mdhd(
    io: &mut crate::io_context::IOContext,
    start: u64,
    end: u64,
    tables: &mut TrackTables,
) -> RsResult<()> {
    io.seek(SeekFrom::Start(start))?;
    if end.saturating_sub(start) < 4 {
        return Ok(());
    }
    let version = io.read_u8()?;
    let _flags = io.read_bytes(3)?;
    if version == 1 {
        if end.saturating_sub(io.tell()?) < 8 + 8 + 4 + 8 {
            return Ok(());
        }
        let _creation = io.read_u64_be()?;
        let _modification = io.read_u64_be()?;
        tables.timescale = io.read_u32_be()?;
        tables.duration = io.read_u64_be()?;
    } else {
        if end.saturating_sub(io.tell()?) < 4 + 4 + 4 + 4 {
            return Ok(());
        }
        let _creation = io.read_u32_be()?;
        let _modification = io.read_u32_be()?;
        tables.timescale = io.read_u32_be()?;
        tables.duration = io.read_u32_be()? as u64;
    }
    Ok(())
}

fn parse_hdlr(
    io: &mut crate::io_context::IOContext,
    start: u64,
    end: u64,
    tables: &mut TrackTables,
) -> RsResult<()> {
    io.seek(SeekFrom::Start(start))?;
    if end.saturating_sub(start) < 4 + 4 + 4 {
        return Ok(());
    }
    let _version_flags = io.read_u32_be()?;
    let _predefined = io.read_u32_be()?;
    let handler = io.read_bytes(4)?;
    tables.media_type = match &handler[..] {
        b"vide" => MediaType::Video,
        b"soun" => MediaType::Audio,
        b"subt" | b"sbtl" => MediaType::Subtitle,
        _ => MediaType::Data,
    };
    Ok(())
}

fn parse_minf(
    io: &mut crate::io_context::IOContext,
    start: u64,
    end: u64,
    tables: &mut TrackTables,
) -> RsResult<()> {
    let mut cursor = start;
    while cursor + 8 <= end {
        io.seek(SeekFrom::Start(cursor))?;
        let (box_size, box_type, header_len) = read_box_header_at(io, end)?;
        if box_size < header_len {
            break;
        }
        let payload_start = cursor + header_len;
        let payload_end = (cursor + box_size).min(end);
        if &box_type == b"stbl" {
            parse_stbl(io, payload_start, payload_end, tables)?;
        }
        if box_size == 0 {
            break;
        }
        cursor = payload_end;
    }
    Ok(())
}

fn parse_stbl(
    io: &mut crate::io_context::IOContext,
    start: u64,
    end: u64,
    tables: &mut TrackTables,
) -> RsResult<()> {
    let mut cursor = start;
    while cursor + 8 <= end {
        io.seek(SeekFrom::Start(cursor))?;
        let (box_size, box_type, header_len) = read_box_header_at(io, end)?;
        if box_size < header_len {
            break;
        }
        let payload_start = cursor + header_len;
        let payload_end = (cursor + box_size).min(end);
        match &box_type {
            b"stsd" => parse_stsd(io, payload_start, payload_end, tables)?,
            b"stts" => parse_stts(io, payload_start, payload_end, tables)?,
            b"ctts" => parse_ctts(io, payload_start, payload_end, tables)?,
            b"stsc" => parse_stsc(io, payload_start, payload_end, tables)?,
            b"stsz" => parse_stsz(io, payload_start, payload_end, tables)?,
            b"stz2" => parse_stsz(io, payload_start, payload_end, tables)?, // treat like stsz if simple
            b"stco" => parse_stco(io, payload_start, payload_end, tables, false)?,
            b"co64" => parse_stco(io, payload_start, payload_end, tables, true)?,
            b"stss" => parse_stss(io, payload_start, payload_end, tables)?,
            _ => {}
        }
        if box_size == 0 {
            break;
        }
        cursor = payload_end;
    }
    Ok(())
}

fn parse_full_box_header(io: &mut crate::io_context::IOContext, end: u64) -> RsResult<(u8, u32)> {
    if io.tell()? + 4 > end {
        return Err(RsError::InvalidData("truncated fullbox".into()));
    }
    let version = io.read_u8()?;
    let b1 = io.read_u8()? as u32;
    let b2 = io.read_u8()? as u32;
    let b3 = io.read_u8()? as u32;
    let flags = (b1 << 16) | (b2 << 8) | b3;
    Ok((version, flags))
}

fn parse_stts(
    io: &mut crate::io_context::IOContext,
    start: u64,
    end: u64,
    tables: &mut TrackTables,
) -> RsResult<()> {
    io.seek(SeekFrom::Start(start))?;
    let (_v, _f) = parse_full_box_header(io, end)?;
    if io.tell()? + 4 > end {
        return Ok(());
    }
    let entry_count = io.read_u32_be()? as usize;
    tables.stts.clear();
    for _ in 0..entry_count {
        if io.tell()? + 8 > end {
            break;
        }
        let count = io.read_u32_be()?;
        let delta = io.read_u32_be()?;
        tables.stts.push((count, delta));
    }
    Ok(())
}

fn parse_ctts(
    io: &mut crate::io_context::IOContext,
    start: u64,
    end: u64,
    tables: &mut TrackTables,
) -> RsResult<()> {
    io.seek(SeekFrom::Start(start))?;
    let (version, _f) = parse_full_box_header(io, end)?;
    if io.tell()? + 4 > end {
        return Ok(());
    }
    let entry_count = io.read_u32_be()? as usize;
    tables.ctts.clear();
    for _ in 0..entry_count {
        if io.tell()? + 8 > end {
            break;
        }
        let count = io.read_u32_be()?;
        let raw = io.read_u32_be()?;
        let offset = if version == 1 {
            raw as i32
        } else {
            // ISO: version 0 offsets are unsigned; cast keeps common B-frame values.
            raw as i32
        };
        tables.ctts.push((count, offset));
    }
    Ok(())
}

fn parse_stsc(
    io: &mut crate::io_context::IOContext,
    start: u64,
    end: u64,
    tables: &mut TrackTables,
) -> RsResult<()> {
    io.seek(SeekFrom::Start(start))?;
    let (_v, _f) = parse_full_box_header(io, end)?;
    if io.tell()? + 4 > end {
        return Ok(());
    }
    let entry_count = io.read_u32_be()? as usize;
    tables.stsc.clear();
    for _ in 0..entry_count {
        if io.tell()? + 12 > end {
            break;
        }
        let first_chunk = io.read_u32_be()?;
        let samples_per_chunk = io.read_u32_be()?;
        let desc = io.read_u32_be()?;
        tables.stsc.push((first_chunk, samples_per_chunk, desc));
    }
    Ok(())
}

fn parse_stsz(
    io: &mut crate::io_context::IOContext,
    start: u64,
    end: u64,
    tables: &mut TrackTables,
) -> RsResult<()> {
    io.seek(SeekFrom::Start(start))?;
    let (_v, _f) = parse_full_box_header(io, end)?;
    if io.tell()? + 8 > end {
        return Ok(());
    }
    let sample_size = io.read_u32_be()?;
    let sample_count = io.read_u32_be()? as usize;
    tables.stsz_default = sample_size;
    tables.stsz.clear();
    if sample_size == 0 {
        tables.stsz.reserve(sample_count);
        for _ in 0..sample_count {
            if io.tell()? + 4 > end {
                break;
            }
            tables.stsz.push(io.read_u32_be()?);
        }
    }
    Ok(())
}

fn parse_stco(
    io: &mut crate::io_context::IOContext,
    start: u64,
    end: u64,
    tables: &mut TrackTables,
    co64: bool,
) -> RsResult<()> {
    io.seek(SeekFrom::Start(start))?;
    let (_v, _f) = parse_full_box_header(io, end)?;
    if io.tell()? + 4 > end {
        return Ok(());
    }
    let entry_count = io.read_u32_be()? as usize;
    tables.stco.clear();
    tables.stco.reserve(entry_count);
    for _ in 0..entry_count {
        if co64 {
            if io.tell()? + 8 > end {
                break;
            }
            tables.stco.push(io.read_u64_be()?);
        } else {
            if io.tell()? + 4 > end {
                break;
            }
            tables.stco.push(io.read_u32_be()? as u64);
        }
    }
    Ok(())
}

fn parse_stss(
    io: &mut crate::io_context::IOContext,
    start: u64,
    end: u64,
    tables: &mut TrackTables,
) -> RsResult<()> {
    io.seek(SeekFrom::Start(start))?;
    let (_v, _f) = parse_full_box_header(io, end)?;
    if io.tell()? + 4 > end {
        return Ok(());
    }
    let entry_count = io.read_u32_be()? as usize;
    tables.stss.clear();
    for _ in 0..entry_count {
        if io.tell()? + 4 > end {
            break;
        }
        tables.stss.push(io.read_u32_be()?);
    }
    Ok(())
}

fn parse_stsd(
    io: &mut crate::io_context::IOContext,
    start: u64,
    end: u64,
    tables: &mut TrackTables,
) -> RsResult<()> {
    io.seek(SeekFrom::Start(start))?;
    let (_v, _f) = parse_full_box_header(io, end)?;
    if io.tell()? + 4 > end {
        return Ok(());
    }
    let entry_count = io.read_u32_be()?;
    let mut cursor = io.tell()?;
    for _ in 0..entry_count {
        if cursor + 8 > end {
            break;
        }
        io.seek(SeekFrom::Start(cursor))?;
        let (entry_size, entry_type, header_len) = read_box_header_at(io, end)?;
        if entry_size < header_len {
            break;
        }
        let payload_start = cursor + header_len;
        let payload_end = (cursor + entry_size).min(end);
        match &entry_type {
            b"avc1" | b"avc3" => {
                tables.codec_id = CodecId::H264;
                tables.media_type = MediaType::Video;
                parse_visual_sample_entry(io, payload_start, payload_end, tables)?;
            }
            b"hvc1" | b"hev1" => {
                tables.codec_id = CodecId::Hevc;
                tables.media_type = MediaType::Video;
                parse_visual_sample_entry(io, payload_start, payload_end, tables)?;
            }
            b"mp4a" => {
                tables.codec_id = CodecId::Aac;
                tables.media_type = MediaType::Audio;
                parse_audio_sample_entry(io, payload_start, payload_end, tables)?;
            }
            b"Opus" | b"opus" => {
                tables.codec_id = CodecId::Opus;
                tables.media_type = MediaType::Audio;
                parse_audio_sample_entry(io, payload_start, payload_end, tables)?;
            }
            b"alac" => {
                tables.codec_id = CodecId::Alac;
                tables.media_type = MediaType::Audio;
                parse_audio_sample_entry(io, payload_start, payload_end, tables)?;
            }
            b"vp09" => {
                tables.codec_id = CodecId::Vp9;
                tables.media_type = MediaType::Video;
            }
            b"av01" => {
                tables.codec_id = CodecId::Av1;
                tables.media_type = MediaType::Video;
            }
            _ => {}
        }
        if entry_size == 0 {
            break;
        }
        cursor = payload_end;
    }
    Ok(())
}

fn parse_visual_sample_entry(
    io: &mut crate::io_context::IOContext,
    start: u64,
    end: u64,
    tables: &mut TrackTables,
) -> RsResult<()> {
    // VisualSampleEntry: 6 reserved + 2 data_ref + 16 predefined + width/height + ...
    // Total fixed header after sample entry type: 78 bytes
    const HDR: u64 = 78;
    if end.saturating_sub(start) < HDR {
        return Ok(());
    }
    io.seek(SeekFrom::Start(start + 16 + 8))?; // skip reserved/predefined to width
                                               // Actually layout: 6+2=8 reserved/data_ref, then 2+2+4*3=16 predef/reserved, then width u16 height u16
                                               // Offset from payload start: 8 + 16 = 24 for width
    io.seek(SeekFrom::Start(start + 24))?;
    tables.width = io.read_u16_be()?;
    tables.height = io.read_u16_be()?;

    // Child boxes start after 78-byte header
    let child_start = start + HDR;
    let mut cursor = child_start;
    while cursor + 8 <= end {
        io.seek(SeekFrom::Start(cursor))?;
        let (box_size, box_type, header_len) = read_box_header_at(io, end)?;
        if box_size < header_len {
            break;
        }
        let payload_start = cursor + header_len;
        let payload_end = (cursor + box_size).min(end);
        if &box_type == b"avcC" {
            let len = payload_end.saturating_sub(payload_start) as usize;
            if len > 0 && len < 1024 * 1024 {
                io.seek(SeekFrom::Start(payload_start))?;
                let data = io.read_bytes(len)?;
                let nls = data.get(4).map(|b| ((*b & 0x03) + 1) as u8).unwrap_or(4);
                let nls = if nls == 3 { 4 } else { nls };
                tables.extradata = Some(data);
                tables.h264_format = H264BitstreamFormat::Avcc {
                    nal_length_size: nls,
                };
                tables.codec_id = CodecId::H264;
            }
        }
        if box_size == 0 {
            break;
        }
        cursor = payload_end;
    }
    Ok(())
}

fn parse_audio_sample_entry(
    io: &mut crate::io_context::IOContext,
    start: u64,
    end: u64,
    tables: &mut TrackTables,
) -> RsResult<()> {
    // AudioSampleEntry: 6+2 reserved/data_ref + 8 version fields + channels + size_size + ...
    // channels at offset 16, sample_rate at offset 24 (u16.u16 fixed)
    if end.saturating_sub(start) < 28 {
        return Ok(());
    }
    io.seek(SeekFrom::Start(start + 16))?;
    tables.channels = io.read_u16_be()?;
    let _sample_size = io.read_u16_be()?;
    let _pre_defined = io.read_u16_be()?;
    let _reserved = io.read_u16_be()?;
    let sr_hi = io.read_u16_be()? as u32;
    let _sr_lo = io.read_u16_be()?;
    tables.sample_rate = sr_hi;

    // Optional esds etc. after 28-byte AudioSampleEntry header (version 0)
    let child_start = start + 28;
    let mut cursor = child_start;
    while cursor + 8 <= end {
        io.seek(SeekFrom::Start(cursor))?;
        let (box_size, box_type, header_len) = read_box_header_at(io, end)?;
        if box_size < header_len {
            break;
        }
        let payload_start = cursor + header_len;
        let payload_end = (cursor + box_size).min(end);
        if &box_type == b"esds" {
            let len = payload_end.saturating_sub(payload_start) as usize;
            if len > 0 && len < 64 * 1024 {
                io.seek(SeekFrom::Start(payload_start))?;
                let data = io.read_bytes(len)?;
                tables.extradata = Some(data);
                tables.codec_id = CodecId::Aac;
            }
        }
        if box_size == 0 {
            break;
        }
        cursor = payload_end;
    }
    Ok(())
}

// ── Unit tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format_context::FormatContext;
    use crate::io_context::IOContext;

    fn be32(v: u32) -> [u8; 4] {
        v.to_be_bytes()
    }
    fn be16(v: u16) -> [u8; 2] {
        v.to_be_bytes()
    }

    fn box_bytes(btype: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let size = 8 + payload.len() as u32;
        let mut out = Vec::with_capacity(size as usize);
        out.extend_from_slice(&be32(size));
        out.extend_from_slice(btype);
        out.extend_from_slice(payload);
        out
    }

    fn fullbox(version: u8, flags: u32, rest: &[u8]) -> Vec<u8> {
        let mut p = vec![
            version,
            ((flags >> 16) & 0xff) as u8,
            ((flags >> 8) & 0xff) as u8,
            (flags & 0xff) as u8,
        ];
        p.extend_from_slice(rest);
        p
    }

    /// Build a minimal non-fragmented MP4: 1 audio-like track, 3 samples in mdat.
    fn build_minimal_mp4() -> Vec<u8> {
        // Sample payloads
        let s0 = b"AAA";
        let s1 = b"BBBB";
        let s2 = b"CC";
        let mdat_payload = {
            let mut v = Vec::new();
            v.extend_from_slice(s0);
            v.extend_from_slice(s1);
            v.extend_from_slice(s2);
            v
        };

        // We'll layout: ftyp | moov | mdat
        // mdat offset depends on ftyp+moov size — build moov first with placeholder stco, then fix.
        // Simpler: put mdat first after ftyp? ISO allows any order. Put mdat before moov so offsets known.

        let ftyp = box_bytes(b"ftyp", &{
            let mut p = Vec::new();
            p.extend_from_slice(b"isom");
            p.extend_from_slice(&be32(0));
            p.extend_from_slice(b"isom");
            p
        });

        let mdat = box_bytes(b"mdat", &mdat_payload);
        let mdat_data_offset = (ftyp.len() + 8) as u64; // after ftyp + mdat header

        // stsd with mp4a
        let mut mp4a_payload = vec![0u8; 28];
        // channels at 16
        mp4a_payload[16] = 0;
        mp4a_payload[17] = 2;
        // sample size 16
        mp4a_payload[18] = 0;
        mp4a_payload[19] = 16;
        // sample rate 48000
        mp4a_payload[24] = 0xbb;
        mp4a_payload[25] = 0x80;
        let mp4a = box_bytes(b"mp4a", &mp4a_payload);
        let mut stsd_rest = Vec::new();
        stsd_rest.extend_from_slice(&be32(1)); // entry count
        stsd_rest.extend_from_slice(&mp4a);
        let stsd = box_bytes(b"stsd", &fullbox(0, 0, &stsd_rest));

        // stts: 3 samples, delta 1024
        let mut stts_rest = Vec::new();
        stts_rest.extend_from_slice(&be32(1));
        stts_rest.extend_from_slice(&be32(3));
        stts_rest.extend_from_slice(&be32(1024));
        let stts = box_bytes(b"stts", &fullbox(0, 0, &stts_rest));

        // stsc: 1 chunk, 3 samples
        let mut stsc_rest = Vec::new();
        stsc_rest.extend_from_slice(&be32(1));
        stsc_rest.extend_from_slice(&be32(1));
        stsc_rest.extend_from_slice(&be32(3));
        stsc_rest.extend_from_slice(&be32(1));
        let stsc = box_bytes(b"stsc", &fullbox(0, 0, &stsc_rest));

        // stsz: sizes 3,4,2
        let mut stsz_rest = Vec::new();
        stsz_rest.extend_from_slice(&be32(0)); // default
        stsz_rest.extend_from_slice(&be32(3));
        stsz_rest.extend_from_slice(&be32(3));
        stsz_rest.extend_from_slice(&be32(4));
        stsz_rest.extend_from_slice(&be32(2));
        let stsz = box_bytes(b"stsz", &fullbox(0, 0, &stsz_rest));

        // stco
        let mut stco_rest = Vec::new();
        stco_rest.extend_from_slice(&be32(1));
        stco_rest.extend_from_slice(&be32(mdat_data_offset as u32));
        let stco = box_bytes(b"stco", &fullbox(0, 0, &stco_rest));

        let stbl = box_bytes(b"stbl", &[stsd, stts, stsc, stsz, stco].concat());
        let minf = box_bytes(b"minf", &stbl);

        // mdhd v0: timescale 48000, duration 3072
        let mut mdhd_rest = Vec::new();
        mdhd_rest.extend_from_slice(&be32(0)); // creation
        mdhd_rest.extend_from_slice(&be32(0)); // modification
        mdhd_rest.extend_from_slice(&be32(48000));
        mdhd_rest.extend_from_slice(&be32(3072));
        mdhd_rest.extend_from_slice(&be16(0x55c4)); // language
        mdhd_rest.extend_from_slice(&be16(0));
        let mdhd = box_bytes(b"mdhd", &fullbox(0, 0, &mdhd_rest));

        // hdlr soun
        let mut hdlr_rest = Vec::new();
        hdlr_rest.extend_from_slice(&be32(0)); // predefined
        hdlr_rest.extend_from_slice(b"soun");
        hdlr_rest.extend_from_slice(&[0u8; 12]);
        hdlr_rest.push(0); // name
        let hdlr = box_bytes(b"hdlr", &fullbox(0, 0, &hdlr_rest));

        let mdia = box_bytes(b"mdia", &[mdhd, hdlr, minf].concat());
        let trak = box_bytes(b"trak", &mdia);
        let moov = box_bytes(b"moov", &trak);

        let mut file = Vec::new();
        file.extend_from_slice(&ftyp);
        file.extend_from_slice(&mdat);
        file.extend_from_slice(&moov);
        file
    }

    #[test]
    fn build_sample_index_stts_stsz_stco() {
        let tables = TrackTables {
            timescale: 1000,
            stts: vec![(3, 10)],
            stsc: vec![(1, 3, 1)],
            stsz: vec![10, 20, 30],
            stco: vec![100],
            stss: vec![1, 3],
            ..Default::default()
        };
        let samples = build_sample_index(&tables);
        assert_eq!(samples.len(), 3);
        assert_eq!(samples[0].offset, 100);
        assert_eq!(samples[0].size, 10);
        assert_eq!(samples[0].dts, 0);
        assert_eq!(samples[0].duration, 10);
        assert!(samples[0].is_key);
        assert_eq!(samples[1].offset, 110);
        assert_eq!(samples[1].dts, 10);
        assert!(!samples[1].is_key);
        assert_eq!(samples[2].offset, 130);
        assert_eq!(samples[2].dts, 20);
        assert!(samples[2].is_key);
    }

    #[test]
    fn build_sample_index_ctts_pts() {
        let tables = TrackTables {
            timescale: 1000,
            stts: vec![(2, 40)],
            ctts: vec![(1, 40), (1, 0)],
            stsc: vec![(1, 2, 1)],
            stsz: vec![1, 1],
            stco: vec![0],
            stss: vec![],
            ..Default::default()
        };
        let samples = build_sample_index(&tables);
        assert_eq!(samples[0].dts, 0);
        assert_eq!(samples[0].pts, 40);
        assert_eq!(samples[1].dts, 40);
        assert_eq!(samples[1].pts, 40);
        // no stss → all key
        assert!(samples[0].is_key && samples[1].is_key);
    }

    #[test]
    fn demux_minimal_mp4_packets() {
        let data = build_minimal_mp4();
        let mut ctx = FormatContext {
            input: None,
            output: None,
            streams: Vec::new(),
            io: Some(IOContext::from_buffer(data)),
            metadata: rsmpeg_util::Dict::new(),
            duration: 0,
            bit_rate: 0,
            filename: Some("test.mp4".into()),
            format_name: Some("mp4".into()),
        };
        let mut demuxer = MP4Demuxer::default();
        demuxer.read_header(&mut ctx).unwrap();
        assert_eq!(ctx.streams.len(), 1);
        assert_eq!(ctx.streams[0].codec_id, CodecId::Aac);
        assert_eq!(ctx.streams[0].media_type, MediaType::Audio);
        assert_eq!(ctx.streams[0].time_base, Rational::new(1, 48000));

        let p0 = demuxer.read_frame(&mut ctx).unwrap().expect("packet 0");
        assert_eq!(&p0.data[..], b"AAA");
        assert_eq!(p0.dts, Some(0));
        assert_eq!(p0.pts, Some(0));
        assert_eq!(p0.duration, 1024);
        assert!(p0.is_key());

        let p1 = demuxer.read_frame(&mut ctx).unwrap().expect("packet 1");
        assert_eq!(&p1.data[..], b"BBBB");
        assert_eq!(p1.dts, Some(1024));

        let p2 = demuxer.read_frame(&mut ctx).unwrap().expect("packet 2");
        assert_eq!(&p2.data[..], b"CC");
        assert_eq!(p2.dts, Some(2048));

        assert!(demuxer.read_frame(&mut ctx).unwrap().is_none());
    }

    #[test]
    fn seek_to_mid_resets_cursor() {
        let data = build_minimal_mp4();
        let mut ctx = FormatContext {
            input: None,
            output: None,
            streams: Vec::new(),
            io: Some(IOContext::from_buffer(data)),
            metadata: rsmpeg_util::Dict::new(),
            duration: 0,
            bit_rate: 0,
            filename: None,
            format_name: Some("mp4".into()),
        };
        let mut demuxer = MP4Demuxer::default();
        demuxer.read_header(&mut ctx).unwrap();
        // 1024/48000 s ≈ 21.3 ms → seek to 30ms should land near sample 1
        demuxer.seek(&mut ctx, 30).unwrap();
        let p = demuxer.read_frame(&mut ctx).unwrap().expect("after seek");
        assert!(p.dts.unwrap() >= 1024);
    }

    #[test]
    fn extended_size_and_parent_bounds() {
        // size=1 extended header for a free box
        let payload = vec![0u8; 16];
        let mut boxv = Vec::new();
        boxv.extend_from_slice(&be32(1)); // extended
        boxv.extend_from_slice(b"free");
        boxv.extend_from_slice(&8u64.wrapping_add(16).to_be_bytes()); // total size
        boxv.extend_from_slice(&payload);
        let mut io = IOContext::from_buffer(boxv);
        let (size, btype, header_len) = read_box_header_at(&mut io, 8 + 8 + 16).unwrap();
        assert_eq!(&btype, b"free");
        assert_eq!(header_len, 16);
        assert_eq!(size, 24);
    }
}
