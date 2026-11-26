//! Centralized UI strings for `rsmpeg-gui`.
//!
//! All user-visible copy lives here with both Traditional Chinese (`zh-TW`)
//! and English (`en-US`) tables so switching locales updates the entire UI
//! without touching view layout code.

/// Supported UI locales.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Locale {
    #[default]
    ZhTw,
    EnUs,
}

impl Locale {
    /// Toggle between Traditional Chinese and English.
    pub const fn toggle(self) -> Self {
        match self {
            Self::ZhTw => Self::EnUs,
            Self::EnUs => Self::ZhTw,
        }
    }

    /// Compact badge text displayed on the top-bar language toggle button.
    pub const fn short_label(self) -> &'static str {
        match self {
            Self::ZhTw => "繁中",
            Self::EnUs => "EN",
        }
    }
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

    // Repeat, shuffle, A-B loop, frame step
    pub repeat_off: &'static str,
    pub repeat_all: &'static str,
    pub repeat_one: &'static str,
    pub shuffle_on: &'static str,
    pub shuffle_off: &'static str,
    pub ab_loop_btn: &'static str,
    pub ab_loop_set_a: &'static str,
    pub ab_loop_set_b: &'static str,
    pub ab_loop_clear: &'static str,
    pub ab_clear_btn: &'static str,
    pub step_frame_btn: &'static str,
    pub step_frame_osd: &'static str,

    // Tools popover, aspect ratio, video filters, screenshot
    pub tools_btn: &'static str,
    pub tools_menu_title: &'static str,
    pub aspect_mode: &'static str,
    pub aspect_contain: &'static str,
    pub aspect_cover: &'static str,
    pub aspect_fill: &'static str,
    pub video_section: &'static str,
    pub rotate_btn: &'static str,
    pub mirror_btn: &'static str,
    pub grayscale_btn: &'static str,
    pub brightness_label: &'static str,
    pub contrast_label: &'static str,
    pub reset_filters_btn: &'static str,
    pub screenshot_btn: &'static str,
    pub screenshot_saved: &'static str,
    pub screenshot_failed: &'static str,

    // Subtitles
    pub subtitle_section: &'static str,
    pub subtitle_load_btn: &'static str,
    pub subtitle_dialog_title: &'static str,
    pub subtitle_filter_label: &'static str,
    pub subtitle_toggle_btn: &'static str,
    pub subtitle_on: &'static str,
    pub subtitle_off: &'static str,
    pub subtitle_offset_label: &'static str,
    pub subtitle_size_label: &'static str,
    pub subtitle_size_small: &'static str,
    pub subtitle_size_medium: &'static str,
    pub subtitle_size_large: &'static str,
    pub subtitle_none: &'static str,
    pub subtitle_loaded: &'static str,
    pub subtitle_failed: &'static str,

    // Playback & playlist helpers
    pub playback_section: &'static str,
    pub speed_reset_btn: &'static str,
    pub playlist_add_folder: &'static str,
    pub folder_dialog_title: &'static str,
    pub move_up: &'static str,
    pub move_down: &'static str,

    // Media info decoder summary
    pub info_section_decoder: &'static str,
    pub info_demuxer: &'static str,
    pub info_video_decoder: &'static str,
    pub info_audio_decoder: &'static str,
    pub info_decoder_none: &'static str,

    // Time mode, volume OSD, shortcuts modal
    pub remaining_time_tooltip: &'static str,
    pub volume_osd: &'static str,
    pub help_btn: &'static str,
    pub shortcuts_title: &'static str,
    pub shortcuts_rows: &'static [(&'static str, &'static str)],
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

    shortcuts_hint: "空白鍵 播放/暫停 · ←/→ 5秒 · ↑/↓ 音量 · F 全螢幕 · M 靜音 · O 開檔 · ? 快捷鍵",

    repeat_off: "循環：關",
    repeat_all: "循環：全部",
    repeat_one: "循環：單曲",
    shuffle_on: "隨機：開",
    shuffle_off: "隨機：關",
    ab_loop_btn: "A-B",
    ab_loop_set_a: "已設定 A 點",
    ab_loop_set_b: "A-B 循環已啟用",
    ab_loop_clear: "已清除 A-B 循環",
    ab_clear_btn: "清除 A-B",
    step_frame_btn: "逐幀",
    step_frame_osd: "單幀前進",

    tools_btn: "工具",
    tools_menu_title: "播放與畫面工具",
    aspect_mode: "畫面比例",
    aspect_contain: "完整適應",
    aspect_cover: "填滿裁切",
    aspect_fill: "拉伸填滿",
    video_section: "視訊調整",
    rotate_btn: "旋轉 90°",
    mirror_btn: "水平鏡像",
    grayscale_btn: "灰階",
    brightness_label: "亮度",
    contrast_label: "對比",
    reset_filters_btn: "重設畫面",
    screenshot_btn: "截圖",
    screenshot_saved: "已儲存截圖",
    screenshot_failed: "截圖失敗",

    subtitle_section: "字幕",
    subtitle_load_btn: "載入字幕",
    subtitle_dialog_title: "選擇字幕檔案",
    subtitle_filter_label: "字幕檔案",
    subtitle_toggle_btn: "字幕顯示",
    subtitle_on: "開",
    subtitle_off: "關",
    subtitle_offset_label: "字幕延遲",
    subtitle_size_label: "字幕大小",
    subtitle_size_small: "小",
    subtitle_size_medium: "中",
    subtitle_size_large: "大",
    subtitle_none: "尚未載入字幕",
    subtitle_loaded: "已載入字幕",
    subtitle_failed: "字幕載入失敗",

    playback_section: "播放控制",
    speed_reset_btn: "重設 1.0x",
    playlist_add_folder: "資料夾",
    folder_dialog_title: "選擇媒體資料夾",
    move_up: "上移",
    move_down: "下移",

    info_section_decoder: "解碼引擎",
    info_demuxer: "解封裝器",
    info_video_decoder: "視訊解碼",
    info_audio_decoder: "音訊解碼",
    info_decoder_none: "無",

    remaining_time_tooltip: "點擊切換剩餘 / 總時長",
    volume_osd: "音量",
    help_btn: "快捷鍵",
    shortcuts_title: "鍵盤與滑鼠快捷鍵",
    shortcuts_rows: &[
        ("Space", "播放 / 暫停"),
        ("← / →", "快退 / 快進 5 秒"),
        ("Shift + ← / →", "快退 / 快進 30 秒"),
        ("Ctrl + ← / →", "精準微調 ±1 秒"),
        ("0 – 9", "跳轉至 0% – 90% 進度"),
        (".", "單幀前進"),
        ("↑ / ↓ / 滾輪", "調整音量 ±5%"),
        ("M", "靜音 / 取消靜音"),
        ("[ / ] / Backspace", "降速 / 加速 / 重設 1.0x"),
        ("B", "A-B 循環（設 A / 設 B / 清除）"),
        ("R / H", "切換循環模式 / 隨機播放"),
        ("D / Shift + R", "切換畫面比例 / 順時針旋轉 90°"),
        ("Shift + S", "儲存目前畫面為 PNG 截圖"),
        ("C / Shift + C", "顯示或隱藏字幕 / 載入字幕檔"),
        ("V / A / T", "視訊軌 / 音軌 / 工具選單"),
        ("I / L", "媒體資訊 / 播放清單面板"),
        ("F / 雙擊畫面", "全螢幕切換"),
        ("O / Ctrl + O", "開啟媒體檔案"),
        ("G / ? / F1", "切換語言 / 快捷鍵說明"),
        ("S / Esc", "停止播放 / 關閉彈窗或退出全螢幕"),
    ],
};

/// English (United States) localization table.
pub const EN_US: UiStrings = UiStrings {
    app_title: "rsmpeg Player",
    app_subtitle: "Pure Rust Multimedia Player & Inspector",

    welcome_title: "Open a Media File",
    welcome_hint: "Drag & drop video or audio files here, or click below to browse (Key: O)",
    open_file_btn: "Open Media File",
    open_files_dialog_title: "Select Media Files",
    media_filter_label: "Media Files",
    drop_overlay_title: "Drop to Add & Play",
    drop_overlay_hint: "Supports MP4, MKV, AVI, MOV, WAV, FLAC, MP3, OGG, M4A",

    play: "Play",
    pause: "Pause",
    stop: "Stop",
    prev_track: "Prev",
    next_track: "Next",
    mute: "Mute",
    unmute: "Unmute",
    volume: "Volume",
    speed: "Speed",
    fullscreen_enter: "Fullscreen",
    fullscreen_exit: "Exit Fullscreen",
    track_video_btn: "Video",
    track_audio_btn: "Audio",
    track_menu_video_title: "Select Video Track",
    track_menu_audio_title: "Select Audio Track",
    track_none: "No tracks available",
    track_unsupported: "Unsupported",
    info_panel_btn: "Media Info",
    playlist_panel_btn: "Playlist",
    open_short_btn: "Open",

    status_idle: "Idle",
    status_opening: "Opening…",
    status_playing: "Playing",
    status_paused: "Paused",
    status_seeking: "Seeking…",
    status_stopped: "Stopped",
    status_ended: "Ended",
    status_error: "Error",

    audio_only_badge: "Audio Only",
    loading_video: "Decoding video…",
    no_file_loaded: "No file loaded",

    info_panel_title: "Media Info",
    info_empty: "Open a media file to inspect container and stream details.",
    info_probing: "Probing container…",
    info_section_container: "Container Overview",
    info_section_streams: "Streams",
    info_file_name: "File Name",
    info_container: "Container",
    info_file_size: "File Size",
    info_duration: "Duration",
    info_bitrate: "Bitrate",
    info_stream_count: "Streams",
    info_codec: "Codec",
    info_resolution: "Resolution",
    info_frame_rate: "Frame Rate",
    info_pixel_format: "Pixel Format",
    info_sample_rate: "Sample Rate",
    info_channels: "Channels",
    info_sample_format: "Sample Format",
    info_unknown: "Unknown",
    stream_video: "Video",
    stream_audio: "Audio",
    stream_subtitle: "Subtitle",
    stream_data: "Data",
    stream_other: "Other",

    playlist_title: "Playlist",
    playlist_empty: "Playlist is empty. Open or drop multiple files to queue them.",
    playlist_add: "Add Files",
    playlist_clear: "Clear",
    playlist_remove: "Remove",
    playlist_now_playing: "Now Playing",

    error_title: "Unable to Play File",
    warning_title: "Playback Notice",
    dismiss: "Dismiss",
    error_open_failed: "Failed to open file",
    error_unsupported_or_corrupt: "Unsupported format or corrupted file",

    shortcuts_hint:
        "Space Play/Pause · ←/→ ±5s · ↑/↓ Volume · F Fullscreen · M Mute · O Open · ? Help",

    repeat_off: "Repeat: Off",
    repeat_all: "Repeat: All",
    repeat_one: "Repeat: One",
    shuffle_on: "Shuffle: On",
    shuffle_off: "Shuffle: Off",
    ab_loop_btn: "A-B",
    ab_loop_set_a: "Loop A set",
    ab_loop_set_b: "A-B loop active",
    ab_loop_clear: "A-B loop cleared",
    ab_clear_btn: "Clear A-B",
    step_frame_btn: "Step",
    step_frame_osd: "Frame step",

    tools_btn: "Tools",
    tools_menu_title: "Playback & Video Tools",
    aspect_mode: "Aspect",
    aspect_contain: "Contain",
    aspect_cover: "Cover",
    aspect_fill: "Stretch",
    video_section: "Video",
    rotate_btn: "Rotate 90°",
    mirror_btn: "Mirror H",
    grayscale_btn: "Grayscale",
    brightness_label: "Brightness",
    contrast_label: "Contrast",
    reset_filters_btn: "Reset Video",
    screenshot_btn: "Snapshot",
    screenshot_saved: "Screenshot saved",
    screenshot_failed: "Screenshot failed",

    subtitle_section: "Subtitles",
    subtitle_load_btn: "Load Subtitle",
    subtitle_dialog_title: "Select Subtitle File",
    subtitle_filter_label: "Subtitle Files",
    subtitle_toggle_btn: "Subtitles",
    subtitle_on: "On",
    subtitle_off: "Off",
    subtitle_offset_label: "Offset",
    subtitle_size_label: "Size",
    subtitle_size_small: "Small",
    subtitle_size_medium: "Medium",
    subtitle_size_large: "Large",
    subtitle_none: "No subtitle loaded",
    subtitle_loaded: "Subtitle loaded",
    subtitle_failed: "Subtitle failed",

    playback_section: "Playback",
    speed_reset_btn: "Reset 1.0x",
    playlist_add_folder: "Folder",
    folder_dialog_title: "Select Media Folder",
    move_up: "Move Up",
    move_down: "Move Down",

    info_section_decoder: "Active Decoder Engine",
    info_demuxer: "Demuxer",
    info_video_decoder: "Video Decoder",
    info_audio_decoder: "Audio Decoder",
    info_decoder_none: "None",

    remaining_time_tooltip: "Click to toggle remaining / total time",
    volume_osd: "Volume",
    help_btn: "Shortcuts",
    shortcuts_title: "Keyboard & Mouse Shortcuts",
    shortcuts_rows: &[
        ("Space", "Play / Pause"),
        ("← / →", "Seek ±5s"),
        ("Shift + ← / →", "Seek ±30s"),
        ("Ctrl + ← / →", "Fine Seek ±1s"),
        ("0 – 9", "Jump to 0% – 90%"),
        (".", "Step Forward 1 Frame"),
        ("↑ / ↓ / Wheel", "Volume ±5%"),
        ("M", "Mute / Unmute"),
        ("[ / ] / Backspace", "Speed - / + / Reset 1.0x"),
        ("B", "A-B Loop (Set A / Set B / Clear)"),
        ("R / H", "Cycle Repeat / Toggle Shuffle"),
        ("D / Shift + R", "Aspect Mode / Rotate 90°"),
        ("Shift + S", "Save PNG Screenshot"),
        ("C / Shift + C", "Toggle Subtitles / Load Subtitle"),
        ("V / A / T", "Video / Audio Tracks / Tools"),
        ("I / L", "Media Info / Playlist Panel"),
        ("F / Double-Click", "Toggle Fullscreen"),
        ("O / Ctrl + O", "Open Media File"),
        ("G / ? / F1", "Toggle Language / Shortcuts Help"),
        ("S / Esc", "Stop / Close Overlay or Exit Fullscreen"),
    ],
};

/// Retrieve the active locale's string table.
pub const fn strings(locale: Locale) -> &'static UiStrings {
    match locale {
        Locale::ZhTw => &ZH_TW,
        Locale::EnUs => &EN_US,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_locales_have_non_empty_strings_and_toggle_cleanly() {
        for locale in [Locale::ZhTw, Locale::EnUs] {
            let s = strings(locale);
            assert!(!s.app_title.is_empty());
            assert!(!s.open_file_btn.is_empty());
            assert!(!s.track_video_btn.is_empty());
            assert!(!s.track_audio_btn.is_empty());
            assert!(!s.info_panel_title.is_empty());
            assert!(!s.playlist_title.is_empty());
            assert!(!s.error_title.is_empty());
            assert!(!s.shortcuts_hint.is_empty());

            assert!(!s.repeat_off.is_empty());
            assert!(!s.repeat_all.is_empty());
            assert!(!s.repeat_one.is_empty());
            assert!(!s.shuffle_on.is_empty());
            assert!(!s.shuffle_off.is_empty());
            assert!(!s.ab_loop_btn.is_empty());
            assert!(!s.ab_loop_set_a.is_empty());
            assert!(!s.ab_loop_set_b.is_empty());
            assert!(!s.ab_loop_clear.is_empty());
            assert!(!s.ab_clear_btn.is_empty());
            assert!(!s.step_frame_btn.is_empty());
            assert!(!s.step_frame_osd.is_empty());
            assert!(!s.tools_btn.is_empty());
            assert!(!s.tools_menu_title.is_empty());
            assert!(!s.aspect_mode.is_empty());
            assert!(!s.aspect_contain.is_empty());
            assert!(!s.aspect_cover.is_empty());
            assert!(!s.aspect_fill.is_empty());
            assert!(!s.video_section.is_empty());
            assert!(!s.rotate_btn.is_empty());
            assert!(!s.mirror_btn.is_empty());
            assert!(!s.grayscale_btn.is_empty());
            assert!(!s.brightness_label.is_empty());
            assert!(!s.contrast_label.is_empty());
            assert!(!s.reset_filters_btn.is_empty());
            assert!(!s.screenshot_btn.is_empty());
            assert!(!s.screenshot_saved.is_empty());
            assert!(!s.screenshot_failed.is_empty());
            assert!(!s.subtitle_section.is_empty());
            assert!(!s.subtitle_load_btn.is_empty());
            assert!(!s.subtitle_dialog_title.is_empty());
            assert!(!s.subtitle_filter_label.is_empty());
            assert!(!s.subtitle_toggle_btn.is_empty());
            assert!(!s.subtitle_on.is_empty());
            assert!(!s.subtitle_off.is_empty());
            assert!(!s.subtitle_offset_label.is_empty());
            assert!(!s.subtitle_size_label.is_empty());
            assert!(!s.subtitle_size_small.is_empty());
            assert!(!s.subtitle_size_medium.is_empty());
            assert!(!s.subtitle_size_large.is_empty());
            assert!(!s.subtitle_none.is_empty());
            assert!(!s.subtitle_loaded.is_empty());
            assert!(!s.subtitle_failed.is_empty());
            assert!(!s.playback_section.is_empty());
            assert!(!s.speed_reset_btn.is_empty());
            assert!(!s.playlist_add_folder.is_empty());
            assert!(!s.folder_dialog_title.is_empty());
            assert!(!s.move_up.is_empty());
            assert!(!s.move_down.is_empty());
            assert!(!s.info_section_decoder.is_empty());
            assert!(!s.info_demuxer.is_empty());
            assert!(!s.info_video_decoder.is_empty());
            assert!(!s.info_audio_decoder.is_empty());
            assert!(!s.info_decoder_none.is_empty());
            assert!(!s.remaining_time_tooltip.is_empty());
            assert!(!s.volume_osd.is_empty());
            assert!(!s.help_btn.is_empty());
            assert!(!s.shortcuts_title.is_empty());
            assert!(!s.shortcuts_rows.is_empty());
            for (key, desc) in s.shortcuts_rows {
                assert!(!key.is_empty());
                assert!(!desc.is_empty());
            }
        }

        assert_ne!(
            strings(Locale::ZhTw).app_title,
            strings(Locale::EnUs).app_title
        );
        assert_eq!(
            strings(Locale::ZhTw).shortcuts_rows.len(),
            strings(Locale::EnUs).shortcuts_rows.len()
        );
        assert_eq!(Locale::ZhTw.toggle(), Locale::EnUs);
        assert_eq!(Locale::EnUs.toggle(), Locale::ZhTw);
        assert_eq!(Locale::ZhTw.short_label(), "繁中");
        assert_eq!(Locale::EnUs.short_label(), "EN");
    }
}
