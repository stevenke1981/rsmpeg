//! Centralized UI strings for `rsmpeg-gui`.
//!
//! All user-visible copy lives here so adding another language (e.g. `en-US`)
//! only requires adding a new `UiStrings` constant without touching view code.

/// Supported UI locales.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Locale {
    #[default]
    ZhTw,
}

/// Complete table of UI labels, tooltips, and status messages.
#[derive(Debug, Clone, Copy)]
pub struct UiStrings {
    pub app_title: &'static str,
    pub app_subtitle: &'static str,

    // Welcome & file opening
    pub welcome_title: &'static str,
    pub welcome_hint: &'static str,
    pub open_file_btn: &'static str,
    pub open_files_dialog_title: &'static str,
    pub media_filter_label: &'static str,
    pub drop_overlay_title: &'static str,
    pub drop_overlay_hint: &'static str,

    // Playback controls & tooltips
    pub play: &'static str,
    pub pause: &'static str,
    pub stop: &'static str,
    pub prev_track: &'static str,
    pub next_track: &'static str,
    pub mute: &'static str,
    pub unmute: &'static str,
    pub volume: &'static str,
    pub speed: &'static str,
    pub fullscreen_enter: &'static str,
    pub fullscreen_exit: &'static str,
    pub track_video_btn: &'static str,
    pub track_audio_btn: &'static str,
    pub track_menu_video_title: &'static str,
    pub track_menu_audio_title: &'static str,
    pub track_none: &'static str,
    pub track_unsupported: &'static str,
    pub info_panel_btn: &'static str,
    pub playlist_panel_btn: &'static str,
    pub open_short_btn: &'static str,

    // Playback states
    pub status_idle: &'static str,
    pub status_opening: &'static str,
    pub status_playing: &'static str,
    pub status_paused: &'static str,
    pub status_seeking: &'static str,
    pub status_stopped: &'static str,
    pub status_ended: &'static str,
    pub status_error: &'static str,

    // Audio-only stage
    pub audio_only_badge: &'static str,
    pub loading_video: &'static str,
    pub no_file_loaded: &'static str,

    // Media info panel
    pub info_panel_title: &'static str,
    pub info_empty: &'static str,
    pub info_probing: &'static str,
    pub info_section_container: &'static str,
    pub info_section_streams: &'static str,
    pub info_file_name: &'static str,
    pub info_container: &'static str,
    pub info_file_size: &'static str,
    pub info_duration: &'static str,
    pub info_bitrate: &'static str,
    pub info_stream_count: &'static str,
    pub info_codec: &'static str,
    pub info_resolution: &'static str,
    pub info_frame_rate: &'static str,
    pub info_pixel_format: &'static str,
    pub info_sample_rate: &'static str,
    pub info_channels: &'static str,
    pub info_sample_format: &'static str,
    pub info_unknown: &'static str,
    pub stream_video: &'static str,
    pub stream_audio: &'static str,
    pub stream_subtitle: &'static str,
    pub stream_data: &'static str,
    pub stream_other: &'static str,

    // Playlist panel
    pub playlist_title: &'static str,
    pub playlist_empty: &'static str,
    pub playlist_add: &'static str,
    pub playlist_clear: &'static str,
    pub playlist_remove: &'static str,
    pub playlist_now_playing: &'static str,

    // Errors & banners
    pub error_title: &'static str,
    pub warning_title: &'static str,
    pub dismiss: &'static str,
    pub error_open_failed: &'static str,
    pub error_unsupported_or_corrupt: &'static str,

    // Shortcuts legend
    pub shortcuts_hint: &'static str,
}

/// Traditional Chinese (Taiwan) localization table.
pub const ZH_TW: UiStrings = UiStrings {
    app_title: "rsmpeg 播放器",
    app_subtitle: "純 Rust 多媒體播放與檢視工具",

    welcome_title: "開始播放影音檔案",
    welcome_hint: "拖放影片或音訊檔案到視窗中，或點擊下方按鈕開啟（快捷鍵 O）",
    open_file_btn: "開啟媒體檔案",
    open_files_dialog_title: "選擇媒體檔案",
    media_filter_label: "影音檔案",
    drop_overlay_title: "放開以加入並播放",
    drop_overlay_hint: "支援 MP4、MKV、AVI、MOV、WAV、FLAC、MP3、OGG、M4A",

    play: "播放",
    pause: "暫停",
    stop: "停止",
    prev_track: "上一首",
    next_track: "下一首",
    mute: "靜音",
    unmute: "取消靜音",
    volume: "音量",
    speed: "速度",
    fullscreen_enter: "全螢幕",
    fullscreen_exit: "退出全螢幕",
    track_video_btn: "視訊軌",
    track_audio_btn: "音軌",
    track_menu_video_title: "選擇視訊軌",
    track_menu_audio_title: "選擇音軌",
    track_none: "無可用軌道",
    track_unsupported: "不支援",
    info_panel_btn: "媒體資訊",
    playlist_panel_btn: "播放清單",
    open_short_btn: "開檔",

    status_idle: "待命",
    status_opening: "載入中…",
    status_playing: "播放中",
    status_paused: "已暫停",
    status_seeking: "定位中…",
    status_stopped: "已停止",
    status_ended: "播放完畢",
    status_error: "錯誤",

    audio_only_badge: "純音訊",
    loading_video: "正在解碼畫面…",
    no_file_loaded: "尚未載入檔案",

    info_panel_title: "媒體資訊",
    info_empty: "開啟檔案後將在此顯示容器與串流詳細資訊。",
    info_probing: "正在分析媒體封裝…",
    info_section_container: "容器概覽",
    info_section_streams: "串流明細",
    info_file_name: "檔名",
    info_container: "容器格式",
    info_file_size: "檔案大小",
    info_duration: "總時長",
    info_bitrate: "總位元率",
    info_stream_count: "串流數",
    info_codec: "編解碼器",
    info_resolution: "解析度",
    info_frame_rate: "幀率",
    info_pixel_format: "像素格式",
    info_sample_rate: "取樣率",
    info_channels: "聲道",
    info_sample_format: "取樣格式",
    info_unknown: "未知",
    stream_video: "視訊",
    stream_audio: "音訊",
    stream_subtitle: "字幕",
    stream_data: "資料",
    stream_other: "其他",

    playlist_title: "播放清單",
    playlist_empty: "播放清單目前為空，可開檔或直接拖放多個檔案加入。",
    playlist_add: "加入檔案",
    playlist_clear: "清空",
    playlist_remove: "移除",
    playlist_now_playing: "播放中",

    error_title: "無法播放此檔案",
    warning_title: "播放提醒",
    dismiss: "關閉",
    error_open_failed: "開啟檔案失敗",
    error_unsupported_or_corrupt: "格式不受支援或檔案已損毀",

    shortcuts_hint: "空白鍵 播放/暫停 · ←/→ 5秒 · ↑/↓ 音量 · F 全螢幕 · M 靜音 · O 開檔",
};

/// Retrieve the active locale's string table.
pub const fn strings(locale: Locale) -> &'static UiStrings {
    match locale {
        Locale::ZhTw => &ZH_TW,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zh_tw_strings_are_non_empty() {
        let s = strings(Locale::ZhTw);
        assert!(!s.app_title.is_empty());
        assert!(!s.open_file_btn.is_empty());
        assert!(!s.info_panel_title.is_empty());
        assert!(!s.playlist_title.is_empty());
        assert!(!s.error_title.is_empty());
        assert!(!s.shortcuts_hint.is_empty());
    }
}
