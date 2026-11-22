//! Media metadata probing backed by `rsmpeg::format::FormatContext`.
//!
//! Always call [`probe_file`] on a background thread — never on the UI thread.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rsmpeg::format::FormatContext;
use rsmpeg::util::MediaType;

/// Simplified media stream category for presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamMediaKind {
    Video,
    Audio,
    Subtitle,
    Data,
    Other,
}

impl From<MediaType> for StreamMediaKind {
    fn from(value: MediaType) -> Self {
        match value {
            MediaType::Video => Self::Video,
            MediaType::Audio => Self::Audio,
            MediaType::Subtitle => Self::Subtitle,
            MediaType::Data => Self::Data,
            _ => Self::Other,
        }
    }
}

/// Parsed details for a single stream inside a media container.
#[derive(Debug, Clone)]
pub struct StreamSummary {
    pub index: usize,
    pub kind: StreamMediaKind,
    pub codec_name: String,
    pub width: Option<usize>,
    pub height: Option<usize>,
    pub fps: Option<f64>,
    pub pixel_format: Option<String>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
    pub sample_format: Option<String>,
    pub bit_rate: Option<u64>,
    pub duration: Duration,
    pub metadata: Vec<(String, String)>,
}

/// Aggregated container + stream metadata produced by `rsmpeg` probe.
#[derive(Debug, Clone)]
pub struct MediaInfo {
    pub file_name: String,
    pub file_path: PathBuf,
    pub file_size_bytes: Option<u64>,
    pub container_format: Option<String>,
    pub duration: Duration,
    pub bit_rate: u64,
    pub streams: Vec<StreamSummary>,
    pub probe_warning: Option<String>,
}

impl MediaInfo {
    pub fn has_video(&self) -> bool {
        self.streams
            .iter()
            .any(|s| s.kind == StreamMediaKind::Video)
    }

    pub fn has_audio(&self) -> bool {
        self.streams
            .iter()
            .any(|s| s.kind == StreamMediaKind::Audio)
    }

    /// True when the probed file clearly has audio streams and no video streams.
    pub fn is_audio_only(&self) -> bool {
        !self.has_video() && self.has_audio()
    }

    pub fn primary_video(&self) -> Option<&StreamSummary> {
        self.streams
            .iter()
            .find(|s| s.kind == StreamMediaKind::Video)
    }

    pub fn primary_audio(&self) -> Option<&StreamSummary> {
        self.streams
            .iter()
            .find(|s| s.kind == StreamMediaKind::Audio)
    }

    /// Supplement duration from the playback engine when container probe lacked one.
    pub fn update_duration_if_missing(&mut self, duration: Duration) {
        if self.duration.is_zero() && !duration.is_zero() {
            self.duration = duration;
        }
    }
}

/// Probe a media file via `rsmpeg::format::FormatContext`.
///
/// If the file exists on disk but uses a container not yet handled by rsmpeg's
/// built-in demuxers (for example MP3/OGG played through Symphonia fallback),
/// returns a partial [`MediaInfo`] with `probe_warning` populated rather than
/// blocking playback.
pub fn probe_file(path: &Path) -> Result<MediaInfo, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("無法讀取檔案：{e}"))?;
    let file_size_bytes = Some(meta.len());
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| path.to_string_lossy().into_owned());

    let mut ctx = FormatContext::open_input(path).map_err(|e| format!("無法開啟容器：{e}"))?;
    let mut probe_warning = None;
    if let Err(e) = ctx.read_header() {
        probe_warning = Some(format!("容器標頭解析受限：{e}"));
    }

    let container_format = ctx.format_name.clone().or_else(|| {
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
    });

    let mut streams = Vec::with_capacity(ctx.streams.len());
    let mut max_stream_duration = Duration::ZERO;

    for stream in &ctx.streams {
        let dur_sec = stream.duration_seconds();
        let duration = if dur_sec.is_finite() && dur_sec > 0.0 {
            Duration::from_secs_f64(dur_sec)
        } else {
            Duration::ZERO
        };
        if duration > max_stream_duration {
            max_stream_duration = duration;
        }

        let fps = [stream.avg_frame_rate, stream.r_frame_rate]
            .into_iter()
            .find(|r| r.num > 0 && r.den > 0)
            .map(|r| r.to_f64())
            .filter(|v| v.is_finite() && *v > 0.0);

        let metadata: Vec<(String, String)> = stream
            .metadata
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();

        streams.push(StreamSummary {
            index: stream.index,
            kind: StreamMediaKind::from(stream.media_type),
            codec_name: stream.codec_id.name().to_string(),
            width: stream.codec_params.width.filter(|&w| w > 0),
            height: stream.codec_params.height.filter(|&h| h > 0),
            fps,
            pixel_format: stream.codec_params.pixel_format.map(|p| format!("{p:?}")),
            sample_rate: stream.codec_params.sample_rate.filter(|&sr| sr > 0),
            channels: stream.codec_params.channels.filter(|&ch| ch > 0),
            sample_format: stream.codec_params.sample_format.map(|f| format!("{f:?}")),
            bit_rate: stream.codec_params.bit_rate.filter(|&b| b > 0),
            duration,
            metadata,
        });
    }

    let duration = if ctx.duration > 0 {
        Duration::from_millis(ctx.duration as u64)
    } else {
        max_stream_duration
    };

    let bit_rate = if ctx.bit_rate > 0 {
        ctx.bit_rate
    } else if !duration.is_zero() {
        file_size_bytes
            .map(|bytes| ((bytes as f64 * 8.0) / duration.as_secs_f64()).round() as u64)
            .unwrap_or(0)
    } else {
        0
    };

    Ok(MediaInfo {
        file_name,
        file_path: path.to_path_buf(),
        file_size_bytes,
        container_format,
        duration,
        bit_rate,
        streams,
        probe_warning,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_test_wav(samples: u32, sample_rate: u32, channels: u16, bits: u16) -> Vec<u8> {
        let block_align = channels * (bits / 8);
        let byte_rate = sample_rate * u32::from(block_align);
        let data_size = samples * u32::from(block_align);
        let riff_size = 36 + data_size;

        let mut buf = Vec::with_capacity(44 + data_size as usize);
        buf.extend_from_slice(b"RIFF");
        buf.extend_from_slice(&riff_size.to_le_bytes());
        buf.extend_from_slice(b"WAVE");
        buf.extend_from_slice(b"fmt ");
        buf.extend_from_slice(&16u32.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes());
        buf.extend_from_slice(&channels.to_le_bytes());
        buf.extend_from_slice(&sample_rate.to_le_bytes());
        buf.extend_from_slice(&byte_rate.to_le_bytes());
        buf.extend_from_slice(&block_align.to_le_bytes());
        buf.extend_from_slice(&bits.to_le_bytes());
        buf.extend_from_slice(b"data");
        buf.extend_from_slice(&data_size.to_le_bytes());
        buf.resize(44 + data_size as usize, 0);
        buf
    }

    #[test]
    fn probe_wav_extracts_container_and_audio_stream() {
        rsmpeg::format::format_registry::register_builtin_formats();
        let path = std::env::temp_dir().join("rsmpeg_gui_probe_test.wav");
        std::fs::write(&path, build_test_wav(48_000, 48_000, 2, 16)).unwrap();

        let info = probe_file(&path).expect("WAV probe succeeds");
        let _ = std::fs::remove_file(&path);

        assert_eq!(info.container_format.as_deref(), Some("wav"));
        assert!(info.is_audio_only());
        assert!(!info.has_video());
        assert_eq!(info.streams.len(), 1);
        assert_eq!(info.duration, Duration::from_secs(1));
        let audio = info.primary_audio().expect("audio stream");
        assert_eq!(audio.sample_rate, Some(48_000));
        assert_eq!(audio.channels, Some(2));
        assert!(info.bit_rate > 0);
    }

    #[test]
    fn probe_missing_file_returns_clear_error() {
        let res = probe_file(Path::new("definitely_nonexistent_rsmpeg_gui_file.mp4"));
        assert!(res.is_err());
    }

    #[test]
    fn probe_unknown_file_degrades_gracefully_with_warning() {
        let path = std::env::temp_dir().join("rsmpeg_gui_corrupt_test.bin");
        std::fs::write(&path, b"not a valid media file").unwrap();

        let info = probe_file(&path).expect("existing unknown file returns partial info");
        let _ = std::fs::remove_file(&path);

        assert!(info.streams.is_empty());
        assert!(info.probe_warning.is_some());
    }
}
