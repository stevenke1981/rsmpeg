//! Pure-Rust SRT and WebVTT subtitle parser and decoder.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::path::Path;
use std::time::Duration;

use crate::codec::{Codec, CodecCapabilities, DecodeStatus, Decoder, Encoder};
use crate::codec_id::CodecId;
use crate::codec_parameters::CodecParameters;
use crate::frame::Frame;
use crate::packet::Packet;
use crate::picture_type::PictureType;
use rsmpeg_util::{MediaType, PixelFormat, RsError, RsResult, SampleFormat};

/// A single timed subtitle cue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubtitleCue {
    pub index: usize,
    pub start: Duration,
    pub end: Duration,
    pub text: String,
}

/// A parsed subtitle track containing ordered cues.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubtitleTrack {
    pub format: CodecId,
    pub cues: Vec<SubtitleCue>,
}

impl SubtitleTrack {
    /// Return the combined subtitle text active at media `position`, shifted by `offset_ms`.
    ///
    /// Uses standard player subtitle-delay semantics:
    /// - `offset_ms > 0`: subtitles are delayed (appear later), so a cue `[5.0s, 7.0s)` with
    ///   `offset_ms = +500` is active for `position` in `[5.5s, 7.5s)`. If `position < offset`,
    ///   `None` is returned.
    /// - `offset_ms < 0`: subtitles appear earlier, so a cue `[5.0s, 7.0s)` with
    ///   `offset_ms = -500` is active for `position` in `[4.5s, 6.5s)`.
    pub fn active_text_at(&self, position: Duration, offset_ms: i64) -> Option<String> {
        let adjusted = if offset_ms >= 0 {
            position.checked_sub(Duration::from_millis(offset_ms as u64))?
        } else {
            position.saturating_add(Duration::from_millis(offset_ms.unsigned_abs()))
        };

        let active: Vec<&str> = self
            .cues
            .iter()
            .filter(|cue| cue.start <= adjusted && adjusted < cue.end)
            .map(|cue| cue.text.as_str())
            .filter(|text| !text.is_empty())
            .collect();

        if active.is_empty() {
            None
        } else {
            Some(active.join("\n"))
        }
    }
}

/// Parse an SRT (SubRip) subtitle document into a [`SubtitleTrack`].
pub fn parse_srt(input: &str) -> RsResult<SubtitleTrack> {
    let normalized = normalize_text(input);
    let blocks = split_blocks(&normalized);
    let mut cues = Vec::with_capacity(blocks.len());

    for block in blocks {
        let (index, timing_line, text_lines) = if block[0].contains("-->") {
            (cues.len() + 1, block[0], &block[1..])
        } else if block.len() >= 2 && block[1].contains("-->") {
            let idx = block[0].trim().parse::<usize>().map_err(|_| {
                RsError::InvalidData(format!("SRT: invalid cue index '{}'", block[0].trim()).into())
            })?;
            (idx, block[1], &block[2..])
        } else {
            return Err(RsError::InvalidData(
                "SRT: cue block missing timing line".into(),
            ));
        };

        let (start, end) = parse_timing_line(timing_line, false)?;
        let raw_text = text_lines.join("\n");
        let text = clean_subtitle_text(&raw_text, false);
        cues.push(SubtitleCue {
            index,
            start,
            end,
            text,
        });
    }

    Ok(SubtitleTrack {
        format: CodecId::Srt,
        cues,
    })
}

/// Parse a WebVTT subtitle document into a [`SubtitleTrack`].
pub fn parse_webvtt(input: &str) -> RsResult<SubtitleTrack> {
    let normalized = normalize_text(input);
    let first_line = normalized.lines().next().unwrap_or("");
    if !is_webvtt_header(first_line) {
        return Err(RsError::InvalidData(
            "WebVTT: missing 'WEBVTT' header".into(),
        ));
    }

    let blocks = split_blocks(&normalized);
    let mut cues = Vec::new();

    for (block_idx, block) in blocks.into_iter().enumerate() {
        if block_idx == 0 {
            // Skip header metadata lines until the first blank line. If a file
            // omits the blank line after `WEBVTT`, salvage any cue lines inside block 0.
            if let Some(timing_pos) = block.iter().position(|line| line.contains("-->")) {
                let slice_start = if timing_pos >= 2 && !block[timing_pos - 1].contains(':') {
                    timing_pos - 1
                } else {
                    timing_pos
                };
                if slice_start > 0 {
                    let next_idx = cues.len() + 1;
                    cues.push(parse_webvtt_cue_block(&block[slice_start..], next_idx)?);
                }
            }
            continue;
        }

        if is_webvtt_ignored_block(block[0]) {
            continue;
        }

        let next_idx = cues.len() + 1;
        cues.push(parse_webvtt_cue_block(&block, next_idx)?);
    }

    Ok(SubtitleTrack {
        format: CodecId::WebVtt,
        cues,
    })
}

/// Read a subtitle file from disk and parse it as WebVTT or SRT.
pub fn parse_subtitle_file(path: &Path) -> RsResult<SubtitleTrack> {
    let bytes = std::fs::read(path)?;
    let text = String::from_utf8_lossy(&bytes);
    let trimmed = text.strip_prefix('\u{feff}').unwrap_or(&text).trim_start();

    let is_vtt_ext = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("vtt"));
    let first_line = trimmed.lines().next().unwrap_or("");

    if is_vtt_ext || is_webvtt_header(first_line) {
        parse_webvtt(&text)
    } else {
        parse_srt(&text)
    }
}

fn normalize_text(input: &str) -> Cow<'_, str> {
    let without_bom = input.strip_prefix('\u{feff}').unwrap_or(input);
    if without_bom.contains('\r') {
        Cow::Owned(without_bom.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        Cow::Borrowed(without_bom)
    }
}

fn split_blocks(text: &str) -> Vec<Vec<&str>> {
    let mut blocks = Vec::new();
    let mut current = Vec::new();

    for line in text.lines() {
        if line.trim().is_empty() {
            if !current.is_empty() {
                blocks.push(std::mem::take(&mut current));
            }
        } else {
            current.push(line);
        }
    }
    if !current.is_empty() {
        blocks.push(current);
    }
    blocks
}

fn is_webvtt_header(line: &str) -> bool {
    let line = line.trim_end_matches([' ', '\t']);
    line == "WEBVTT" || line.starts_with("WEBVTT ") || line.starts_with("WEBVTT\t")
}

fn is_webvtt_ignored_block(first_line: &str) -> bool {
    ["NOTE", "STYLE", "REGION"].iter().any(|&keyword| {
        first_line
            .strip_prefix(keyword)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '\t']))
    })
}

fn parse_webvtt_cue_block(block: &[&str], fallback_index: usize) -> RsResult<SubtitleCue> {
    let (index, timing_line, text_lines) = if block[0].contains("-->") {
        (fallback_index, block[0], &block[1..])
    } else if block.len() >= 2 && block[1].contains("-->") {
        let idx = block[0].trim().parse::<usize>().unwrap_or(fallback_index);
        (idx, block[1], &block[2..])
    } else {
        return Err(RsError::InvalidData(
            "WebVTT: cue block missing timing line".into(),
        ));
    };

    let (start, end) = parse_timing_line(timing_line, true)?;
    let raw_text = text_lines.join("\n");
    let text = clean_subtitle_text(&raw_text, true);
    Ok(SubtitleCue {
        index,
        start,
        end,
        text,
    })
}

fn parse_timing_line(line: &str, allow_short_mm_ss: bool) -> RsResult<(Duration, Duration)> {
    let (left, right) = line.split_once("-->").ok_or_else(|| {
        RsError::InvalidData(format!("subtitle timing line missing '-->': {line}").into())
    })?;

    let start = parse_timestamp(left.trim(), allow_short_mm_ss)?;
    let end_token = right.split_whitespace().next().unwrap_or("");
    let end = parse_timestamp(end_token, allow_short_mm_ss)?;
    if end < start {
        return Err(RsError::InvalidData(
            format!("subtitle cue end precedes start: {line}").into(),
        ));
    }
    Ok((start, end))
}

fn parse_timestamp(token: &str, allow_short_mm_ss: bool) -> RsResult<Duration> {
    let s = token.trim();
    if s.is_empty() {
        return Err(RsError::InvalidData("empty subtitle timestamp".into()));
    }

    let (hms_part, frac_part) = if let Some((l, r)) = s.split_once(',') {
        (l, r)
    } else if let Some((l, r)) = s.split_once('.') {
        (l, r)
    } else {
        return Err(RsError::InvalidData(
            format!("subtitle timestamp missing fractional separator: {s}").into(),
        ));
    };

    if frac_part.is_empty() || frac_part.len() > 9 || !frac_part.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(RsError::InvalidData(
            format!("invalid fractional seconds in timestamp: {s}").into(),
        ));
    }

    let mut nanos_buf = [b'0'; 9];
    nanos_buf[..frac_part.len()].copy_from_slice(frac_part.as_bytes());
    let nanos_str = std::str::from_utf8(&nanos_buf)
        .map_err(|_| RsError::InvalidData("invalid timestamp digits".into()))?;
    let nanos: u32 = nanos_str
        .parse()
        .map_err(|_| RsError::InvalidData("invalid timestamp nanoseconds".into()))?;

    let parts: Vec<&str> = hms_part.split(':').collect();
    let total_secs: u64 = match parts.as_slice() {
        [mm, ss] if allow_short_mm_ss => {
            if mm.len() < 2
                || !mm.bytes().all(|b| b.is_ascii_digit())
                || ss.len() != 2
                || !ss.bytes().all(|b| b.is_ascii_digit())
            {
                return Err(RsError::InvalidData(
                    format!("invalid MM:SS timestamp: {s}").into(),
                ));
            }
            let mins: u64 = mm
                .parse()
                .map_err(|_| RsError::InvalidData(format!("invalid minutes: {s}").into()))?;
            let secs: u64 = ss
                .parse()
                .map_err(|_| RsError::InvalidData(format!("invalid seconds: {s}").into()))?;
            if secs >= 60 {
                return Err(RsError::InvalidData(
                    format!("seconds out of range in timestamp: {s}").into(),
                ));
            }
            mins.checked_mul(60)
                .and_then(|m| m.checked_add(secs))
                .ok_or_else(|| RsError::InvalidData("subtitle timestamp overflow".into()))?
        }
        [hh, mm, ss] => {
            if hh.is_empty()
                || !hh.bytes().all(|b| b.is_ascii_digit())
                || mm.len() != 2
                || !mm.bytes().all(|b| b.is_ascii_digit())
                || ss.len() != 2
                || !ss.bytes().all(|b| b.is_ascii_digit())
            {
                return Err(RsError::InvalidData(
                    format!("invalid HH:MM:SS timestamp: {s}").into(),
                ));
            }
            let hours: u64 = hh
                .parse()
                .map_err(|_| RsError::InvalidData(format!("invalid hours: {s}").into()))?;
            let mins: u64 = mm
                .parse()
                .map_err(|_| RsError::InvalidData(format!("invalid minutes: {s}").into()))?;
            let secs: u64 = ss
                .parse()
                .map_err(|_| RsError::InvalidData(format!("invalid seconds: {s}").into()))?;
            if mins >= 60 || secs >= 60 {
                return Err(RsError::InvalidData(
                    format!("minutes/seconds out of range in timestamp: {s}").into(),
                ));
            }
            hours
                .checked_mul(3600)
                .and_then(|h| h.checked_add(mins * 60))
                .and_then(|m| m.checked_add(secs))
                .ok_or_else(|| RsError::InvalidData("subtitle timestamp overflow".into()))?
        }
        _ => {
            return Err(RsError::InvalidData(
                format!("invalid subtitle timestamp: {s}").into(),
            ));
        }
    };

    Ok(Duration::new(total_secs, nanos))
}

const MAX_TAG_INNER_LEN: usize = 64;

fn clean_subtitle_text(raw: &str, decode_entities: bool) -> String {
    let mut out = String::with_capacity(raw.len());
    let bytes = raw.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if raw[i..].starts_with("{\\") {
            if let Some(rel_end) = find_closing_delim(&raw[i + 2..], '}') {
                i = i + 2 + rel_end + 1;
                continue;
            }
        } else if bytes[i] == b'<' {
            if let Some(rel_end) = find_tag_close(&raw[i + 1..]) {
                let inner = &raw[i + 1..i + 1 + rel_end];
                if is_subtitle_tag(inner) {
                    i = i + 1 + rel_end + 1;
                    continue;
                }
            }
        }

        if let Some(ch) = raw[i..].chars().next() {
            out.push(ch);
            i += ch.len_utf8();
        } else {
            break;
        }
    }

    if decode_entities {
        unescape_webvtt_entities(&out)
    } else {
        out
    }
}

fn find_closing_delim(slice: &str, delim: char) -> Option<usize> {
    for (idx, ch) in slice.char_indices() {
        if idx > MAX_TAG_INNER_LEN || ch == '\n' {
            return None;
        }
        if ch == delim {
            return Some(idx);
        }
    }
    None
}

fn find_tag_close(slice: &str) -> Option<usize> {
    for (idx, ch) in slice.char_indices() {
        if idx > MAX_TAG_INNER_LEN || ch == '\n' || ch == '<' {
            return None;
        }
        if ch == '>' {
            return Some(idx);
        }
    }
    None
}

fn is_subtitle_tag(inner: &str) -> bool {
    if inner.is_empty() {
        return false;
    }
    let first = inner.as_bytes()[0];
    if first == b'/' {
        let name = inner[1..].trim_end();
        return !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-');
    }
    if first.is_ascii_digit() {
        // WebVTT inline timestamp cue tag like <00:01:02.000> or <01:02.000>
        return inner.contains(':')
            && inner
                .bytes()
                .all(|b| b.is_ascii_digit() || b == b':' || b == b'.');
    }
    if !first.is_ascii_alphabetic() {
        return false;
    }

    let trimmed = inner.strip_suffix('/').unwrap_or(inner).trim_end();
    if let Some((tag_name, _rest)) = trimmed.split_once(char::is_whitespace) {
        let known = matches!(
            tag_name.to_ascii_lowercase().as_str(),
            "font"
                | "span"
                | "c"
                | "v"
                | "lang"
                | "b"
                | "i"
                | "u"
                | "s"
                | "em"
                | "strong"
                | "ruby"
                | "rt"
                | "div"
                | "p"
                | "br"
        ) || tag_name.starts_with("c.");
        known || inner.contains('=') || inner.ends_with('/')
    } else {
        trimmed
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
    }
}

fn unescape_webvtt_entities(input: &str) -> String {
    if !input.contains('&') {
        return input.to_string();
    }
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(amp_idx) = rest.find('&') {
        out.push_str(&rest[..amp_idx]);
        let after = &rest[amp_idx..];
        let mut matched = false;
        for (entity, replacement) in [
            ("&amp;", "&"),
            ("&lt;", "<"),
            ("&gt;", ">"),
            ("&nbsp;", " "),
            ("&lrm;", "\u{200E}"),
            ("&rlm;", "\u{200F}"),
        ] {
            if let Some(stripped) = after.strip_prefix(entity) {
                out.push_str(replacement);
                rest = stripped;
                matched = true;
                break;
            }
        }
        if !matched {
            out.push('&');
            rest = &after[1..];
        }
    }
    out.push_str(rest);
    out
}

/// SRT (SubRip) subtitle codec descriptor.
pub struct SrtCodec;

impl Codec for SrtCodec {
    fn id(&self) -> CodecId {
        CodecId::Srt
    }

    fn media_type(&self) -> MediaType {
        MediaType::Subtitle
    }

    fn name(&self) -> &'static str {
        "srt"
    }

    fn long_name(&self) -> &'static str {
        "SubRip (SRT) subtitle"
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
        Ok(Box::new(SubtitleDecoder::new(CodecId::Srt)))
    }

    fn create_encoder(&self) -> RsResult<Box<dyn Encoder>> {
        Err(RsError::Unsupported("SRT encoding not implemented".into()))
    }
}

/// WebVTT subtitle codec descriptor.
pub struct WebVttCodec;

impl Codec for WebVttCodec {
    fn id(&self) -> CodecId {
        CodecId::WebVtt
    }

    fn media_type(&self) -> MediaType {
        MediaType::Subtitle
    }

    fn name(&self) -> &'static str {
        "webvtt"
    }

    fn long_name(&self) -> &'static str {
        "WebVTT subtitle"
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
        Ok(Box::new(SubtitleDecoder::new(CodecId::WebVtt)))
    }

    fn create_encoder(&self) -> RsResult<Box<dyn Encoder>> {
        Err(RsError::Unsupported(
            "WebVTT encoding not implemented".into(),
        ))
    }
}

/// Subtitle packet decoder passing timed UTF-8 subtitle payload frames through.
pub struct SubtitleDecoder {
    codec_id: CodecId,
    params: CodecParameters,
    pending: VecDeque<Frame>,
    eof: bool,
}

impl SubtitleDecoder {
    pub fn new(codec_id: CodecId) -> Self {
        Self {
            codec_id,
            params: CodecParameters::new(codec_id),
            pending: VecDeque::new(),
            eof: false,
        }
    }

    pub fn from_params(params: &CodecParameters) -> RsResult<Self> {
        match params.codec_id {
            CodecId::Srt | CodecId::WebVtt => {
                let mut dec = Self::new(params.codec_id);
                dec.params = params.clone();
                Ok(dec)
            }
            other => Err(RsError::Unsupported(
                format!("SubtitleDecoder does not support codec {:?}", other).into(),
            )),
        }
    }

    pub fn set_parameters(&mut self, params: CodecParameters) {
        self.params = params;
    }
}

impl Decoder for SubtitleDecoder {
    fn codec_id(&self) -> CodecId {
        self.codec_id
    }

    fn send_packet(&mut self, packet: Option<&Packet>) -> RsResult<()> {
        match packet {
            Some(pkt) => {
                if self.eof {
                    return Err(RsError::Codec(
                        "cannot send packet after end-of-stream; call reset() first".into(),
                    ));
                }
                let data = pkt.data.to_vec();
                let len = data.len();
                self.pending.push_back(Frame {
                    data: vec![data],
                    linesize: vec![len],
                    width: 0,
                    height: 0,
                    pixel_format: PixelFormat::None,
                    sample_format: SampleFormat::None,
                    sample_rate: 0,
                    channels: 0,
                    samples: 0,
                    pts: pkt.pts,
                    duration: pkt.duration,
                    time_base: pkt.time_base,
                    key_frame: true,
                    pict_type: PictureType::None,
                });
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

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use rsmpeg_util::Rational;

    #[test]
    fn test_parse_srt_basic_bom_crlf_and_tags() {
        let srt = "\u{feff}1\r\n00:00:01,500 --> 00:00:04,250\r\n<i>Hello</i> <b>World</b>!\r\n{\\an8}Line two\r\n\r\n2\r\n00:00:05.000 --> 00:00:07.500\r\n<font color=\"#ff0\">2 < 3 and x < y > z</font>\r\n";
        let track = parse_srt(srt).unwrap();
        assert_eq!(track.format, CodecId::Srt);
        assert_eq!(track.cues.len(), 2);

        assert_eq!(track.cues[0].index, 1);
        assert_eq!(track.cues[0].start, Duration::from_millis(1500));
        assert_eq!(track.cues[0].end, Duration::from_millis(4250));
        assert_eq!(track.cues[0].text, "Hello World!\nLine two");

        assert_eq!(track.cues[1].index, 2);
        assert_eq!(track.cues[1].start, Duration::from_millis(5000));
        assert_eq!(track.cues[1].end, Duration::from_millis(7500));
        assert_eq!(track.cues[1].text, "2 < 3 and x < y > z");
    }

    #[test]
    fn test_parse_srt_unclosed_math_and_empty() {
        assert_eq!(
            parse_srt("   \r\n\r\n").unwrap(),
            SubtitleTrack {
                format: CodecId::Srt,
                cues: Vec::new()
            }
        );

        let srt = "1\n00:00:00,000 --> 00:00:02,000\nIf a < b then return";
        let track = parse_srt(srt).unwrap();
        assert_eq!(track.cues[0].text, "If a < b then return");
    }

    #[test]
    fn test_parse_webvtt_features() {
        let vtt = "\u{feff}WEBVTT - Example\r\nKind: captions\r\nLanguage: en\r\n\r\nNOTE This is a multi-line\r\ncomment block\r\n\r\nSTYLE\r\n::cue { color: yellow; }\r\n\r\nREGION\r\nid:fred\r\nwidth:40%\r\n\r\n01:02.345 --> 01:05.000 align:start position:10%\r\n<v Alice>Hello &amp; <u>welcome</u>!</v>\r\n\r\ncue-two\r\n00:01:06.000 --> 00:01:08.500\r\n<c.cyan>Second cue</c> &lt;ok&gt;\r\n";

        let track = parse_webvtt(vtt).unwrap();
        assert_eq!(track.format, CodecId::WebVtt);
        assert_eq!(track.cues.len(), 2);

        assert_eq!(track.cues[0].index, 1);
        assert_eq!(track.cues[0].start, Duration::from_millis(62_345));
        assert_eq!(track.cues[0].end, Duration::from_millis(65_000));
        assert_eq!(track.cues[0].text, "Hello & welcome!");

        assert_eq!(track.cues[1].index, 2);
        assert_eq!(track.cues[1].start, Duration::from_millis(66_000));
        assert_eq!(track.cues[1].end, Duration::from_millis(68_500));
        assert_eq!(track.cues[1].text, "Second cue <ok>");
    }

    #[test]
    fn test_parse_webvtt_rejects_missing_header() {
        assert!(parse_webvtt("00:00:01.000 --> 00:00:02.000\nHi").is_err());
        assert!(parse_webvtt("WEBVTTX\n\n00:01.000 --> 00:02.000\nHi").is_err());
    }

    #[test]
    fn test_active_text_at_overlapping_and_offsets() {
        let srt = "\
1
00:00:05,000 --> 00:00:07,000
First line

2
00:00:06,000 --> 00:00:08,000
Second line
";
        let track = parse_srt(srt).unwrap();

        // Zero offset
        assert_eq!(track.active_text_at(Duration::from_millis(4999), 0), None);
        assert_eq!(
            track.active_text_at(Duration::from_millis(5000), 0),
            Some("First line".to_string())
        );
        assert_eq!(
            track.active_text_at(Duration::from_millis(6500), 0),
            Some("First line\nSecond line".to_string())
        );
        assert_eq!(
            track.active_text_at(Duration::from_millis(7000), 0),
            Some("Second line".to_string())
        );
        assert_eq!(track.active_text_at(Duration::from_millis(8000), 0), None);

        // Positive offset (+500 ms delay): cue [5.0, 7.0) appears at [5.5, 7.5)
        assert_eq!(track.active_text_at(Duration::from_millis(200), 500), None);
        assert_eq!(track.active_text_at(Duration::from_millis(5499), 500), None);
        assert_eq!(
            track.active_text_at(Duration::from_millis(5500), 500),
            Some("First line".to_string())
        );
        assert_eq!(
            track.active_text_at(Duration::from_millis(6500), 500),
            Some("First line\nSecond line".to_string())
        );
        assert_eq!(track.active_text_at(Duration::from_millis(8500), 500), None);

        // Negative offset (-500 ms earlier): cue [5.0, 7.0) appears at [4.5, 6.5)
        assert_eq!(
            track.active_text_at(Duration::from_millis(4499), -500),
            None
        );
        assert_eq!(
            track.active_text_at(Duration::from_millis(4500), -500),
            Some("First line".to_string())
        );
        assert_eq!(
            track.active_text_at(Duration::from_millis(5500), -500),
            Some("First line\nSecond line".to_string())
        );
        assert_eq!(
            track.active_text_at(Duration::from_millis(7500), -500),
            None
        );
    }

    #[test]
    fn test_parse_subtitle_file_autodetect() {
        let dir = std::env::temp_dir();
        let pid = std::process::id();
        let vtt_path = dir.join(format!("rsmpeg_test_{pid}.vtt"));
        let srt_path = dir.join(format!("rsmpeg_test_{pid}.srt"));

        std::fs::write(&vtt_path, "WEBVTT\n\n00:01.000 --> 00:02.000\nVTT text\n").unwrap();
        std::fs::write(&srt_path, "1\n00:00:01,000 --> 00:00:02,000\nSRT text\n").unwrap();

        let vtt_track = parse_subtitle_file(&vtt_path).unwrap();
        assert_eq!(vtt_track.format, CodecId::WebVtt);
        assert_eq!(vtt_track.cues[0].text, "VTT text");

        let srt_track = parse_subtitle_file(&srt_path).unwrap();
        assert_eq!(srt_track.format, CodecId::Srt);
        assert_eq!(srt_track.cues[0].text, "SRT text");

        let _ = std::fs::remove_file(&vtt_path);
        let _ = std::fs::remove_file(&srt_path);
    }

    #[test]
    fn test_subtitle_codecs_send_receive_reset_eos() {
        let srt_codec = SrtCodec;
        assert_eq!(srt_codec.id(), CodecId::Srt);
        assert_eq!(srt_codec.media_type(), MediaType::Subtitle);
        assert_eq!(srt_codec.name(), "srt");
        assert!(srt_codec.capabilities().can_decode);
        assert!(!srt_codec.capabilities().can_encode);
        assert!(srt_codec.create_encoder().is_err());

        let vtt_codec = WebVttCodec;
        assert_eq!(vtt_codec.id(), CodecId::WebVtt);
        assert_eq!(vtt_codec.media_type(), MediaType::Subtitle);
        assert_eq!(vtt_codec.name(), "webvtt");
        assert!(vtt_codec.create_encoder().is_err());

        let mut decoder = srt_codec.create_decoder().unwrap();
        assert_eq!(decoder.codec_id(), CodecId::Srt);
        assert!(matches!(
            decoder.receive_frame().unwrap(),
            DecodeStatus::NeedMoreInput
        ));

        let pkt = Packet {
            data: Bytes::from_static(b"Subtitle line"),
            pts: Some(1500),
            dts: Some(1500),
            duration: 2000,
            stream_index: 0,
            flags: crate::packet::PacketFlags::KEY,
            pos: 0,
            time_base: Rational::new(1, 1000),
        };
        decoder.send_packet(Some(&pkt)).unwrap();
        match decoder.receive_frame().unwrap() {
            DecodeStatus::Frame(frame) => {
                assert_eq!(frame.data, vec![b"Subtitle line".to_vec()]);
                assert_eq!(frame.pts, Some(1500));
                assert_eq!(frame.duration, 2000);
                assert_eq!(frame.time_base, Rational::new(1, 1000));
            }
            other => panic!("expected Frame, got {other:?}"),
        }

        decoder.send_packet(None).unwrap();
        assert!(matches!(
            decoder.receive_frame().unwrap(),
            DecodeStatus::EndOfStream
        ));
        assert!(decoder.send_packet(Some(&pkt)).is_err());

        decoder.reset().unwrap();
        decoder.send_packet(Some(&pkt)).unwrap();
        assert!(matches!(
            decoder.receive_frame().unwrap(),
            DecodeStatus::Frame(_)
        ));
    }
}
