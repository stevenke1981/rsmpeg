use crate::format::InputFormat;
use crate::format_context::FormatContext;
use crate::io_context::IOContext;
use crate::probe::ProbeScore;
use crate::stream::Stream;
use bytes::Bytes;
use rsmpeg_codec::{CodecId, CodecParameters, Packet, PacketFlags};
use rsmpeg_util::{MediaType, Rational, RsError, RsResult, SampleFormat};
use std::io::SeekFrom;

const CHUNK_SIZE: usize = 8192;
const MAX_FRAME_HDR_LEN: usize = 16;
const SEEK_SCAN_WINDOW: usize = 256 * 1024;

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

#[inline]
fn crc16_update(crc: u16, byte: u8) -> u16 {
    (crc << 8) ^ CRC16_TABLE[(((crc >> 8) as u8) ^ byte) as usize]
}

#[derive(Debug, Clone, Copy)]
struct FlacFrameHeader {
    hdr_len: usize,
    blocksize: u32,
    sample_rate: u32,
    channels: u16,
    bits_per_sample: u16,
    variable_blocksize: bool,
    coded_number: u64,
}

impl FlacFrameHeader {
    fn matches_stream(&self, sr: u32, ch: u16, bps: u16) -> bool {
        self.sample_rate == sr && self.channels == ch && self.bits_per_sample == bps
    }
}

fn parse_frame_header(buf: &[u8], stream_sr: u32, stream_bps: u16) -> Option<FlacFrameHeader> {
    if buf.len() < 6 {
        return None;
    }
    if buf[0] != 0xFF || (buf[1] & 0xFE) != 0xF8 {
        return None;
    }
    let variable_blocksize = (buf[1] & 0x01) != 0;

    let bs_code = buf[2] >> 4;
    let sr_code = buf[2] & 0x0F;
    if bs_code == 0 || sr_code == 15 {
        return None;
    }

    let ch_code = buf[3] >> 4;
    let bps_code = (buf[3] >> 1) & 0x07;
    let reserved1 = buf[3] & 0x01;
    if ch_code > 10 || bps_code == 3 || reserved1 != 0 {
        return None;
    }

    let channels = if ch_code <= 7 {
        u16::from(ch_code) + 1
    } else {
        2
    };

    let mut pos = 4usize;
    let b0 = buf[pos];
    pos += 1;
    let coded_number = if b0 & 0x80 == 0 {
        u64::from(b0)
    } else {
        let ones = (!b0).leading_zeros() as usize;
        if ones == 1 || ones > 7 || pos + (ones - 1) > buf.len() {
            return None;
        }
        let mut val = if ones < 7 {
            u64::from(b0 & ((1u8 << (7 - ones)) - 1))
        } else {
            0
        };
        for _ in 1..ones {
            let cb = buf[pos];
            pos += 1;
            if cb & 0xC0 != 0x80 {
                return None;
            }
            val = (val << 6) | u64::from(cb & 0x3F);
        }
        val
    };

    let blocksize: u32 = match bs_code {
        1 => 192,
        2..=5 => 576 << (bs_code - 2),
        6 => {
            if pos >= buf.len() {
                return None;
            }
            let bs = u32::from(buf[pos]) + 1;
            pos += 1;
            bs
        }
        7 => {
            if pos + 1 >= buf.len() {
                return None;
            }
            let bs = u32::from(u16::from_be_bytes([buf[pos], buf[pos + 1]])) + 1;
            pos += 2;
            bs
        }
        8..=15 => 256 << (bs_code - 8),
        _ => return None,
    };

    let sample_rate: u32 = match sr_code {
        0 => stream_sr,
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
        12 => {
            if pos >= buf.len() {
                return None;
            }
            let sr = u32::from(buf[pos]) * 1000;
            pos += 1;
            sr
        }
        13 => {
            if pos + 1 >= buf.len() {
                return None;
            }
            let sr = u32::from(u16::from_be_bytes([buf[pos], buf[pos + 1]]));
            pos += 2;
            sr
        }
        14 => {
            if pos + 1 >= buf.len() {
                return None;
            }
            let sr = u32::from(u16::from_be_bytes([buf[pos], buf[pos + 1]])) * 10;
            pos += 2;
            sr
        }
        _ => return None,
    };
    if sample_rate == 0 {
        return None;
    }

    let bits_per_sample: u16 = match bps_code {
        0 => stream_bps,
        1 => 8,
        2 => 12,
        4 => 16,
        5 => 20,
        6 => 24,
        7 => 32,
        _ => return None,
    };
    if !(4..=32).contains(&bits_per_sample) {
        return None;
    }

    if pos >= buf.len() {
        return None;
    }
    let expected_crc8 = buf[pos];
    if crc8(&buf[..pos]) != expected_crc8 {
        return None;
    }

    Some(FlacFrameHeader {
        hdr_len: pos + 1,
        blocksize,
        sample_rate,
        channels,
        bits_per_sample,
        variable_blocksize,
        coded_number,
    })
}

fn frame_start_sample(hdr: &FlacFrameHeader, min_blocksize: u16, max_blocksize: u16) -> u64 {
    if hdr.variable_blocksize {
        hdr.coded_number
    } else {
        let nominal = if min_blocksize > 0 && min_blocksize == max_blocksize {
            u64::from(min_blocksize)
        } else {
            u64::from(hdr.blocksize)
        };
        hdr.coded_number.saturating_mul(nominal)
    }
}

/// FLAC (Free Lossless Audio Codec) stateful demuxer.
///
/// Parses the `fLaC` marker and all metadata blocks (`STREAMINFO`, `SEEKTABLE`,
/// `VORBIS_COMMENT`), emits CRC-verified FLAC frame packets from `read_frame`,
/// and supports sample-accurate `seek`.
#[derive(Default)]
pub struct FLACDemuxer {
    sample_rate: u32,
    channels: u16,
    bits_per_sample: u16,
    min_blocksize: u16,
    max_blocksize: u16,
    total_samples: u64,
    audio_start: u64,
    file_end: u64,
    seek_points: Vec<(u64, u64)>,
    current_sample: u64,
}

impl FLACDemuxer {
    fn reset_state(&mut self) {
        self.sample_rate = 0;
        self.channels = 0;
        self.bits_per_sample = 0;
        self.min_blocksize = 0;
        self.max_blocksize = 0;
        self.total_samples = 0;
        self.audio_start = 0;
        self.file_end = 0;
        self.seek_points.clear();
        self.current_sample = 0;
    }

    fn build_packet(&mut self, data: Vec<u8>, pos: u64, blocksize: u32) -> Packet {
        let den = self.sample_rate.min(i32::MAX as u32).max(1) as i32;
        let pts = self.current_sample.min(i64::MAX as u64) as i64;
        let mut packet = Packet::new(Bytes::from(data), 0);
        packet.pts = Some(pts);
        packet.dts = Some(pts);
        packet.duration = i64::from(blocksize);
        packet.pos = pos.min(i64::MAX as u64) as i64;
        packet.time_base = Rational::new(1, den);
        packet.flags = PacketFlags::KEY;
        self.current_sample = self.current_sample.saturating_add(u64::from(blocksize));
        packet
    }
}

impl InputFormat for FLACDemuxer {
    fn name(&self) -> &'static str {
        "flac"
    }

    fn description(&self) -> &'static str {
        "FLAC (Free Lossless Audio Codec)"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["flac"]
    }

    fn probe(&self, buf: &[u8]) -> ProbeScore {
        if buf.len() >= 4 && &buf[0..4] == b"fLaC" {
            ProbeScore::Certain
        } else {
            ProbeScore::NoMatch
        }
    }

    fn read_header(&mut self, ctx: &mut FormatContext) -> RsResult<()> {
        let io = ctx
            .io
            .as_mut()
            .ok_or_else(|| RsError::InvalidData("No IO context".into()))?;

        self.reset_state();
        io.seek(SeekFrom::Start(0))?;

        let mut magic = [0u8; 4];
        io.read_exact(&mut magic)?;
        if &magic != b"fLaC" {
            return Err(RsError::InvalidData("FLAC: missing fLaC marker".into()));
        }

        let mut streaminfo_raw: Option<Vec<u8>> = None;

        loop {
            let block_header_byte = io.read_u8()?;
            let is_last = (block_header_byte & 0x80) != 0;
            let block_type = block_header_byte & 0x7F;
            if block_type == 127 {
                return Err(RsError::InvalidData(
                    "FLAC: invalid metadata block type 127".into(),
                ));
            }
            let block_size = read_u24_be(io)?;

            match block_type {
                0 => {
                    if block_size < 34 {
                        return Err(RsError::InvalidData(
                            "FLAC: STREAMINFO block is truncated".into(),
                        ));
                    }
                    let info = io.read_bytes(34)?;
                    if block_size > 34 {
                        io.seek(SeekFrom::Current((block_size - 34) as i64))?;
                    }
                    self.min_blocksize = u16::from_be_bytes([info[0], info[1]]);
                    self.max_blocksize = u16::from_be_bytes([info[2], info[3]]);
                    self.sample_rate = (u32::from(info[10]) << 12)
                        | (u32::from(info[11]) << 4)
                        | (u32::from(info[12]) >> 4);
                    self.channels = u16::from((info[12] >> 1) & 0x07) + 1;
                    let bps_raw =
                        ((u32::from(info[12]) & 0x01) << 4) | ((u32::from(info[13]) >> 4) & 0x0F);
                    self.bits_per_sample = (bps_raw + 1) as u16;
                    self.total_samples = ((u64::from(info[13]) & 0x0F) << 32)
                        | u64::from(u32::from_be_bytes([info[14], info[15], info[16], info[17]]));
                    if streaminfo_raw.is_none() {
                        streaminfo_raw = Some(info);
                    }
                }
                3 => {
                    let num_entries = block_size / 18;
                    for _ in 0..num_entries {
                        let sample_number = io.read_u64_be()?;
                        let offset = io.read_u64_be()?;
                        let _num_samples = io.read_u16_be()?;
                        if sample_number != u64::MAX {
                            self.seek_points.push((sample_number, offset));
                        }
                    }
                    let rem = block_size % 18;
                    if rem > 0 {
                        io.seek(SeekFrom::Current(rem as i64))?;
                    }
                    self.seek_points.sort_by_key(|&(sample, _)| sample);
                }
                4 => {
                    let data = io.read_bytes(block_size as usize)?;
                    parse_vorbis_comments(&data, &mut ctx.metadata);
                }
                _ => {
                    if block_size > 0 {
                        io.seek(SeekFrom::Current(block_size as i64))?;
                    }
                }
            }

            if is_last {
                break;
            }
        }

        if streaminfo_raw.is_none() || self.sample_rate == 0 {
            return Err(RsError::InvalidData(
                "FLAC: missing or invalid STREAMINFO block".into(),
            ));
        }

        self.audio_start = io.tell()?;
        self.file_end = io.seek(SeekFrom::End(0))?;
        io.seek(SeekFrom::Start(self.audio_start))?;

        let duration_ms = {
            let ms = (u128::from(self.total_samples) * 1000) / u128::from(self.sample_rate);
            ms.min(i64::MAX as u128) as i64
        };

        let audio_bytes = self.file_end.saturating_sub(self.audio_start);
        let bit_rate = if self.total_samples > 0 && self.sample_rate > 0 {
            let br = (u128::from(audio_bytes) * 8 * u128::from(self.sample_rate))
                / u128::from(self.total_samples);
            Some(br.min(u64::MAX as u128) as u64)
        } else {
            None
        };

        let sample_format = match self.bits_per_sample {
            1..=8 => SampleFormat::U8,
            9..=16 => SampleFormat::S16,
            17..=32 => SampleFormat::S32,
            _ => SampleFormat::S16,
        };

        let time_base_den = self.sample_rate.min(i32::MAX as u32).max(1) as i32;
        let mut stream = Stream::new(0, CodecId::Flac);
        stream.media_type = MediaType::Audio;
        stream.codec_params = CodecParameters {
            codec_id: CodecId::Flac,
            media_type: MediaType::Audio,
            width: None,
            height: None,
            pixel_format: None,
            sample_format: Some(sample_format),
            sample_rate: Some(self.sample_rate),
            channels: Some(self.channels),
            bit_rate,
            extradata: streaminfo_raw,
            h264_bitstream_format: Default::default(),
        };
        stream.time_base = Rational::new(1, time_base_den);
        stream.duration = self.total_samples.min(i64::MAX as u64) as i64;
        ctx.duration = duration_ms;
        if let Some(br) = bit_rate {
            ctx.bit_rate = br;
        }
        ctx.streams.push(stream);

        tracing::info!(
            "FLAC: {}ch, {}Hz, {}bit, {}samples ({}ms)",
            self.channels,
            self.sample_rate,
            self.bits_per_sample,
            self.total_samples,
            duration_ms
        );
        Ok(())
    }

    fn read_frame(&mut self, ctx: &mut FormatContext) -> RsResult<Option<Packet>> {
        let io = ctx
            .io
            .as_mut()
            .ok_or_else(|| RsError::InvalidData("No IO context".into()))?;

        let mut pos = io.tell()?;
        if pos < self.audio_start {
            pos = io.seek(SeekFrom::Start(self.audio_start))?;
        }
        if pos >= self.file_end {
            return Ok(None);
        }

        let mut buf = Vec::with_capacity(CHUNK_SIZE);
        let mut eof_reached = false;
        let mut sync_scan = 0usize;
        let mut found: Option<(usize, FlacFrameHeader)> = None;

        // Locate the start of the current FLAC frame.
        loop {
            while !eof_reached && buf.len().saturating_sub(sync_scan) < MAX_FRAME_HDR_LEN + 1 {
                read_more_to_buf(io, &mut buf, pos, self.file_end, &mut eof_reached)?;
            }
            let scan_limit = if eof_reached {
                buf.len().saturating_sub(5)
            } else {
                buf.len().saturating_sub(MAX_FRAME_HDR_LEN)
            };

            while sync_scan < scan_limit {
                if buf[sync_scan] == 0xFF && (buf[sync_scan + 1] & 0xFE) == 0xF8 {
                    if let Some(hdr) = parse_frame_header(
                        &buf[sync_scan..],
                        self.sample_rate,
                        self.bits_per_sample,
                    ) {
                        if hdr.matches_stream(self.sample_rate, self.channels, self.bits_per_sample)
                        {
                            found = Some((sync_scan, hdr));
                            break;
                        }
                    }
                }
                sync_scan += 1;
            }

            if found.is_some() {
                break;
            }
            if eof_reached {
                io.seek(SeekFrom::Start(self.file_end))?;
                return Ok(None);
            }
            // Discard scanned bytes before the unconfirmed tail so memory stays bounded.
            if sync_scan > 0 {
                buf.drain(..sync_scan);
                pos += sync_scan as u64;
                sync_scan = 0;
            }
        }

        let (start_idx, cur_hdr) = found.expect("loop broke with found = Some");
        let frame_start_offset = pos + start_idx as u64;
        if start_idx > 0 {
            buf.drain(..start_idx);
            self.current_sample =
                frame_start_sample(&cur_hdr, self.min_blocksize, self.max_blocksize);
        }

        // Minimum end offset: header + at least 1 byte subframe + 2 bytes CRC-16.
        let min_next_i = cur_hdr.hdr_len + 3;
        while !eof_reached && buf.len() < min_next_i + MAX_FRAME_HDR_LEN + 1 {
            read_more_to_buf(
                io,
                &mut buf,
                frame_start_offset,
                self.file_end,
                &mut eof_reached,
            )?;
        }

        if buf.len() <= min_next_i {
            io.seek(SeekFrom::Start(self.file_end))?;
            return Ok(Some(self.build_packet(
                buf,
                frame_start_offset,
                cur_hdr.blocksize,
            )));
        }

        let mut scan_i = min_next_i;
        let mut running_crc16 = crc16(&buf[..scan_i - 2]);

        loop {
            while !eof_reached && buf.len().saturating_sub(scan_i) < MAX_FRAME_HDR_LEN + 1 {
                read_more_to_buf(
                    io,
                    &mut buf,
                    frame_start_offset,
                    self.file_end,
                    &mut eof_reached,
                )?;
            }

            let limit = if eof_reached {
                buf.len().saturating_sub(5)
            } else {
                buf.len().saturating_sub(MAX_FRAME_HDR_LEN)
            };

            if scan_i >= limit {
                if eof_reached {
                    break;
                }
                continue;
            }

            for i in scan_i..limit {
                if buf[i] == 0xFF && (buf[i + 1] & 0xFE) == 0xF8 {
                    let expected_crc16 = u16::from_be_bytes([buf[i - 2], buf[i - 1]]);
                    if running_crc16 == expected_crc16 {
                        if let Some(next_hdr) =
                            parse_frame_header(&buf[i..], self.sample_rate, self.bits_per_sample)
                        {
                            if next_hdr.matches_stream(
                                self.sample_rate,
                                self.channels,
                                self.bits_per_sample,
                            ) {
                                io.seek(SeekFrom::Start(frame_start_offset + i as u64))?;
                                buf.truncate(i);
                                return Ok(Some(self.build_packet(
                                    buf,
                                    frame_start_offset,
                                    cur_hdr.blocksize,
                                )));
                            }
                        }
                    }
                }
                running_crc16 = crc16_update(running_crc16, buf[i - 2]);
            }
            scan_i = limit;

            if eof_reached {
                break;
            }
        }

        io.seek(SeekFrom::Start(self.file_end))?;
        Ok(Some(self.build_packet(
            buf,
            frame_start_offset,
            cur_hdr.blocksize,
        )))
    }

    fn seek(&mut self, ctx: &mut FormatContext, timestamp_ms: i64) -> RsResult<()> {
        let io = ctx
            .io
            .as_mut()
            .ok_or_else(|| RsError::InvalidData("No IO context".into()))?;

        if self.sample_rate == 0 || self.audio_start >= self.file_end {
            io.seek(SeekFrom::Start(self.audio_start))?;
            self.current_sample = 0;
            return Ok(());
        }

        let target_sample =
            ((timestamp_ms.max(0) as u128 * u128::from(self.sample_rate)) / 1000) as u64;

        if target_sample == 0 {
            io.seek(SeekFrom::Start(self.audio_start))?;
            self.current_sample = 0;
            return Ok(());
        }

        if self.total_samples > 0 && target_sample >= self.total_samples {
            io.seek(SeekFrom::Start(self.file_end))?;
            self.current_sample = self.total_samples;
            return Ok(());
        }

        let guess_offset = if !self.seek_points.is_empty() {
            let idx = self
                .seek_points
                .partition_point(|&(sample, _)| sample <= target_sample);
            if idx > 0 {
                self.audio_start.saturating_add(self.seek_points[idx - 1].1)
            } else {
                self.audio_start
            }
        } else if self.total_samples > 0 {
            let audio_len = u128::from(self.file_end.saturating_sub(self.audio_start));
            let rel = (audio_len * u128::from(target_sample)) / u128::from(self.total_samples);
            self.audio_start.saturating_add(rel as u64)
        } else {
            self.audio_start
        }
        .clamp(self.audio_start, self.file_end);

        if guess_offset >= self.file_end {
            io.seek(SeekFrom::Start(self.file_end))?;
            self.current_sample = self.total_samples;
            return Ok(());
        }

        // Scan forward up to SEEK_SCAN_WINDOW for the next valid FLAC frame header.
        let fwd_len = ((self.file_end - guess_offset) as usize).min(SEEK_SCAN_WINDOW);
        io.seek(SeekFrom::Start(guess_offset))?;
        let fwd_buf = read_up_to(io, fwd_len)?;
        if fwd_buf.len() >= 6 {
            for i in 0..=fwd_buf.len() - 6 {
                if fwd_buf[i] == 0xFF && (fwd_buf[i + 1] & 0xFE) == 0xF8 {
                    if let Some(hdr) =
                        parse_frame_header(&fwd_buf[i..], self.sample_rate, self.bits_per_sample)
                    {
                        if hdr.matches_stream(self.sample_rate, self.channels, self.bits_per_sample)
                        {
                            let frame_pos = guess_offset + i as u64;
                            io.seek(SeekFrom::Start(frame_pos))?;
                            self.current_sample =
                                frame_start_sample(&hdr, self.min_blocksize, self.max_blocksize);
                            return Ok(());
                        }
                    }
                }
            }
        }

        // If `guess_offset` fell inside the final frame near EOF, scan backward to its header.
        let back_start = guess_offset.saturating_sub(64 * 1024).max(self.audio_start);
        if back_start < guess_offset {
            let back_end = guess_offset
                .saturating_add(MAX_FRAME_HDR_LEN as u64)
                .min(self.file_end);
            let back_len = (back_end - back_start) as usize;
            io.seek(SeekFrom::Start(back_start))?;
            let back_buf = read_up_to(io, back_len)?;
            if back_buf.len() >= 6 {
                let max_i = ((guess_offset - back_start) as usize).min(back_buf.len() - 6);
                for i in (0..=max_i).rev() {
                    if back_buf[i] == 0xFF && (back_buf[i + 1] & 0xFE) == 0xF8 {
                        if let Some(hdr) = parse_frame_header(
                            &back_buf[i..],
                            self.sample_rate,
                            self.bits_per_sample,
                        ) {
                            if hdr.matches_stream(
                                self.sample_rate,
                                self.channels,
                                self.bits_per_sample,
                            ) {
                                let frame_pos = back_start + i as u64;
                                io.seek(SeekFrom::Start(frame_pos))?;
                                self.current_sample = frame_start_sample(
                                    &hdr,
                                    self.min_blocksize,
                                    self.max_blocksize,
                                );
                                return Ok(());
                            }
                        }
                    }
                }
            }
        }

        io.seek(SeekFrom::Start(self.file_end))?;
        self.current_sample = self.total_samples;
        Ok(())
    }
}

/// Read a 24-bit big-endian unsigned integer.
fn read_u24_be(io: &mut IOContext) -> RsResult<u32> {
    let mut buf = [0u8; 3];
    io.read_exact(&mut buf)?;
    Ok((u32::from(buf[0]) << 16) | (u32::from(buf[1]) << 8) | u32::from(buf[2]))
}

fn read_more_to_buf(
    io: &mut IOContext,
    buf: &mut Vec<u8>,
    base_offset: u64,
    file_end: u64,
    eof_reached: &mut bool,
) -> RsResult<()> {
    let current_end = base_offset.saturating_add(buf.len() as u64);
    if current_end >= file_end {
        *eof_reached = true;
        return Ok(());
    }
    let remaining = (file_end - current_end) as usize;
    let to_read = remaining.min(CHUNK_SIZE);
    let old_len = buf.len();
    buf.resize(old_len + to_read, 0);

    loop {
        match io.read(&mut buf[old_len..]) {
            Ok(n) => {
                buf.truncate(old_len + n);
                if n == 0 || base_offset.saturating_add(buf.len() as u64) >= file_end {
                    *eof_reached = true;
                }
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                buf.truncate(old_len);
                return Err(e.into());
            }
        }
    }
}

fn read_up_to(io: &mut IOContext, len: usize) -> RsResult<Vec<u8>> {
    let mut buf = vec![0u8; len];
    let mut filled = 0usize;
    while filled < len {
        match io.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }
    }
    buf.truncate(filled);
    Ok(buf)
}

fn parse_vorbis_comments(data: &[u8], metadata: &mut rsmpeg_util::Dict) {
    if data.len() < 4 {
        return;
    }
    let vendor_len = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let Some(after_vendor) = 4usize.checked_add(vendor_len) else {
        return;
    };
    let Some(count_end) = after_vendor.checked_add(4) else {
        return;
    };
    if count_end > data.len() {
        return;
    }
    let count = u32::from_le_bytes([
        data[after_vendor],
        data[after_vendor + 1],
        data[after_vendor + 2],
        data[after_vendor + 3],
    ]) as usize;

    let mut pos = count_end;
    for _ in 0..count {
        if pos + 4 > data.len() {
            break;
        }
        let len =
            u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        pos += 4;
        let Some(end) = pos.checked_add(len) else {
            break;
        };
        if end > data.len() {
            break;
        }
        let entry = String::from_utf8_lossy(&data[pos..end]);
        pos = end;
        if let Some((key, value)) = entry.split_once('=') {
            metadata.insert(key.to_ascii_lowercase(), value.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            format_name: Some("flac".into()),
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

    fn append_metadata_block(out: &mut Vec<u8>, block_type: u8, is_last: bool, body: &[u8]) {
        let header_byte = block_type | if is_last { 0x80 } else { 0x00 };
        out.push(header_byte);
        let len = body.len() as u32;
        out.push(((len >> 16) & 0xFF) as u8);
        out.push(((len >> 8) & 0xFF) as u8);
        out.push((len & 0xFF) as u8);
        out.extend_from_slice(body);
    }

    fn build_vorbis_comment_block(vendor: &str, comments: &[&str]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        v.extend_from_slice(vendor.as_bytes());
        v.extend_from_slice(&(comments.len() as u32).to_le_bytes());
        for c in comments {
            v.extend_from_slice(&(c.len() as u32).to_le_bytes());
            v.extend_from_slice(c.as_bytes());
        }
        v
    }

    fn encode_utf8_u64(val: u64) -> Vec<u8> {
        if val < 0x80 {
            return vec![val as u8];
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
        let mut out = vec![prefix_ones | top];
        for i in (0..cont).rev() {
            out.push(0x80 | (((val >> (i * 6)) as u8) & 0x3F));
        }
        out
    }

    /// Build a minimal valid 16-bit mono CONSTANT FLAC frame (`bs_code = 8` -> 256 samples,
    /// `sr_code = 10` -> 48000 Hz, `ch_code = 0` -> 1ch, `bps_code = 4` -> 16-bit).
    fn build_constant_frame_48k_mono_16(frame_num: u64, constant_val: i16) -> Vec<u8> {
        let mut frame = vec![
            0xFF,
            0xF8,          // fixed-blocksize
            (8 << 4) | 10, // bs=256 (code 8), sr=48000 (code 10)
            4 << 1,        // ch=1, bps=16 (code 4), reserved=0
        ];
        frame.extend_from_slice(&encode_utf8_u64(frame_num));
        let c8 = crc8(&frame);
        frame.push(c8);
        // CONSTANT subframe header (0) + 16-bit signed value
        frame.push(0x00);
        frame.extend_from_slice(&constant_val.to_be_bytes());
        let c16 = crc16(&frame);
        frame.extend_from_slice(&c16.to_be_bytes());
        frame
    }

    #[test]
    fn test_streaminfo_36bit_total_samples_and_metadata_blocks() {
        // 36-bit total_samples > 2^32: 0x0000_0005_1234_5678
        let big_samples: u64 = 0x0000_0005_1234_5678;
        let si = build_streaminfo(256, 256, 48_000, 2, 24, big_samples);
        let vc = build_vorbis_comment_block("rsmpeg", &["TITLE=Test Song", "ARTIST=Pure Rust"]);

        let mut flac = Vec::from(&b"fLaC"[..]);
        append_metadata_block(&mut flac, 0, false, &si);
        append_metadata_block(&mut flac, 4, false, &vc);
        append_metadata_block(&mut flac, 1, true, &[0u8; 16]); // PADDING

        let mut ctx = ctx_with(flac);
        let mut demuxer = FLACDemuxer::default();
        demuxer.read_header(&mut ctx).unwrap();

        assert_eq!(demuxer.total_samples, big_samples);
        assert_eq!(demuxer.sample_rate, 48_000);
        assert_eq!(demuxer.channels, 2);
        assert_eq!(demuxer.bits_per_sample, 24);
        assert_eq!(ctx.streams.len(), 1);
        let stream = &ctx.streams[0];
        assert_eq!(stream.codec_id, CodecId::Flac);
        assert_eq!(stream.codec_params.sample_rate, Some(48_000));
        assert_eq!(stream.codec_params.channels, Some(2));
        assert_eq!(stream.codec_params.sample_format, Some(SampleFormat::S32));
        assert_eq!(stream.codec_params.extradata.as_ref(), Some(&si));
        assert_eq!(stream.time_base, Rational::new(1, 48_000));
        assert_eq!(stream.duration, big_samples as i64);
        assert_eq!(ctx.metadata.get("title"), Some("Test Song"));
        assert_eq!(ctx.metadata.get("artist"), Some("Pure Rust"));
    }

    #[test]
    fn test_multi_frame_read_and_seek_with_seektable_and_linear() {
        let frames: Vec<Vec<u8>> = (0..5u64)
            .map(|i| build_constant_frame_48k_mono_16(i, (i as i16 + 1) * 100))
            .collect();

        // Compute relative byte offsets from audio_start for SEEKTABLE
        let mut offsets = Vec::new();
        let mut acc = 0u64;
        for f in &frames {
            offsets.push(acc);
            acc += f.len() as u64;
        }

        let mut seektable = Vec::new();
        for (i, &off) in offsets.iter().enumerate() {
            let sample = (i as u64) * 256;
            seektable.extend_from_slice(&sample.to_be_bytes());
            seektable.extend_from_slice(&off.to_be_bytes());
            seektable.extend_from_slice(&256u16.to_be_bytes());
        }
        // Include a placeholder entry that must be ignored
        seektable.extend_from_slice(&u64::MAX.to_be_bytes());
        seektable.extend_from_slice(&0u64.to_be_bytes());
        seektable.extend_from_slice(&0u16.to_be_bytes());

        let total_samples = 5 * 256;
        let si = build_streaminfo(256, 256, 48_000, 1, 16, total_samples);

        let mut flac = Vec::from(&b"fLaC"[..]);
        append_metadata_block(&mut flac, 0, false, &si);
        append_metadata_block(&mut flac, 3, true, &seektable);
        for f in &frames {
            flac.extend_from_slice(f);
        }

        let mut ctx = ctx_with(flac);
        let mut demuxer = FLACDemuxer::default();
        demuxer.read_header(&mut ctx).unwrap();
        assert_eq!(demuxer.seek_points.len(), 5);
        assert!(ctx.streams[0].codec_params.bit_rate.is_some());

        // Read all 5 frames sequentially and check exact packet boundaries + PTS
        let mut read_packets = Vec::new();
        while let Some(pkt) = demuxer.read_frame(&mut ctx).unwrap() {
            read_packets.push(pkt);
        }
        assert_eq!(read_packets.len(), 5);
        for (i, pkt) in read_packets.iter().enumerate() {
            assert_eq!(pkt.data.as_ref(), frames[i].as_slice());
            assert_eq!(pkt.pts, Some((i as i64) * 256));
            assert_eq!(pkt.duration, 256);
            assert_eq!(pkt.time_base, Rational::new(1, 48_000));
            assert!(pkt.is_key());
        }
        assert!(demuxer.read_frame(&mut ctx).unwrap().is_none());

        // Seek using SEEKTABLE to ~16 ms -> sample 768 -> frame 3
        // (frame 3 starts at sample 768 = 16.0 ms at 48 kHz)
        demuxer.seek(&mut ctx, 16).unwrap();
        let pkt3 = demuxer.read_frame(&mut ctx).unwrap().unwrap();
        assert_eq!(pkt3.data.as_ref(), frames[3].as_slice());
        assert_eq!(pkt3.pts, Some(768));

        // Now test linear seek WITHOUT SEEKTABLE
        let mut flac_no_st = Vec::from(&b"fLaC"[..]);
        append_metadata_block(&mut flac_no_st, 0, true, &si);
        for f in &frames {
            flac_no_st.extend_from_slice(f);
        }
        let mut ctx2 = ctx_with(flac_no_st);
        let mut demuxer2 = FLACDemuxer::default();
        demuxer2.read_header(&mut ctx2).unwrap();
        assert!(demuxer2.seek_points.is_empty());

        demuxer2.seek(&mut ctx2, 12).unwrap(); // 576 samples -> lands inside or at frame 2/3
        let seek_pkt = demuxer2.read_frame(&mut ctx2).unwrap().unwrap();
        assert!(seek_pkt.pts.unwrap() >= 512);

        // Seek to 0 and past end
        demuxer2.seek(&mut ctx2, 0).unwrap();
        let first_pkt = demuxer2.read_frame(&mut ctx2).unwrap().unwrap();
        assert_eq!(first_pkt.pts, Some(0));
        assert_eq!(first_pkt.data.as_ref(), frames[0].as_slice());

        demuxer2.seek(&mut ctx2, 10_000).unwrap();
        assert!(demuxer2.read_frame(&mut ctx2).unwrap().is_none());
    }

    #[test]
    fn test_malformed_vorbis_comment_and_invalid_magic() {
        // Truncated/overflowing Vorbis comment length must not panic.
        let si = build_streaminfo(256, 256, 44_100, 1, 16, 0);
        let mut bad_vc = Vec::new();
        bad_vc.extend_from_slice(&u32::MAX.to_le_bytes());
        let mut flac = Vec::from(&b"fLaC"[..]);
        append_metadata_block(&mut flac, 0, false, &si);
        append_metadata_block(&mut flac, 4, true, &bad_vc);

        let mut ctx = ctx_with(flac);
        let mut demuxer = FLACDemuxer::default();
        demuxer.read_header(&mut ctx).unwrap();
        assert!(demuxer.read_frame(&mut ctx).unwrap().is_none());

        // Non-FLAC input rejected cleanly
        let mut bad_ctx = ctx_with(b"RIFF0000WAVE".to_vec());
        let mut bad_demuxer = FLACDemuxer::default();
        assert!(bad_demuxer.read_header(&mut bad_ctx).is_err());
        assert!(bad_ctx.streams.is_empty());
    }
}
