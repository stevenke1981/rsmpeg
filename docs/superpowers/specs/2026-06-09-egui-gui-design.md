# rsmpeg egui GUI — 設計規格

## 摘要
為 rsmpeg-cli 新增 `rsmpeg gui [file]` 子命令，用 egui/eframe 取代 minifb 作為圖形播放器，提供 VLC 風格的影音播放體驗。

## 架構

```
┌──────────────────────────────────────────────────────────┐
│                     eframe 主視窗                          │
│  ┌────────────────────────────────────────────────────┐  │
│  │                   egui UI 層                        │  │
│  │  ┌─────────┐  ┌──────────┐  ┌──────┐  ┌────────┐ │  │
│  │  │影片畫面  │  │控制列    │  │音量  │  │時間    │ │  │
│  │  │(Texture)│  │▶⏸  Seek │  │🔊══ │  │01:23   │ │  │
│  │  └─────────┘  └──────────┘  └──────┘  └────────┘ │  │
│  └────────────────────────────────────────────────────┘  │
│                           ▲                               │
│                   最新 RGB frame                           │
│                           │                               │
│  ┌────────────────────────────────────────────────────┐  │
│  │              Playback Engine (背景執行緒)             │  │
│  │  Symphonia → OpenH264 → RGB → mpsc::Sender        │  │
│  │                   → rodio 音訊輸出                   │  │
│  │  PlaybackState: Arc<Mutex<{playing, position}>>    │  │
│  └────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────┘
```

## 模組職責

### `gui/mod.rs`
- `MediaApp` — 實作 `eframe::App`
- `run_gui(path: Option<&str>)` — 公開入口，啟動 eframe

### `gui/state.rs`
- `PlaybackState` — playing/paused, current_position, duration, status
- `FrameData` — RGBA buffer, width, height, pts

### `gui/engine.rs`
- `PlaybackEngine` — 背景執行緒管理
- `new(path)` → 啟動 thread 跑 demux + decode + rodio
- 透過 `mpsc::Sender<FrameData>` 送 frame 給 UI
- PlaybackState 透過 `Arc<Mutex<>>` 共享

### `gui/ui.rs`
- 影片顯示區：`egui::Image` + TextureHandle
- 底部控制列：Play/Pause、Seek slider（唯讀位置）、音量 Slider、時間顯示、開啟檔案
- 無檔案時顯示拖曳/點擊開啟提示

## 資料流
1. Background thread: `Symphonia::next_packet()` → 影片 → OpenH264 → RGBA → channel
2. Audio samples 直接餵 rodio（只送 frame 給 UI）
3. egui `update()` 每幀 try_recv() 最新 frame，更新 texture
4. `ctx.request_repaint()` 保持連續更新
5. Play/Pause 透過 `Arc<Mutex<PlaybackState>>.playing` 控制

## 依賴
- `eframe = "0.28"`（含 egui 0.28）
- 保留 `minifb`（給 `rsmpeg play` 指令使用）
- 保留既有 symphonia/openh264/rodio

## 控制項（MVP）
- ⏸/▶ Play/Pause 切換
- ⏹ Stop（回到起點）
- 📂 開啟檔案按鈕（原生對話框）
- 時間文字：`00:00 / 03:45`
- 🔊 音量滑桿
- Seek 進度條（唯讀，顯示位置）

## 非 MVP（延後）
- 互動式 Seek（點擊進度條跳轉）
- 全螢幕
- 播放清單
- 拖曳載入檔案
- 快捷鍵
- 字幕
