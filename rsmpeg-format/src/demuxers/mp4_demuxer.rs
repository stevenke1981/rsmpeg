use crate::format::InputFormat;
use crate::format_context::FormatContext;
use crate::probe::ProbeScore;
use crate::stream::Stream;
use rsmpeg_codec::{CodecId, Packet};
use rsmpeg_util::{MediaType, RsError, RsResult};
use std::io::SeekFrom;

/// MP4/ISOBMFF demuxer.
///
/// Parses the `ftyp` box, then scans for the `moov` box to discover
/// tracks via `trak` → `mdia` → `hdlr` (media type).
///
/// This is a simplified implementation — full chunk offset parsing
/// (stbl/stco/stsz) is not yet implemented.
pub struct MP4Demuxer;

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
        let io = ctx
            .io
            .as_mut()
            .ok_or_else(|| RsError::InvalidData("No IO context".into()))?;
        io.seek(SeekFrom::Start(0))?;

        // Scan top-level boxes until we find moov
        let mut track_index = 0usize;
        let found_moov = read_and_find_moov(ctx, &mut track_index)?;

        if !found_moov {
            tracing::warn!("MP4: no moov box found (fragmented or streaming file?)");
            if ctx.streams.is_empty() {
                let stream = Stream::new(0, CodecId::Unknown);
                ctx.streams.push(stream);
            }
        } else {
            tracing::info!("MP4: found {} stream(s)", ctx.streams.len());
        }

        Ok(())
    }

    fn read_frame(&mut self, _ctx: &mut FormatContext) -> RsResult<Option<Packet>> {
        Ok(None)
    }

    fn seek(&mut self, _ctx: &mut FormatContext, _ts: i64) -> RsResult<()> {
        Ok(())
    }
}

/// Scan through top-level ISOBMFF boxes until we find and parse `moov`.
/// Returns `true` if `moov` was found.
///
/// This helper avoids holding a long-lived borrow on `ctx.io` so that
/// nested parsing functions can access it without causing E0499.
fn read_and_find_moov(ctx: &mut FormatContext, track_index: &mut usize) -> RsResult<bool> {
    loop {
        // Read box header in a small scope so the io borrow is dropped
        // before any call that mutates `ctx`.
        let (box_size, box_type) = {
            let io = match ctx.io.as_mut() {
                Some(io) => io,
                None => return Ok(false),
            };
            let size = match io.read_u32_be() {
                Ok(s) => s,
                Err(_) => return Ok(false),
            };
            let mut btype = [0u8; 4];
            if io.read_exact(&mut btype).is_err() {
                return Ok(false);
            }
            (size, btype)
        };

        if box_size < 8 {
            return Ok(false);
        }
        let remaining = (box_size as u64) - 8;

        match &box_type {
            b"moov" => {
                parse_moov_box(ctx, track_index)?;
                return Ok(true);
            }
            _ => {
                if remaining > 0 {
                    let io = match ctx.io.as_mut() {
                        Some(io) => io,
                        None => return Ok(false),
                    };
                    if io.seek(SeekFrom::Current(remaining as i64)).is_err() {
                        return Ok(false);
                    }
                }
            }
        }

        // Safety: if we consumed nothing, stop to avoid infinite loop
        if remaining == 0 {
            return Ok(false);
        }
    }
}

/// Parse a `moov` box for track information.
fn parse_moov_box(ctx: &mut FormatContext, track_index: &mut usize) -> RsResult<()> {
    loop {
        let (sub_size, sub_type) = {
            let io = match ctx.io.as_mut() {
                Some(io) => io,
                None => return Ok(()),
            };
            let size = match io.read_u32_be() {
                Ok(s) => s,
                Err(_) => return Ok(()),
            };
            let mut stype = [0u8; 4];
            if io.read_exact(&mut stype).is_err() {
                return Ok(());
            }
            (size, stype)
        };

        if sub_size < 8 {
            return Ok(());
        }
        let sub_remaining = (sub_size as u64) - 8;

        match &sub_type {
            b"trak" => {
                parse_trak_box(ctx, *track_index)?;
                *track_index += 1;
                // The trak parser consumes the entire sub-box, so no need to skip
            }
            _ => {
                if sub_remaining > 0 {
                    let io = match ctx.io.as_mut() {
                        Some(io) => io,
                        None => return Ok(()),
                    };
                    if io.seek(SeekFrom::Current(sub_remaining as i64)).is_err() {
                        return Ok(());
                    }
                }
            }
        }
    }
}

/// Parse a `trak` box to find the `mdia` box.
fn parse_trak_box(ctx: &mut FormatContext, track_index: usize) -> RsResult<()> {
    loop {
        let (sub_size, sub_type) = {
            let io = match ctx.io.as_mut() {
                Some(io) => io,
                None => return Ok(()),
            };
            let size = match io.read_u32_be() {
                Ok(s) => s,
                Err(_) => return Ok(()),
            };
            let mut stype = [0u8; 4];
            if io.read_exact(&mut stype).is_err() {
                return Ok(());
            }
            (size, stype)
        };

        if sub_size < 8 {
            return Ok(());
        }
        let sub_remaining = (sub_size as u64) - 8;

        if &sub_type == b"mdia" {
            parse_mdia_box(ctx, track_index)?;
            return Ok(());
        }

        if sub_remaining > 0 {
            let io = match ctx.io.as_mut() {
                Some(io) => io,
                None => return Ok(()),
            };
            if io.seek(SeekFrom::Current(sub_remaining as i64)).is_err() {
                return Ok(());
            }
        }
    }
}

/// Parse a `mdia` box to find the `hdlr` handler reference.
fn parse_mdia_box(ctx: &mut FormatContext, track_index: usize) -> RsResult<()> {
    loop {
        let (sub_size, sub_type) = {
            let io = match ctx.io.as_mut() {
                Some(io) => io,
                None => return Ok(()),
            };
            let size = match io.read_u32_be() {
                Ok(s) => s,
                Err(_) => return Ok(()),
            };
            let mut stype = [0u8; 4];
            if io.read_exact(&mut stype).is_err() {
                return Ok(());
            }
            (size, stype)
        };

        if sub_size < 8 {
            return Ok(());
        }
        let sub_remaining = (sub_size as u64) - 8;

        match &sub_type {
            b"hdlr" => {
                let (media_type, _handler_type) = {
                    let io = match ctx.io.as_mut() {
                        Some(io) => io,
                        None => return Ok(()),
                    };
                    let _predefined = io.read_u32_be()?;
                    let handler_type = io.read_bytes(4)?;
                    let _reserved = io.read_bytes(12)?;

                    let mt = match &handler_type[..] {
                        b"vide" => MediaType::Video,
                        b"soun" => MediaType::Audio,
                        b"subt" => MediaType::Subtitle,
                        _ => MediaType::Data,
                    };
                    (mt, handler_type)
                };

                let mut stream = Stream::new(track_index, CodecId::Unknown);
                stream.media_type = media_type;
                ctx.streams.push(stream);
            }
            b"minf" => {
                if sub_remaining > 0 {
                    let io = match ctx.io.as_mut() {
                        Some(io) => io,
                        None => return Ok(()),
                    };
                    if io.seek(SeekFrom::Current(sub_remaining as i64)).is_err() {
                        return Ok(());
                    }
                }
            }
            _ => {
                if sub_remaining > 0 {
                    let io = match ctx.io.as_mut() {
                        Some(io) => io,
                        None => return Ok(()),
                    };
                    if io.seek(SeekFrom::Current(sub_remaining as i64)).is_err() {
                        return Ok(());
                    }
                }
            }
        }
    }
}
