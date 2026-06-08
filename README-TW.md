# rsmpeg

**純 Rust 多媒體框架 — 完整 FFmpeg 等效實作，全安全 Rust 程式碼**

rsmpeg 是一個模組化的純 Rust 多媒體處理函式庫，其架構深受 FFmpeg 啟發。它提供完整的媒體處理工具，涵蓋讀取、檢測、轉換及寫入影音內容 — 全部以安全 Rust 撰寫（`#![forbid(unsafe_code)]`）。

## 架構

rsmpeg 鏡像 FFmpeg 的元件模型，每個子系統由獨立的 crate 負責：

```
rsmpeg/                          # Facade crate（統一公開 API + re-export）
├── rsmpeg-util/                 # 基礎工具（錯誤類型、有理數、媒體類型、
│                                #   像素/取樣格式、聲道佈局）
├── rsmpeg-codec/                # 編解碼層（CodecId、Packet、Frame、Codec trait、
│                                #   Decoder/Encoder trait、CodecRegistry、CodecContext）
├── rsmpeg-format/               # 容器格式層（IOContext、Stream、偵測、
│                                #   InputFormat/OutputFormat trait、FormatRegistry、
│                                #   FormatContext、內建 MP4/MKV/AVI/FLAC/WAV demuxer）
├── rsmpeg-filter/               # 濾鏡圖（FilterGraph DAG、Pad、FilterContext、
│                                #   BufferSrc/Sink、內建 Scale/Trim/Null/Overlay 濾鏡）
├── rsmpeg-scale/                # 影片縮放（Scaler、色彩空間轉換、內插方法）
├── rsmpeg-resample/             # 音訊重採樣（Resampler、聲道映射、抖動、格式轉換）
└── rsmpeg-cli/                  # 命令列工具（probe、transcode、play）
    └── rsmpeg                  # 執行檔：`rsmpeg probe|transcode|play|list-formats|list-codecs`
```

| Crate | FFmpeg 對應 | 用途 |
|-------|-------------|------|
| `rsmpeg-util` | `libavutil` | 共用型別、錯誤處理、有理數運算、格式列舉 |
| `rsmpeg-codec` | `libavcodec` | 編解碼器識別、封包/幀型別、解碼/編碼 traits |
| `rsmpeg-format` | `libavformat` | 容器格式 I/O、demuxer/muxer 註冊表、格式偵測 |
| `rsmpeg-filter` | `libavfilter` | 濾鏡圖 DAG、來源/接收緩衝區、內建影片濾鏡 |
| `rsmpeg-scale` | `libswscale` | 影片縮放、像素格式轉換、色彩空間數學 |
| `rsmpeg-resample` | `libswresample` | 音訊重採樣、聲道混音、抖動處理 |

## 快速開始

### 前置需求

- [Rust](https://rustup.rs/) 1.70 或更新版本

### 建置

```bash
git clone https://github.com/stevenke1981/rsmpeg.git
cd rsmpeg
cargo build --workspace
```

### 執行測試

```bash
cargo test --workspace
```

### CLI 使用

```bash
# 顯示說明
cargo run --bin rsmpeg -- --help

# 列出已註冊的格式 demuxer
cargo run --bin rsmpeg -- list-formats

# 列出已註冊的編解碼器
cargo run --bin rsmpeg -- list-codecs

# 探測媒體檔案（基本）
cargo run --bin rsmpeg -- probe example.wav

# 詳細串流資訊
cargo run --bin rsmpeg -- probe example.mp4 --verbose

# JSON 輸出（適合程式處理）
cargo run --bin rsmpeg -- probe example.mkv --json
```

### 範例

```bash
# 執行 pipeline 範例（展示所有層）
cargo run --example pipeline

# 基本檔案探測
cargo run --example probe_basic -- example.wav

# 濾鏡圖建構
cargo run --example filter_graph

# 版本資訊
cargo run --example version
```

## 功能狀態

### ✅ 已完成

- **Util**：錯誤型別、有理數運算、媒體類型偵測、像素/取樣格式列舉、聲道佈局 bitflags、字典
- **Codec**：CodecId 列舉（22+ 格式）、Packet/Frame 型別、Codec trait + Decoder/Encoder traits、CodecRegistry 全域單例、Builder 模式 CodecContext、內建 RawVideo 與 PCM 音訊編解碼器
- **Format**：IOContext 抽象層（File/Buffer）、魔術位元組格式偵測、InputFormat/OutputFormat traits、FormatRegistry、FormatContext header 解析、**5 個真實 demuxer**（MP4、MKV、AVI、FLAC、WAV）
- **Filter**：Filter trait、FilterGraph DAG、Pad/PadDirection、BufferSrc/BufferSink、內建濾鏡（Scale、Trim、Null、Overlay、Transpose）
- **Scale**：Scaler 含 ScalerConfig builder、7 種內插方法、色彩空間定義（BT.601/709/2020/RGB）
- **Resample**：Resampler 含 ResamplerConfig、聲道映射矩陣、抖動方法
- **CLI**：probe（支援 JSON/verbose 輸出）、transcode（骨架）、play（骨架）、list-formats、list-codecs

### 🚧 進行中

- 完整解碼 → 縮放 → 編碼 pipeline
- 透過 GPU API 的硬體加速編解碼支援
- 串流協定支援（RTMP、HLS、SRT）
- 透過影音裝置輸出的即時播放

### ❌ 明確排除範圍

- 專利保護的編解碼演算法（從零實作 H.264/H.265/AAC 解碼）
- Unsafe 程式碼或 C 函式庫 FFI 綁定

## 專案狀態

本專案處於**積極開發中**。核心架構已穩定，基礎元件層已可使用。更高階的功能如完整逐幀轉碼與即時播放正在開發中。

目前測試覆蓋率：**45+ 項測試**涵蓋所有 crate，全部通過。

## 設計原則

1. **零 `unsafe`** — 所有 crate 使用 `#![forbid(unsafe_code)]`
2. **Trait 多型** — 編解碼器、demuxer、濾鏡均透過 Rust trait 實作，確保可擴充性
3. **註冊表模式** — 透過 `OnceLock<RwLock<...>>` 全域註冊表實現執行時期編解碼器/格式發現
4. **無 C 相依** — 全純 Rust，無 FFI 或 bindgen
5. **模組化架構** — 每個子系統為獨立 crate，最小化跨相依性

## 授權條款

MIT
