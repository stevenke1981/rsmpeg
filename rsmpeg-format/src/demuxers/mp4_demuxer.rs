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
#[derive(Debug, Clone, Copy)]
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
    /// Payload ranges `[start, end)` of every `mdat` box. Sample data must live
    /// inside one of them; anything else (the `ftyp`/`moov` header region, or a
    /// range spanning two boxes) is structural bytes, not media.
    media_ranges: Vec<(u64, u64)>,
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
            media_ranges: Vec::new(),
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
#[derive(Default)]
pub struct MP4Demuxer {
    tracks: Vec<TrackState>,
    /// True when moov lacked sample tables (e.g. fragmented MP4).
    fragmented_or_empty: bool,
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
        let mut media_ranges: Vec<(u64, u64)> = Vec::new();
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
                b"mdat" => {
                    // Only the payload is media; the 8/16-byte box header is not.
                    if payload_end > payload_start {
                        media_ranges.push((payload_start, payload_end));
                    }
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

        // Sample ranges must fall inside an `mdat` payload; the file length alone
        // is not enough (byte 4 of a tiny file is `ftyp`, not media).
        if media_ranges.is_empty() {
            tracing::warn!("MP4: no mdat payload found; no sample can be located");
        }
        for tables in &mut track_builds {
            tables.media_ranges = media_ranges.clone();
        }

        let mut max_duration_ms: i64 = 0;
        let track_total = track_builds.len();
        let mut skipped_invalid_time_base = 0usize;
        let mut skipped_unplayable = 0usize;
        for (trak_idx, tables) in track_builds.into_iter().enumerate() {
            let samples = build_sample_index(&tables);
            if samples.is_empty()
                && tables.media_type != MediaType::Data
                && tables.media_type != MediaType::Subtitle
            {
                tracing::warn!(
                    "MP4: track {} has no samples (fragmented or incomplete stbl)",
                    trak_idx
                );
            }

            // A track with no locatable sample cannot be played: publishing it
            // would put a dead entry in `ctx.streams`, and `find_best_stream`
            // could select it while every packet belongs to a different track.
            // Data/subtitle tracks are metadata-only and stay publishable.
            let playable_kind = !matches!(
                tables.media_type,
                MediaType::Data | MediaType::Subtitle
            );
            if samples.is_empty() && playable_kind {
                tracing::error!(
                    "MP4: track {} has no sample inside any mdat payload; not publishing it",
                    trak_idx
                );
                skipped_unplayable += 1;
                continue;
            }

            // `tb = 1/timescale` must be representable as an `i32` pair, and a
            // missing (`mdhd` timescale 0) or out-of-range value is not. The
            // previous `max(1)` turned 0 into 1/1, re-interpreting every
            // DTS/PTS in seconds: refuse the track instead.
            let Some(time_base) = timescale_den(tables.timescale) else {
                tracing::error!(
                    "MP4: track {} has timescale {} which cannot be represented as a \
                     time base; skipping the track",
                    trak_idx,
                    tables.timescale
                );
                skipped_invalid_time_base += 1;
                continue;
            };
            let timescale = time_base as u32;
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

            // The stream index must match the position in `ctx.streams`: the
            // player picks a stream by index (`find_best_stream`) and routes
            // packets by `packet.stream_index`. Keeping the original `trak`
            // ordinal after dropping a track left both readers pointing at the
            // wrong slot (and at a stream that no longer exists).
            let index = ctx.streams.len();

            let mut stream = Stream::new(index, tables.codec_id);
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
            stream.time_base = Rational::new(1, time_base);
            stream.duration = duration_tb;
            ctx.streams.push(stream);

            self.tracks.push(TrackState {
                stream_index: index,
                timescale,
                samples,
                next_sample: 0,
            });
        }

        // Every track was rejected (unusable time base or unlocatable samples):
        // report the failure instead of returning success with an empty stream
        // list, which reads like an intentionally empty file.
        let skipped_any = skipped_invalid_time_base + skipped_unplayable;
        if ctx.streams.is_empty() && skipped_any > 0 && skipped_any == track_total
        {
            return Err(RsError::InvalidData(
                format!(
                    "MP4: all {track_total} track(s) have a timescale that cannot be \
                     represented as a time base"
                )
                .into(),
            ));
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

        // Secondary guard: never translate an unaddressable index entry into a
        // packet. `build_sample_index` already drops such samples, but a track
        // built by any other path must not fall through to reading byte 0.
        for track in &mut self.tracks {
            while track.next_sample < track.samples.len()
                && !sample_is_readable(&track.samples[track.next_sample])
            {
                tracing::warn!(
                    "MP4: skipping sample {} of stream {} (offset/size unavailable)",
                    track.next_sample,
                    track.stream_index
                );
                track.next_sample += 1;
            }
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
        // `SampleEntry` is `Copy`, so this is a plain byte copy (no `Vec` clone).
        let sample = track.samples[track.next_sample];
        track.next_sample += 1;
        let stream_index = track.stream_index;
        let timescale = track.timescale;
        // `read_header` skips tracks with an unrepresentable timescale, so this
        // is unreachable in practice; fail loudly rather than emit 1/1 ticks.
        let time_base = Rational::new(
            1,
            timescale_den(timescale).ok_or_else(|| {
                RsError::InvalidData(
                    format!(
                        "MP4: timescale {timescale} cannot be represented as a time base"
                    )
                    .into(),
                )
            })?,
        );

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

/// Expand the run-length `stsc` table into one samples-per-chunk value per
/// chunk, or `None` when the entries cannot prove a complete, contiguous
/// mapping of chunks `0..chunk_count`.
///
/// Two shapes are legitimate:
///
/// * `stsc` starts at `first_chunk == 1` and its ranges cover every chunk.
/// * `stsc` starts at `first_chunk == 1` and its last range runs past the last
///   chunk (the tail is simply larger than the table, which is how most muxers
///   write it) — covered by clamping the last range to `chunk_count`.
///
/// Anything else leaves a *leading* chunk (the reviewer's reproduction: 2
/// chunks, `first_chunk == 2`) or an interior chunk without a mapping. Those
/// chunks would silently receive `0` samples, so the samples they should have
/// carried would be looked for in the *next* chunk — with this sample's
/// `stsz` size and this sample's `stts` DTS. That is exactly the truncated
/// `BBB`／DTS 0 packet the review caught, so refuse the track instead of
/// returning a shifted index.
fn expand_stsc(
    t: &TrackTables,
    chunk_count: usize,
    sample_count: usize,
) -> Option<Vec<u32>> {
    if chunk_count == 0 {
        return Some(Vec::new());
    }
    if t.stsc.is_empty() {
        // No `stsc` at all: `stco` names chunks but says nothing about how many
        // samples each holds, so only the single-chunk case is unambiguous.
        if chunk_count == 1 {
            return Some(vec![u32::try_from(sample_count).ok()?]);
        }
        tracing::warn!(
            "MP4: no stsc table for {chunk_count} chunk(s); the chunk→sample \
             mapping is unknown"
        );
        return None;
    }

    // The first entry must start at chunk 1. A later start means the chunks
    // before it have no declared contents — the case this guard exists for.
    if t.stsc[0].0 != 1 {
        tracing::warn!(
            "MP4: stsc starts at first_chunk {} instead of 1; the earlier chunk(s) \
             have no sample mapping",
            t.stsc[0].0
        );
        return None;
    }

    let mut samples_per_chunk = vec![0u32; chunk_count];
    let mut filled_through = 0usize; // exclusive end of the covered chunk run
    for (ei, entry) in t.stsc.iter().enumerate() {
        let first_chunk = entry.0;
        let next_first = t.stsc.get(ei + 1).map(|e| e.0);

        // `first_chunk` is attacker-controlled and 1-based. A zero is invalid
        // (the spec allows 1..), and the table must be strictly increasing;
        // either error would otherwise be "repaired" into a wrong ordinal.
        if first_chunk == 0 || next_first.is_some_and(|next| next <= first_chunk) {
            tracing::warn!(
                "MP4: stsc entry {ei} has first_chunk {first_chunk} (next {:?}); the \
                 table must be strictly increasing from 1",
                next_first
            );
            return None;
        }

        let start = first_chunk as usize - 1;
        // A gap (start > filled_through) leaves chunks without a mapping.
        if start > filled_through {
            tracing::warn!(
                "MP4: stsc leaves chunk(s) {}..{} of {chunk_count} unmapped",
                filled_through + 1,
                start
            );
            return None;
        }
        // Starts past the chunk table: the real chunks above are all mapped by
        // earlier entries, so this only strands its own samples (a tail). Keep
        // the provable prefix rather than discarding the track.
        if start >= chunk_count {
            tracing::warn!(
                "MP4: stsc entry {ei} starts at chunk {first_chunk}, beyond the \
                 {chunk_count} chunk(s) in stco; its samples have no chunk"
            );
            continue;
        }

        let end = next_first
            .map(|next| next as usize - 1)
            .unwrap_or(chunk_count)
            .min(chunk_count);
        if end < start {
            tracing::warn!("MP4: stsc entry {ei} has an inverted chunk range");
            return None;
        }
        for slot in &mut samples_per_chunk[start..end] {
            *slot = entry.1;
        }
        filled_through = end;
    }

    // Every chunk must now have a declared sample count. A tail left at 0 means
    // the table stopped early, which is only acceptable when the sample tables
    // are exhausted too (a genuinely short tail); otherwise those chunks are
    // unmapped and would shift ordinals.
    let declared: u64 = samples_per_chunk.iter().map(|&c| u64::from(c)).sum();
    if declared == 0 {
        tracing::warn!("MP4: stsc declares no sample for any of {chunk_count} chunk(s)");
        return None;
    }
    if declared < sample_count as u64 {
        // The remaining samples have no chunk of their own. That is a missing
        // *tail*, and the caller keeps the locatable prefix — but only because
        // the mapping above is contiguous from chunk 1, which was just checked.
        tracing::warn!(
            "MP4: stsc addresses {declared} of {sample_count} samples; the rest have \
             no chunk mapping"
        );
    }
    Some(samples_per_chunk)
}

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
    let Some(samples_per_chunk) = expand_stsc(t, chunk_count, sample_count) else {
        // The sample tables are indexed by ordinal, so a chunk that `stsc`
        // cannot map is not "one missing packet" — every sample after it would
        // inherit another chunk's bytes with the wrong size and DTS. Refuse the
        // whole track instead of emitting a shifted index.
        tracing::warn!(
            "MP4: stsc cannot prove the sample→chunk mapping (chunks={chunk_count}, \
             entries={}); dropping all samples of this track",
            t.stsc.len()
        );
        return Vec::new();
    };

    // Assign file offsets
    let mut offsets = Vec::with_capacity(sample_count);
    let mut sample_i = 0usize;
    for (ci, &spc) in samples_per_chunk.iter().enumerate() {
        let Some(&chunk_off) = t.stco.get(ci) else {
            // Chunk table shorter than the sample-per-chunk table: nothing to
            // read from, so these samples stay unaddressed (see the truncation
            // below — they must never reach the packet reader).
            break;
        };
        let mut off = chunk_off;
        for _ in 0..spc {
            if sample_i >= sample_count {
                break;
            }
            offsets.push(off);
            off = off.saturating_add(sizes.get(sample_i).copied().unwrap_or(0) as u64);
            sample_i += 1;
        }
    }

    // Every sample must lie completely inside one `mdat` payload. Checking only
    // "offset != 0 and inside the file" was not enough: offset 4 of a small file
    // is the `ftyp` box, so a structurally valid packet was built out of
    // container header bytes. Running past the end of the file, or spanning two
    // boxes, is rejected the same way.
    //
    // Dropping just the offending entries is not enough either — the
    // sample/duration tables are indexed by ordinal, so removing the middle of a
    // chunk shifts every later sample's DTS/size onto the wrong bytes. Treat the
    // whole track as unaddressable instead.
    let valid_chunks = offsets.iter().zip(&sizes).take(offsets.len()).all(|(&off, &size)| {
        let end = off as u128 + size as u128;
        size != 0
            && t.media_ranges
                .iter()
                .any(|&(start, stop)| off >= start && end <= stop as u128)
    });
    if !valid_chunks {
        tracing::warn!(
            "MP4: sample/chunk table is inconsistent (chunk offset 0 or payload past \
             end of file); dropping {} locatable samples of this track",
            offsets.len()
        );
        offsets.clear();
    }

    // Any sample without a chunk of its own cannot be located. Padding it with
    // offset 0 would make `read_frame` return file-header bytes dressed up as
    // a media packet (the review caught exactly this), so drop such samples
    // instead: a shorter DTS-ordered index is safe, bogus packets are not.
    //
    // The truncation is only sound because `expand_stsc` proved that chunks are
    // mapped contiguously from the first one, so the addressed samples are
    // exactly ordinals `0..offsets.len()` and the shortfall is a *tail*. A
    // leading or middle gap makes `expand_stsc` refuse the whole track, since
    // then the surviving samples would keep the DTS of a different ordinal.
    if offsets.len() < sample_count {
        tracing::warn!(
            "MP4: {} of {} samples have no chunk mapping (stsc/stco/stsz disagree); \
             dropping the unaddressable tail",
            sample_count - offsets.len(),
            sample_count
        );
    }
    let sample_count = offsets.len();

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
            // Every table above is padded to `sample_count`, but stay defensive:
            // a mismatch must not panic on index.
            offset: offsets.get(i).copied().unwrap_or(0),
            size: sizes.get(i).copied().unwrap_or(0),
            dts: dts_i,
            pts: pts_i,
            duration: d,
            is_key: is_key.get(i).copied().unwrap_or(false),
        });
    }
    samples
}

/// Whether a sample index entry can be read as media data.
///
/// `offset == 0` means the sample was never assigned a chunk (a file header
/// starts at byte 0, so reading there would hand back garbage); `size == 0`
/// yields an empty packet. Both are skipped instead of being emitted.
fn sample_is_readable(sample: &SampleEntry) -> bool {
    sample.offset != 0 && sample.size != 0
}

/// Clamp a container `timescale` (raw `u32` from `mdhd`) into the `i32`
/// denominator a [`Rational`] time base can hold.
///
/// `as i32` would wrap values above `i32::MAX` into a negative (or zero)
/// denominator, which silently corrupts every timestamp derived from the
/// stream's time base, so an unrepresentable value returns `None` and the
/// caller reports an error instead of substituting `1/1`.
fn timescale_den(timescale: u32) -> Option<i32> {
    match i32::try_from(timescale) {
        Ok(v) if v > 0 => Some(v),
        _ => None,
    }
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
        if &box_type == b"trak" {
            let mut tables = TrackTables::default();
            parse_trak(io, payload_start, payload_end, &mut tables)?;
            tracks.push(tables);
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
        if &box_type == b"mdia" {
            parse_mdia(io, payload_start, payload_end, tables)?
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
                let nls = data.get(4).map(|b| (*b & 0x03) + 1).unwrap_or(4);
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

    /// Media range covering a synthetic test file's payload.
    ///
    /// Sample data is only accepted inside an `mdat` payload, so unit tests that
    /// hand-build `TrackTables` must declare the region their offsets point into.
    fn media_range(start: u64, end: u64) -> Vec<(u64, u64)> {
        vec![(start, end)]
    }

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
            media_ranges: media_range(1, u64::MAX / 2),
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
            stco: vec![2048],
            media_ranges: media_range(1, u64::MAX / 2),
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

    #[test]
    fn stsc_first_chunk_past_table_yields_no_bogus_samples() {
        // Malformed `stsc`: `first_chunk` far beyond the real chunk count.
        // Regression guard for two bugs in a row: the slice used to panic, and
        // the fix then padded the missing offsets with 0 — which made
        // `read_frame` hand back file-header bytes as a media packet.
        let tables = TrackTables {
            timescale: 1000,
            stts: vec![(2, 40)],
            stsc: vec![(999, 1, 1)],
            stsz: vec![1, 1],
            stco: vec![100],
            media_ranges: media_range(1, u64::MAX / 2),
            ..Default::default()
        };
        let samples = build_sample_index(&tables);
        // No sample can be located, so the index must be empty — not two
        // samples pointing at offset 0.
        assert!(
            samples.is_empty(),
            "unaddressable samples must be dropped, got {samples:?}"
        );
    }

    #[test]
    fn stsc_non_monotonic_table_drops_the_whole_track() {
        // Second entry claims an *earlier* chunk than the first. The old code
        // patched the slice bounds and silently dropped chunk 0's samples, so
        // the two survivors carried ordinals 0/1 while sitting at chunk 1 —
        // i.e. the wrong size and DTS. A table that cannot be walked in
        // increasing order proves nothing, so the track must be refused.
        let tables = TrackTables {
            timescale: 1000,
            stts: vec![(3, 10)],
            stsc: vec![(3, 1, 1), (1, 1, 1)],
            stsz: vec![1, 1, 1],
            stco: vec![100, 200],
            media_ranges: media_range(1, u64::MAX / 2),
            ..Default::default()
        };
        let samples = build_sample_index(&tables);
        assert!(
            samples.is_empty(),
            "a non-monotonic stsc must not shift sample ordinals: {samples:?}"
        );
    }

    #[test]
    fn sample_is_readable_rejects_zero_offset_and_zero_size() {
        // A file header lives at offset 0; such an entry must never be emitted.
        let unaddressed = SampleEntry {
            offset: 0,
            size: 16,
            dts: 0,
            pts: 0,
            duration: 1,
            is_key: true,
        };
        let empty = SampleEntry {
            offset: 32,
            size: 0,
            ..unaddressed
        };
        let good = SampleEntry {
            offset: 32,
            size: 16,
            ..unaddressed
        };
        assert!(!sample_is_readable(&unaddressed));
        assert!(!sample_is_readable(&empty));
        assert!(sample_is_readable(&good));
    }

    #[test]
    fn demuxing_a_broken_stsc_never_returns_header_bytes() {
        // End-to-end: a file whose stsc cannot address any sample must yield no
        // packets at all, instead of packets built from the `ftyp` bytes.
        let mut data = build_minimal_mp4();
        // Locate "stsc" and set its first_chunk to an impossible value.
        let pos = data
            .windows(4)
            .position(|w| w == b"stsc")
            .expect("minimal mp4 contains an stsc box");
        // stsc payload: version/flags (4) + entry_count (4) + first_chunk (4)
        let first_chunk_at = pos + 4 + 4 + 4;
        data[first_chunk_at..first_chunk_at + 4].copy_from_slice(&be32(9_999));

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
        // The only track has no locatable sample, so it is not published and the
        // file reports that nothing is playable (rather than exposing a stream
        // that can never yield a packet).
        assert!(demuxer.read_header(&mut ctx).is_err());
        assert!(ctx.streams.is_empty());
        assert!(demuxer.read_frame(&mut ctx).unwrap().is_none());
    }

    #[test]
    fn build_sample_index_pads_mismatched_tables() {
        // stsz is authoritative for the sample count (2) while stts claims 4:
        // the index must follow stsz, ignore the out-of-range stss entry, and
        // never panic on a table mismatch.
        let tables = TrackTables {
            timescale: 1000,
            stts: vec![(4, 10)],
            stsc: vec![(1, 4, 1)],
            stsz: vec![3, 3],
            stco: vec![2048],
            media_ranges: media_range(1, u64::MAX / 2),
            stss: vec![99],
            ..Default::default()
        };
        let samples = build_sample_index(&tables);
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[1].size, 3);
        // Out-of-range stss index is ignored rather than panicking.
        assert!(samples.iter().all(|s| !s.is_key));
    }

    #[test]
    fn truncated_mp4_header_returns_error_not_panic() {
        // Truncate a valid file inside the moov box.
        let mut data = build_minimal_mp4();
        let keep = data.len() / 2;
        data.truncate(keep);
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
        // Either a clean error or an empty track list is fine — never a panic.
        let _ = demuxer.read_header(&mut ctx);
    }

    #[test]
    fn timescale_den_reports_unrepresentable_values() {
        assert_eq!(timescale_den(1000), Some(1000));
        assert_eq!(timescale_den(90_000), Some(90_000));
        // Raw u32 values above i32::MAX used to become negative denominators,
        // and the first fix substituted 1/1 — both corrupt timestamps. They
        // must now be reported as unrepresentable.
        assert_eq!(timescale_den(u32::MAX), None);
        assert_eq!(timescale_den(i32::MAX as u32 + 1), None);
        assert_eq!(timescale_den(0), None);
    }

    #[test]
    fn track_with_unrepresentable_timescale_is_rejected_not_coerced() {
        // End-to-end: patch mdhd's timescale to 2^31 and confirm the track is
        // dropped (no stream promising 1/1 ticks over real tick timestamps).
        let mut data = build_minimal_mp4();
        let pos = data
            .windows(4)
            .position(|w| w == b"mdhd")
            .expect("minimal mp4 contains an mdhd box");
        // mdhd v0 payload: version/flags(4) creation(4) modification(4) timescale(4)
        let timescale_at = pos + 4 + 4 + 4 + 4;
        data[timescale_at..timescale_at + 4].copy_from_slice(&be32(0x8000_0000));

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
        // All tracks rejected ⇒ report an error instead of a successful empty
        // stream list (which reads like a legitimately empty file).
        let err = demuxer
            .read_header(&mut ctx)
            .expect_err("every track has an unusable timescale");
        assert!(matches!(err, RsError::InvalidData(_)), "got {err:?}");
        assert!(
            ctx.streams.is_empty(),
            "a track whose timescale cannot form a time base must not be published: {:?}",
            ctx.streams.iter().map(|s| s.time_base).collect::<Vec<_>>()
        );
    }

    // ── Regression tests mirroring the independent boundary probe ─────────

    /// Build a one- or two-track MP4 with the given `mdhd` timescale and chunk
    /// offset, mirroring the reviewer's fixture generator.
    fn build_probe_mp4(tracks: &[(u32, usize, usize, u64)]) -> Vec<u8> {
        let ftyp = box_bytes(b"ftyp", b"isom\0\0\0\0isom");
        let media: [&[u8]; 2] = [b"AAA", b"BBBB"];
        let mdat = box_bytes(b"mdat", b"AAABBBB");
        let first_payload = (ftyp.len() + 8) as u64;

        let mut moov_payload = Vec::new();
        for &(timescale, sample_count, per_chunk, offset) in tracks {
            let sizes: Vec<u32> = media[..sample_count].iter().map(|m| m.len() as u32).collect();

            let mut audio_entry = vec![0u8; 28];
            audio_entry[16..18].copy_from_slice(&be16(2)); // channel count
            audio_entry[18..20].copy_from_slice(&be16(16)); // sample size
            audio_entry[24..28].copy_from_slice(&be32(48_000 << 16));
            let mut stsd_rest = be32(1).to_vec();
            stsd_rest.extend_from_slice(&box_bytes(b"mp4a", &audio_entry));
            let stsd = box_bytes(b"stsd", &fullbox(0, 0, &stsd_rest));

            let mut stts_rest = be32(1).to_vec();
            stts_rest.extend_from_slice(&be32(sample_count as u32));
            stts_rest.extend_from_slice(&be32(1000));
            let stts = box_bytes(b"stts", &fullbox(0, 0, &stts_rest));

            let mut stsc_rest = be32(1).to_vec();
            stsc_rest.extend_from_slice(&be32(1));
            stsc_rest.extend_from_slice(&be32(per_chunk as u32));
            stsc_rest.extend_from_slice(&be32(1));
            let stsc = box_bytes(b"stsc", &fullbox(0, 0, &stsc_rest));

            let mut stsz_rest = be32(0).to_vec();
            stsz_rest.extend_from_slice(&be32(sample_count as u32));
            for size in &sizes {
                stsz_rest.extend_from_slice(&be32(*size));
            }
            let stsz = box_bytes(b"stsz", &fullbox(0, 0, &stsz_rest));

            let actual_offset = offset.max(first_payload);
            let actual_offset = if offset == u64::MAX { offset } else { actual_offset };
            let mut stco_rest = be32(1).to_vec();
            stco_rest.extend_from_slice(&be32(actual_offset as u32));
            let stco = box_bytes(b"stco", &fullbox(0, 0, &stco_rest));

            let mut mdhd_rest = be32(0).to_vec();
            mdhd_rest.extend_from_slice(&be32(0));
            mdhd_rest.extend_from_slice(&be32(timescale));
            mdhd_rest.extend_from_slice(&be32(1000 * sample_count as u32));
            mdhd_rest.extend_from_slice(&be16(0x55c4));
            mdhd_rest.extend_from_slice(&be16(0));
            let mdhd = box_bytes(b"mdhd", &fullbox(0, 0, &mdhd_rest));

            let mut hdlr_rest = be32(0).to_vec();
            hdlr_rest.extend_from_slice(b"soun");
            hdlr_rest.extend_from_slice(&[0u8; 13]);
            let hdlr = box_bytes(b"hdlr", &fullbox(0, 0, &hdlr_rest));

            let stbl = box_bytes(b"stbl", &[stsd, stts, stsc, stsz, stco].concat());
            let minf = box_bytes(b"minf", &stbl);
            let mdia = box_bytes(b"mdia", &[mdhd, hdlr, minf].concat());
            moov_payload.extend_from_slice(&box_bytes(b"trak", &mdia));
        }

        let moov = box_bytes(b"moov", &moov_payload);
        [ftyp, mdat, moov].concat()
    }

    fn ctx_from(data: Vec<u8>) -> FormatContext {
        FormatContext {
            input: None,
            output: None,
            streams: Vec::new(),
            io: Some(IOContext::from_buffer(data)),
            metadata: rsmpeg_util::Dict::new(),
            duration: 0,
            bit_rate: 0,
            filename: None,
            format_name: Some("mp4".into()),
        }
    }

    fn read_all(demuxer: &mut MP4Demuxer, ctx: &mut FormatContext) -> Vec<Packet> {
        let mut out = Vec::new();
        while let Some(p) = demuxer.read_frame(ctx).unwrap() {
            out.push(p);
            assert!(out.len() < 10, "unexpected packet count");
        }
        out
    }

    #[test]
    fn probe_control_two_samples_read_back_verbatim() {
        let mut ctx = ctx_from(build_probe_mp4(&[(1000, 2, 2, 0)]));
        let mut demuxer = MP4Demuxer::default();
        demuxer.read_header(&mut ctx).unwrap();
        let packets = read_all(&mut demuxer, &mut ctx);
        assert_eq!(packets.len(), 2);
        assert_eq!(packets[0].data.as_ref(), b"AAA");
        assert_eq!(packets[0].stream_index, 0);
        assert_eq!(packets[1].data.as_ref(), b"BBBB");
        assert_eq!(packets[1].dts, Some(1000));
    }

    #[test]
    fn probe_locatable_prefix_kept_without_fake_tail() {
        // stsc maps only one sample per chunk while stsz/stts describe two: the
        // locatable prefix is kept, and no invented tail packet appears.
        let mut ctx = ctx_from(build_probe_mp4(&[(1000, 2, 1, 0)]));
        let mut demuxer = MP4Demuxer::default();
        demuxer.read_header(&mut ctx).unwrap();
        let packets = read_all(&mut demuxer, &mut ctx);
        assert_eq!(packets.len(), 1, "got {:?}", packets.len());
        assert_eq!(packets[0].data.as_ref(), b"AAA");
        assert_eq!(packets[0].dts, Some(0));
    }

    /// Build the reviewer's two-chunk fixture: `mdat` holds `AAA` then `BBBB`,
    /// `stts` ticks by 1000 per sample, and `stsc` is supplied verbatim as
    /// `(first_chunk, samples_per_chunk)` runs.
    ///
    /// Mirrors `focused-probe/make_fixtures.py`, where
    /// `first_chunk = 2` produced the failing case: chunk 1 has no declared
    /// samples, so the old expansion put "BBBB" at ordinal 0 and emitted the
    /// truncated packet `BBB` with DTS 0.
    fn build_two_chunk_mp4(stsc_runs: &[(u32, u32)]) -> Vec<u8> {
        build_mp4_with_chunks(&[b"AAA", b"BBBB"], stsc_runs)
    }

    /// Same fixture, but with an arbitrary chunk layout, so chunk runs can be
    /// made to skip a chunk in the middle.
    fn build_mp4_with_chunks(chunks: &[&[u8]], stsc_runs: &[(u32, u32)]) -> Vec<u8> {
        let ftyp = box_bytes(b"ftyp", b"isom\0\0\0\0isom");
        let payload: Vec<u8> = chunks.concat();
        let mdat = box_bytes(b"mdat", &payload);
        let media = (ftyp.len() + 8) as u32;

        let mut audio_entry = vec![0u8; 28];
        audio_entry[16..18].copy_from_slice(&be16(2));
        audio_entry[18..20].copy_from_slice(&be16(16));
        audio_entry[24..28].copy_from_slice(&be32(48_000 << 16));
        let mut stsd_rest = be32(1).to_vec();
        stsd_rest.extend_from_slice(&box_bytes(b"mp4a", &audio_entry));
        let stsd = box_bytes(b"stsd", &fullbox(0, 0, &stsd_rest));

        let mut stts_rest = be32(1).to_vec();
        stts_rest.extend_from_slice(&be32(chunks.len() as u32));
        stts_rest.extend_from_slice(&be32(1000));
        let stts = box_bytes(b"stts", &fullbox(0, 0, &stts_rest));

        let mut stsc_rest = be32(stsc_runs.len() as u32).to_vec();
        for &(first_chunk, per_chunk) in stsc_runs {
            stsc_rest.extend_from_slice(&be32(first_chunk));
            stsc_rest.extend_from_slice(&be32(per_chunk));
            stsc_rest.extend_from_slice(&be32(1));
        }
        let stsc = box_bytes(b"stsc", &fullbox(0, 0, &stsc_rest));

        let mut stsz_rest = be32(0).to_vec();
        stsz_rest.extend_from_slice(&be32(chunks.len() as u32));
        for chunk in chunks {
            stsz_rest.extend_from_slice(&be32(chunk.len() as u32));
        }
        let stsz = box_bytes(b"stsz", &fullbox(0, 0, &stsz_rest));

        let mut stco_rest = be32(chunks.len() as u32).to_vec();
        let mut next = media;
        for chunk in chunks {
            stco_rest.extend_from_slice(&be32(next));
            next += chunk.len() as u32;
        }
        let stco = box_bytes(b"stco", &fullbox(0, 0, &stco_rest));

        let mut mdhd_rest = be32(0).to_vec();
        mdhd_rest.extend_from_slice(&be32(0));
        mdhd_rest.extend_from_slice(&be32(1000));
        mdhd_rest.extend_from_slice(&be32(1000 * chunks.len() as u32));
        mdhd_rest.extend_from_slice(&be16(0x55c4));
        mdhd_rest.extend_from_slice(&be16(0));
        let mdhd = box_bytes(b"mdhd", &fullbox(0, 0, &mdhd_rest));

        let mut hdlr_rest = be32(0).to_vec();
        hdlr_rest.extend_from_slice(b"soun");
        hdlr_rest.extend_from_slice(&[0u8; 13]);
        let hdlr = box_bytes(b"hdlr", &fullbox(0, 0, &hdlr_rest));

        let stbl = box_bytes(b"stbl", &[stsd, stts, stsc, stsz, stco].concat());
        let minf = box_bytes(b"minf", &stbl);
        let mdia = box_bytes(b"mdia", &[mdhd, hdlr, minf].concat());
        let moov = box_bytes(b"moov", &box_bytes(b"trak", &mdia));
        [ftyp, mdat, moov].concat()
    }

    #[test]
    fn review_control_two_chunks_keep_verbatim_payloads_and_dts() {
        // Baseline for the two cases below: a complete mapping must still be
        // decoded verbatim, with the DTS coming from the sample table.
        let mut ctx = ctx_from(build_two_chunk_mp4(&[(1, 1)]));
        let mut demuxer = MP4Demuxer::default();
        demuxer.read_header(&mut ctx).unwrap();
        let packets = read_all(&mut demuxer, &mut ctx);
        let got: Vec<(Option<i64>, &[u8])> = packets
            .iter()
            .map(|p| (p.dts, p.data.as_ref()))
            .collect();
        assert_eq!(
            got,
            vec![(Some(0), b"AAA".as_slice()), (Some(1000), b"BBBB".as_slice())],
            "a complete stsc mapping must be decoded as-is"
        );
    }

    #[test]
    fn review_missing_first_chunk_mapping_emits_nothing() {
        // Review P1 reproduction: `stsc.first_chunk == 2` with two chunks, so
        // chunk 1 has no declared sample count. Previously the surviving
        // samples slid down one ordinal each and the demuxer returned `BBB`
        // (a truncation of "BBBB" sized by sample 0) stamped with DTS 0.
        //
        // A chunk with no mapping cannot be proven to hold *these* samples, so
        // neither the shifted packet nor a "guess" may be emitted. Dropping
        // every sample leaves the only track unplayable, and `read_header`
        // reports the file instead of publishing an empty stream.
        let mut ctx = ctx_from(build_two_chunk_mp4(&[(2, 1)]));
        let mut demuxer = MP4Demuxer::default();
        let header = demuxer.read_header(&mut ctx);
        let packets = if header.is_ok() {
            read_all(&mut demuxer, &mut ctx)
        } else {
            Vec::new()
        };
        let rendered: Vec<(Option<i64>, Vec<u8>)> = packets
            .iter()
            .map(|p| (p.dts, p.data.to_vec()))
            .collect();
        assert!(
            rendered.is_empty(),
            "no packet may be emitted from an unprovable chunk mapping: {rendered:?}"
        );
    }

    #[test]
    fn review_chunk_run_starting_at_zero_emits_nothing() {
        // Same class of bug through a different door: `stsc.first_chunk` is
        // 1-based, so an entry claiming chunk 0 cannot be honoured (subtracting
        // one wraps, and clamping to zero silently mis-assigns the run). The
        // mapping is unprovable, so no packet may be emitted.
        let data = build_two_chunk_mp4(&[(0, 1)]);
        let mut ctx = ctx_from(data);
        let mut demuxer = MP4Demuxer::default();
        let header = demuxer.read_header(&mut ctx);
        let packets = if header.is_ok() {
            read_all(&mut demuxer, &mut ctx)
        } else {
            Vec::new()
        };
        assert!(
            packets.is_empty(),
            "a zero-based chunk run must not shift ordinals: {:?}",
            packets.iter().map(|p| p.data.to_vec()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn review_multi_run_mapping_still_decodes_in_order() {
        // The complement of the rejections above: a two-run `stsc`
        // (`first_chunk` 1 → 1 sample each, then 2 → 1 sample each) must keep
        // working, so the guards do not over-reject legitimate files.
        let data = build_mp4_with_chunks(
            &[b"AAA", b"BBBB", b"CC", b"DDDD"],
            &[(1, 1), (3, 1)],
        );
        let mut ctx = ctx_from(data);
        let mut demuxer = MP4Demuxer::default();
        demuxer.read_header(&mut ctx).unwrap();
        let packets = read_all(&mut demuxer, &mut ctx);
        let got: Vec<(Option<i64>, &[u8])> = packets
            .iter()
            .map(|p| (p.dts, p.data.as_ref()))
            .collect();
        assert_eq!(
            got,
            vec![
                (Some(0), b"AAA".as_slice()),
                (Some(1000), b"BBBB".as_slice()),
                (Some(2000), b"CC".as_slice()),
                (Some(3000), b"DDDD".as_slice()),
            ],
            "a contiguous multi-run mapping must decode verbatim, in order"
        );
    }

    #[test]
    fn probe_surviving_track_packets_match_the_selected_stream() {
        // Review P1: dropping track 0 left the surviving track's Stream and
        // Packet indices at 1, while the player selects position 0.
        // Track 1 starts at the second payload ("BBBB").
        let second_payload = (box_bytes(b"ftyp", b"isom\0\0\0\0isom").len() + 8 + 3) as u64;
        let data = build_probe_mp4(&[(0x8000_0000, 1, 1, 0), (1000, 2, 1, second_payload)]);
        let mut ctx = ctx_from(data);
        let mut demuxer = MP4Demuxer::default();
        demuxer.read_header(&mut ctx).unwrap();

        assert_eq!(ctx.streams.len(), 1);
        assert_eq!(
            ctx.streams[0].index, 0,
            "the surviving stream must be renumbered to its position"
        );
        let selected = ctx
            .find_best_stream(MediaType::Audio)
            .expect("one audio stream survives");
        assert_eq!(selected, 0);

        let packets = read_all(&mut demuxer, &mut ctx);
        // Only one chunk is declared, so one sample is locatable; what matters
        // is that it is routed to the stream the player actually selected.
        assert_eq!(packets.len(), 1, "got {packets:?}");
        assert_eq!(
            packets[0].stream_index, selected,
            "packet.stream_index must match the selected stream"
        );
        assert_eq!(&packets[0].data[..], b"BBB");
        assert!(packets.iter().all(|p| p.stream_index == selected));
    }

    #[test]
    fn review_track_without_locatable_samples_is_not_published() {
        // Third review round: dropping a track's samples still left it in
        // `ctx.streams`, so `find_best_stream` selected index 0 (the broken
        // track) while every packet belonged to index 1. Selection must only
        // ever see playable tracks.
        let broken = (1000u32, 1usize, 1usize, u64::MAX); // chunk offset 0
        let second_payload = (box_bytes(b"ftyp", b"isom    isom").len() + 8 + 3) as u64;
        let healthy = (1000u32, 2usize, 1usize, second_payload);
        let mut data = build_probe_mp4(&[broken, healthy]);
        // Force the first track's chunk offset to 0.
        let pos = data.windows(4).position(|w| w == b"stco").expect("stco box");
        let offset_at = pos + 4 + 4 + 4;
        data[offset_at..offset_at + 4].copy_from_slice(&be32(0));

        let mut ctx = ctx_from(data);
        let mut demuxer = MP4Demuxer::default();
        demuxer.read_header(&mut ctx).unwrap();

        // Only the healthy track is published, renumbered to position 0.
        let indices: Vec<usize> = ctx.streams.iter().map(|s| s.index).collect();
        assert_eq!(indices, vec![0], "streams must contain playable tracks only");
        let selected = ctx.find_best_stream(MediaType::Audio);
        assert_eq!(selected, Some(0));

        let packets = read_all(&mut demuxer, &mut ctx);
        assert_eq!(packets.len(), 1, "got {packets:?}");
        assert_eq!(
            packets[0].stream_index, 0,
            "packets must be routed to the selected stream"
        );
        assert_eq!(&packets[0].data[..], b"BBB");
    }

    #[test]
    fn review_sample_offsets_must_point_inside_mdat() {
        // Offset 4 is inside the file but inside the `ftyp` box, not the media
        // payload; the old check (non-zero, before EOF) let it through and
        // returned the bytes "ftyp" as a packet.
        let mut data = build_probe_mp4(&[(1000, 2, 1, 0)]);
        let pos = data.windows(4).position(|w| w == b"stco").expect("stco box");
        let offset_at = pos + 4 + 4 + 4;
        data[offset_at..offset_at + 4].copy_from_slice(&be32(4));

        let mut ctx = ctx_from(data);
        let mut demuxer = MP4Demuxer::default();
        let header = demuxer.read_header(&mut ctx);
        let packets = if header.is_ok() {
            read_all(&mut demuxer, &mut ctx)
        } else {
            Vec::new()
        };
        assert!(
            packets.is_empty(),
            "container header bytes must never become a packet: {:?}",
            packets.iter().map(|p| p.data.to_vec()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn review_sample_spanning_two_boxes_is_rejected() {
        // A sample that starts inside `mdat` but runs past its end (into `moov`)
        // must be rejected: the range check is on the whole sample, not the start.
        let mut data = build_probe_mp4(&[(1000, 2, 2, 0)]);
        let pos = data.windows(4).position(|w| w == b"stco").expect("stco box");
        let offset_at = pos + 4 + 4 + 4;
        // mdat payload starts after ftyp (24 bytes) + 8-byte mdat header.
        let mdat_payload_start = 24u32 + 8;
        // Point two bytes before the last media byte so aAAA straddles the end.
        data[offset_at..offset_at + 4].copy_from_slice(&be32(mdat_payload_start + 5));

        let mut ctx = ctx_from(data);
        let mut demuxer = MP4Demuxer::default();
        let header = demuxer.read_header(&mut ctx);
        let packets = if header.is_ok() {
            read_all(&mut demuxer, &mut ctx)
        } else {
            Vec::new()
        };
        assert!(
            packets.is_empty(),
            "a sample crossing the mdat boundary must be dropped: {packets:?}"
        );
    }

    #[test]
    fn probe_zero_timescale_is_an_error_not_seconds() {
        // mdhd timescale 0 must not be coerced to 1/1 (which read ticks as seconds).
        let mut ctx = ctx_from(build_probe_mp4(&[(0, 2, 2, 0)]));
        let mut demuxer = MP4Demuxer::default();
        let err = demuxer
            .read_header(&mut ctx)
            .expect_err("timescale 0 cannot form a time base");
        assert!(matches!(err, RsError::InvalidData(_)), "got {err:?}");
        assert!(ctx.streams.is_empty());
    }

    #[test]
    fn probe_all_tracks_invalid_reports_error() {
        let mut ctx = ctx_from(build_probe_mp4(&[(0x8000_0000, 2, 2, 0)]));
        let mut demuxer = MP4Demuxer::default();
        let err = demuxer
            .read_header(&mut ctx)
            .expect_err("no usable track remains");
        assert!(matches!(err, RsError::InvalidData(_)), "got {err:?}");
        assert_eq!(ctx.streams.len(), 0);
    }

    #[test]
    fn probe_zero_chunk_offset_never_reads_the_header() {
        // A chunk starting at byte 0 would hand back `\x14 f t y p`. Without a
        // file-size bound the accumulated sizes gave later samples a nonzero
        // offset, so a single-sample check is not enough — the whole track must
        // be dropped.
        let data = build_probe_mp4(&[(1000, 2, 2, u64::MAX)]); // u64::MAX ⇒ patched to 0 below
        let mut data = data;
        // Locate the first `stco` entry and force it to 0.
        let pos = data.windows(4).position(|w| w == b"stco").expect("stco box");
        // stco payload: version/flags(4) + entry_count(4) + first offset(4)
        let offset_at = pos + 4 + 4 + 4;
        data[offset_at..offset_at + 4].copy_from_slice(&be32(0));

        let mut ctx = ctx_from(data);
        let mut demuxer = MP4Demuxer::default();
        let header = demuxer.read_header(&mut ctx);
        let packets = if header.is_ok() {
            read_all(&mut demuxer, &mut ctx)
        } else {
            Vec::new()
        };
        assert!(
            packets.is_empty(),
            "no packet may be produced from a chunk at offset 0: {:?}",
            packets.iter().map(|p| p.data.to_vec()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn probe_track_payload_past_eof_is_dropped() {
        // A chunk offset inside the file but whose payload runs past EOF must
        // not be read either.
        let mut data = build_probe_mp4(&[(1000, 2, 2, 0)]);
        let pos = data.windows(4).position(|w| w == b"stco").expect("stco box");
        let offset_at = pos + 4 + 4 + 4;
        // Point the chunk at the last byte of the file: 7 bytes cannot fit.
        let last = (data.len() - 1) as u32;
        data[offset_at..offset_at + 4].copy_from_slice(&be32(last));

        let mut ctx = ctx_from(data);
        let mut demuxer = MP4Demuxer::default();
        // The payload is outside every `mdat`, so the track is unplayable.
        let header = demuxer.read_header(&mut ctx);
        let packets = if header.is_ok() {
            read_all(&mut demuxer, &mut ctx)
        } else {
            Vec::new()
        };
        assert!(packets.is_empty(), "samples past EOF must be dropped");
    }
}
