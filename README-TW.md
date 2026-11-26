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
│                                #   Decoder/Encoder trait、CodecRegistry、CodecContext、
│                                #   FlacDecoder、MjpegDecoder、PCMAudio、RawVideo、SRT/WebVTT）
├── rsmpeg-format/               # 容器格式層（IOContext 含 64 KiB 讀取緩衝、Stream、偵測、
│                                #   InputFormat/OutputFormat trait、FormatRegistry、
│                                #   FormatContext 讀寫、7 個 demuxer 與 WAV/Y4M muxer）
├── rsmpeg-filter/               # 濾鏡圖（FilterGraph DAG、Pad、FilterContext、
│                                #   BufferSrc/Sink、內建 Scale/Trim/Null/Overlay 濾鏡）
├── rsmpeg-scale/                # 影片縮放（Scaler、色彩空間轉換、內插方法）
├── rsmpeg-resample/             # 音訊重採樣（Resampler、聲道映射、抖動、格式轉換）
├── rsmpeg-player/               # 統一播放核心（原生 demux/decode +
│                                #   OpenH264/Symphonia 後端、同步時鐘、影音輸出）
├── rsmpeg-gui/                  # GPUI 桌面影音播放器（見 `docs/gui.md`）
└── rsmpeg-cli/                  # 命令列工具（probe、decode、transcode、play、gui）
    └── rsmpeg                  # 執行檔：`rsmpeg probe|decode|transcode|play|gui|list-formats|list-codecs`
```

| Crate | FFmpeg 對應 | 用途 |
|-------|-------------|------|
| `rsmpeg-util` | `libavutil` | 共用型別、錯誤處理、有理數運算、格式列舉 |
| `rsmpeg-codec` | `libavcodec` | 編解碼器識別、封包/幀型別、解碼/編碼 traits、純 Rust 解碼器 |
| `rsmpeg-format` | `libavformat` | 容器格式 I/O、demuxer/muxer 註冊表、格式偵測 |
| `rsmpeg-filter` | `libavfilter` | 濾鏡圖 DAG、來源/接收緩衝區、內建影片濾鏡 |
| `rsmpeg-scale` | `libswscale` | 影片縮放、像素格式轉換、色彩空間數學 |
| `rsmpeg-resample` | `libswresample` | 音訊重採樣、聲道混音、抖動處理 |
| `rsmpeg-player` | — | 統一播放控制平面、時鐘同步、原生與後端管線 |
| `rsmpeg-gui` | — | 基於 GPUI 的 GPU 加速桌面播放器 |

## 快速開始

### 前置需求

- [Rust](https://rustup.rs/) 1.75 或更新版本（`rsmpeg-gui` 因 `gpui 0.2.2` 需 Rust 1.88+）

### 建置

```bash
git clone https://github.com/stevenke1981/rsmpeg.git
cd rsmpeg
cargo build --workspace
```

#### 純 Rust / Feature-Gated 建置模式（`rsmpeg-player` 與 `rsmpeg-cli`）

`rsmpeg-player` 與 `rsmpeg-cli` 已將外部/C 後端以 Cargo feature 隔離，可獨立建置與測試純 Rust 管線：

```bash
# 僅啟用純 Rust 解碼器與原生 demuxer/muxer（不含 OpenH264、Symphonia、Rodio）
cargo test -p rsmpeg-player -p rsmpeg-cli --no-default-features

# 純 Rust 播放（含 Symphonia 音訊解碼與 Rodio 輸出，不含 C/C++ OpenH264）
cargo build -p rsmpeg-cli --no-default-features --features pure-rust

# 僅使用 rsmpeg 原生 demuxer/decoder 搭配 Rodio 音訊輸出
cargo build -p rsmpeg-cli --no-default-features --features native-only
```

### 執行測試

```bash
cargo test --workspace
cargo test -p rsmpeg-player -p rsmpeg-cli --no-default-features
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

# 使用純 Rust 解碼器解碼 FLAC 或 WAV 音訊並輸出 16-bit PCM WAV
cargo run --bin rsmpeg -- decode audio.flac --output decoded.wav

# 解碼 MJPEG / RawVideo 並將第一幀匯出為二進位 PPM
cargo run --bin rsmpeg -- decode mjpeg_clip.mp4 --output first_frame.ppm --max-frames 30

# 純 Rust 音訊轉碼至 WAV（FLAC/WAV -> 重採樣/混音 PCM WAV）
cargo run --bin rsmpeg -- transcode input.flac output.wav --acodec pcm_s16le --sample-rate 48000 --channels 2
cargo run --bin rsmpeg -- transcode input.wav output_mono.wav --sample-rate 48000 --channels 1

# 純 Rust 視訊轉碼至 Y4M（MJPEG AVI/MP4 或 Y4M -> 縮放轉換原始 YUV4MPEG2）
cargo run --bin rsmpeg -- transcode mjpeg_clip.avi output.y4m --vcodec rawvideo --width 640 --height 360 --max-frames 60
cargo run --bin rsmpeg -- transcode input.y4m scaled.y4m --width 1280 --height 720

# 解析並檢視 SRT / WebVTT 字幕內容
cargo run --bin rsmpeg -- decode subtitles.srt
cargo run --bin rsmpeg -- decode captions.vtt

# 播放媒體檔案或啟動 GPUI 桌面播放器（詳見 docs/gui.md）
cargo run --bin rsmpeg -- play example.mp4
cargo run -p rsmpeg-gui -- example.mp4
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
- **Codec**：`CodecId` 列舉（27+ 格式）、`Packet`/`Frame` 型別、`Codec` + `Decoder`/`Encoder` traits、`CodecRegistry` 全域單例、Builder 模式 `CodecContext`，以及純 Rust 編解碼器：
  - `FlacDecoder`（`FlacCodec`）：RFC 9639 純 Rust 無損 FLAC 音訊解碼器，採用 64 位元 `BitReader`（CRC-8/CRC-16 驗證、CONSTANT/VERBATIM/FIXED/LPC 子幀、立體聲去相關與單封包多幀解碼）
  - `MjpegDecoder`（`MjpegCodec`）：ITU-T T.81 Baseline DCT Motion-JPEG 視訊解碼器，採用整數 IDCT（`SOF0`、Annex K.3.3 預設 Huffman 表、8/16 位元 DQT、灰階與 YCbCr 取樣、重啟標記，輸出 `Rgb24`）
  - `PCMAudioDecoder` / `PCMAudioEncoder`（`PCMAudioCodec`）：無壓縮 PCM（`U8`、`S16`、`S32`、`F32`、`F64`）
  - `RawVideoDecoder` / `RawVideoEncoder`（`RawVideoCodec`）：無壓縮視訊幀直通，支援 planar/semi-planar/packed 佈局拆解
  - `SrtCodec`、`WebVttCodec`、`SubtitleDecoder`、`SubtitleTrack`、`SubtitleCue`，以及 `parse_srt` / `parse_webvtt` / `parse_subtitle_file`
- **Format**：`IOContext` 抽象層（檔案含 64 KiB 讀取緩衝 / 記憶體緩衝）、魔術位元組格式偵測、`InputFormat`/`OutputFormat` traits、`FormatRegistry`、`FormatContext` 讀寫介面（`open_input`、`open_output`、`open_output_buffer`）、**7 個已註冊 demuxer**（`mp4`、`mkv`、`avi`、`flac`、`wav`、`rawvideo`、`y4m`）與 **2 個已註冊 muxer**（`wav`、`y4m`）：
  - 有狀態 `FLACDemuxer`：`read_header`（STREAMINFO/SEEKTABLE/VORBIS_COMMENT）、串流式 `read_frame`（含幀同步與 CRC-16 邊界掃描），以及 `seek`（透過 SEEKTABLE 二分搜尋與位元組偏移估算重新同步）
  - `MP4Demuxer`：除 H.264/AAC 外，支援 MJPEG 視訊樣本項目（`jpeg`、`mjpa`、`mjpb`、`mjpg` 與 `mp4v` `esds` `objectTypeIndication = 0x6C`）與 FLAC 音訊樣本項目（`fLaC` + `dfLa` STREAMINFO extradata）
  - `Y4mDemuxer` / `Y4mMuxer` 與 `WAVMuxer`：純 Rust YUV4MPEG2（`C420`/`C422`/`C444`/`Cmono`）與 RIFF/WAVE PCM（`U8`/`S16`/`S32`/`F32`）封裝與解封裝（含 seek 支援）
- **Filter**：Filter trait、FilterGraph DAG、Pad/PadDirection、BufferSrc/BufferSink、內建濾鏡（Scale、Trim、Null、Overlay、Transpose，以及支援 padded `linesize` 與 RGBA 快速路徑的 `BoxBlurFilter`、`CropFilter`、`GrayscaleFilter`、`MirrorFilter`、`RotateFilter`）
- **Scale**：Scaler 含 ScalerConfig builder、4 種實作內插核（`nearest`、`bilinear`、`bicubic`、`lanczos`）、色彩空間定義（BT.601/BT.709/BT.2020/RGB，limited/full range），以及 `Yuv420P`/`Yuv422P`/`Yuv444P`/`Nv12`/`Nv21` → packed RGB/BGR/RGBA 1:1 快速路徑
- **Resample**：Resampler 含 ResamplerConfig、支援 `U8`/`S16`/`S32`/`F32`/`F64` 與 planar（`S16P`/`S32P`/`F32P`/`F64P`）輸入、加權 `ChannelMapping` 矩陣混音（含 5.1 → stereo 降混）與抖動方法
- **Player 與 GUI**：`rsmpeg-player` 統一背景播放引擎（feature-gated `backend-symphonia`、`backend-openh264`、`audio-rodio`、`pure-rust`、`native-only`；原生 `rsmpeg-format` + 純 Rust `FlacDecoder`/`MjpegDecoder`/`RawVideoDecoder`/`PCMAudioDecoder` 管線、OpenH264 與 Symphonia 後端回退、`DecoderInfo` 管線回報與單幀步進）及 `rsmpeg-gui` GPUI 桌面播放器（詳見 [docs/gui.md](docs/gui.md)）
- **CLI**：`probe`（支援 JSON/verbose 輸出）、`decode`（純 Rust 解碼 FLAC/MJPEG/PCM/RawVideo/SRT/WebVTT，支援 `.wav`/`.ppm` 匯出與 `--max-frames`）、`transcode`（純 Rust `.flac`/`.wav` → `.wav` 音訊重採樣/混音與 `.avi`/`.mp4`/`.y4m` → `.y4m` 視訊縮放/格式轉換）、`play`、`gui`、`list-formats`（同時列出 demuxer 與 muxer）、`list-codecs`

### 🚧 進行中

- 完整解碼 → 縮放 → 編碼轉碼管線
- 透過 GPU API 的硬體加速編解碼支援
- 串流協定支援（RTMP、HLS、SRT）

### ❌ 明確排除範圍

- 專利保護的編解碼演算法（從零實作 H.264/H.265/AAC 解碼）
- Unsafe 程式碼或 C 函式庫 FFI 綁定

## 專案狀態

本專案處於**積極開發中**。核心架構已穩定，純 Rust 解碼器與解封裝器已同時驅動 CLI 工具與 `rsmpeg-player` / `rsmpeg-gui` 原生播放管線；完整逐幀轉碼工作流持續擴充中。

所有 workspace crate 均具備完整的單元與回歸測試。

## 設計原則

1. **零 `unsafe`** — 所有 crate 使用 `#![forbid(unsafe_code)]`
2. **Trait 多型** — 編解碼器、demuxer、濾鏡均透過 Rust trait 實作，確保可擴充性
3. **註冊表模式** — 透過 `OnceLock<RwLock<...>>` 全域註冊表實現執行時期編解碼器/格式發現
4. **無 C 相依** — 全純 Rust，無 FFI 或 bindgen
5. **模組化架構** — 每個子系統為獨立 crate，最小化跨相依性

## 授權條款

MIT
