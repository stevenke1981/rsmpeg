//! Pure formatting helpers for timecodes, bitrates, audio parameters, and file sizes.

use std::time::Duration;

use crate::i18n::Locale;

/// Format a [`Duration`] as `MM:SS` (or `HH:MM:SS` when >= 1 hour).
pub fn format_duration(duration: Duration) -> String {
    let total_secs = duration.as_secs();
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;
    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

/// Format seconds safely (clamps NaN, infinity, and negatives to `00:00`).
pub fn format_duration_secs(secs: f64) -> String {
    if !secs.is_finite() || secs <= 0.0 {
        return "00:00".to_string();
    }
    format_duration(Duration::from_secs_f64(secs))
}

/// Format a bitrate in bits per second into a human-readable string.
pub fn format_bitrate(bps: u64) -> String {
    if bps == 0 {
        return "-".to_string();
    }
    if bps >= 1_000_000 {
        format!("{:.2} Mbps", bps as f64 / 1_000_000.0)
    } else if bps >= 1_000 {
        format!("{:.0} kbps", bps as f64 / 1_000.0)
    } else {
        format!("{bps} bps")
    }
}

/// Format an audio sample rate in Hz.
pub fn format_sample_rate(hz: u32) -> String {
    if hz == 0 {
        return "-".to_string();
    }
    if hz >= 1000 && hz.is_multiple_of(100) {
        format!("{:.1} kHz", f64::from(hz) / 1000.0)
    } else {
        format!("{hz} Hz")
    }
}

/// Format channel count in Traditional Chinese.
pub fn format_channels(channels: u16) -> String {
    format_channels_for(Locale::ZhTw, channels)
}

/// Format channel count localized for the active [`Locale`].
pub fn format_channels_for(locale: Locale, channels: u16) -> String {
    match locale {
        Locale::ZhTw => match channels {
            0 => "-".to_string(),
            1 => "單聲道 (1)".to_string(),
            2 => "立體聲 (2)".to_string(),
            6 => "5.1 環繞 (6)".to_string(),
            8 => "7.1 環繞 (8)".to_string(),
            n => format!("{n} 聲道"),
        },
        Locale::EnUs => match channels {
            0 => "-".to_string(),
            1 => "Mono (1)".to_string(),
            2 => "Stereo (2)".to_string(),
            6 => "5.1 Surround (6)".to_string(),
            8 => "7.1 Surround (8)".to_string(),
            n => format!("{n} ch"),
        },
    }
}

/// Format file size in bytes into B / KB / MB / GB.
pub fn format_file_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.2} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

/// Format playback speed multiplier (e.g. `1.0x`, `1.25x`).
pub fn format_rate(rate: f64) -> String {
    let rounded = (rate * 100.0).round() / 100.0;
    if ((rounded * 10.0).round() - rounded * 10.0).abs() < 1e-6 {
        format!("{rounded:.1}x")
    } else {
        format!("{rounded:.2}x")
    }
}

/// Format a volume value (`0.0..=1.0`) as a whole-number percentage.
pub fn format_volume_percent(volume: f32) -> String {
    let pct = (volume.clamp(0.0, 1.0) * 100.0).round() as u32;
    format!("{pct}%")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_duration_handles_seconds_minutes_hours() {
        assert_eq!(format_duration(Duration::ZERO), "00:00");
        assert_eq!(format_duration(Duration::from_secs(9)), "00:09");
        assert_eq!(format_duration(Duration::from_secs(65)), "01:05");
        assert_eq!(format_duration(Duration::from_secs(3599)), "59:59");
        assert_eq!(format_duration(Duration::from_secs(3600)), "01:00:00");
        assert_eq!(format_duration(Duration::from_secs(3661)), "01:01:01");
    }

    #[test]
    fn format_duration_secs_clamps_invalid_values() {
        assert_eq!(format_duration_secs(-10.0), "00:00");
        assert_eq!(format_duration_secs(f64::NAN), "00:00");
        assert_eq!(format_duration_secs(f64::INFINITY), "00:00");
        assert_eq!(format_duration_secs(125.9), "02:05");
    }

    #[test]
    fn format_bitrate_scales_units() {
        assert_eq!(format_bitrate(0), "-");
        assert_eq!(format_bitrate(512), "512 bps");
        assert_eq!(format_bitrate(192_000), "192 kbps");
        assert_eq!(format_bitrate(2_500_000), "2.50 Mbps");
    }

    #[test]
    fn format_sample_rate_formats_khz_and_hz() {
        assert_eq!(format_sample_rate(0), "-");
        assert_eq!(format_sample_rate(44_100), "44.1 kHz");
        assert_eq!(format_sample_rate(48_000), "48.0 kHz");
        assert_eq!(format_sample_rate(11_025), "11025 Hz");
    }

    #[test]
    fn format_channels_and_size_and_rate() {
        assert_eq!(format_channels(1), "單聲道 (1)");
        assert_eq!(format_channels(2), "立體聲 (2)");
        assert_eq!(format_channels(6), "5.1 環繞 (6)");
        assert_eq!(format_channels(4), "4 聲道");
        assert_eq!(format_channels_for(Locale::EnUs, 1), "Mono (1)");
        assert_eq!(format_channels_for(Locale::EnUs, 2), "Stereo (2)");
        assert_eq!(format_channels_for(Locale::EnUs, 6), "5.1 Surround (6)");
        assert_eq!(format_channels_for(Locale::EnUs, 4), "4 ch");

        assert_eq!(format_file_size(512), "512 B");
        assert_eq!(format_file_size(2048), "2.0 KB");
        assert_eq!(format_file_size(5 * 1024 * 1024), "5.00 MB");

        assert_eq!(format_rate(1.0), "1.0x");
        assert_eq!(format_rate(0.75), "0.75x");
        assert_eq!(format_rate(1.25), "1.25x");
        assert_eq!(format_rate(2.0), "2.0x");
        assert_eq!(format_volume_percent(0.804), "80%");
    }
}
