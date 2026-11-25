# rsmpeg 測試紀錄

## 2026-07-23 — AcmeUI Native 播放器遷移

| 指令／驗證 | 結果 |
|---|---|
| `cargo test --workspace --all-targets` | PASS（GUI 10 tests；5 個外部 H.264 fixture 測試為 ignored） |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | PASS |
| `cargo test -p acme-platform` | PASS（15 tests，1 doctest ignored） |
| `cargo build --release -p rsmpeg-cli` | PASS |
| `rsmpeg.exe gui rsmpeg-acmeui-test.mp4` | PASS；實際完成 4 秒 H.264/AAC 播放並顯示最後 RGBA frame |

Visual QA 使用 320x220、400x280、720x500 與 960x600 邏輯尺寸檢查高 DPI 行為。
影片、時間軸與音量軌可見；窄視窗採固定 responsive geometry，避免 Taffy intrinsic
高度把控制區推離 viewport。Computer Use 的 Windows.Graphics.Capture 在本機回報
`SetIsBorderRequired ... 0x80004002`，因此改用專案外的 OS screenshot helper 留存畫面。

## 2026-07-23 — Rust 相容性與播放器控制面診斷

| 指令 | 結果 |
|---|---|
| `cargo fmt --all -- --check` | PASS |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | PASS |
| `cargo test --workspace --all-targets` | PASS（5 個外部 H.264 fixture 測試為 ignored） |
| `scripts/build-release.ps1 -CliOnly` | PASS，產出 `target/release/rsmpeg.exe` |

修正新版 Rust 對 GUI `f32` literal fallback 的警告，並新增 command channel 斷線回歸測試。
`Player::send_command` 現在會分別回報 queue full 與 worker disconnected。

## 2026-07-12 — GUI timeline seek preview

| 指令 | 結果 |
|---|---|
| `cargo test -p rsmpeg-cli --bin rsmpeg` | PASS（timeline preview throttle） |
| `cargo test -p rsmpeg-player --lib` | PASS（92 tests，含 stale generation 過濾） |
| `cargo test --workspace` | PASS（5 個本機外部 H.264 fixture 測試為 ignored） |
| `cargo check --workspace --no-default-features` | PASS |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | PASS |

GUI 拖曳時間軸現在每 75 ms 送出 preview seek，放開時提交最終位置；舊 seek 事件不會覆寫
當前 scrub target，native/fallback seek preview 保留全域 PTS，不再回到 0:00。

## 2026-07-12 — P0 playback control and release gate

| 指令 | 結果 |
|---|---|
| `cargo fmt --all -- --check` | PASS |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | PASS |
| `cargo check --workspace --no-default-features` | PASS |
| `cargo test --workspace` | PASS（本機未追蹤的 5 個需要 `target/debug/123.mp4` 的 OpenH264 手動媒體測試為 ignored） |
| `scripts/build-release.ps1 -RunTests` | PASS，產出 `target/release/rsmpeg.exe` |

本輪新增 `AudioPlaybackClock`，避免把送入 rodio 的佇列樣本誤當成已播放；pacing wait
不再取走後丟棄控制命令；變速 API 支援 0.25–4.0，並同步音訊輸出與視訊排程。CI 現在把
clippy 視為 required gate，且包含 minimal-feature 編譯檢查。

分支：`feat/native-playback-pipeline`  
日期：2026-07-11（第十六刀 multi-agent round 12）

## 指令

```text
cargo fmt --all
cargo test --workspace
cargo build --release -p rsmpeg-cli -p rsmpeg-player
```

## 結果

| 項目 | 結果 |
|------|------|
| workspace tests | **PASS** |
| rsmpeg-player | **85**（+5：sync 接入 4 / pool 接入 1） |
| rsmpeg-codec | 29 |
| rsmpeg-scale | 18（+2：rgb24→rgba） |
| rsmpeg-util | 16（+4：pixel_format helpers） |
| rsmpeg-resample | 24（+4：gain helpers） |
| rsmpeg-format | 18（+4：round 12 duration；time_util +4 已含） |
| rsmpeg-filter | 20（+3：blur） |
| release build | **PASS** |
| fmt --check | **PASS** |

## round 4 重點
- PcmRingBuffer 元件 + demux_worker 樣本數節流（rodio backstop 保留）
- ScalerCache 按解析度重用 Scaler（native + flush 路徑）
- VideoScheduler::drop_before_seek + native/fallback seek 丟棄 pre-target frame

## round 5 重點
- demux_worker：`abs_pos` 改為以解碼幀 PTS 為準（VFR 正確），無 PTS 才退回固定 1/30
- demux_worker：Seek 綁定 `mode`，僅 `SeekMode::Precise` 啟用 drop_before_seek 丟幀
- OpenH264Decoder：`take_display_order` 按 PTS 升冪出幀（B-frame 顯示序），缺 PTS 退回 FIFO
- FramePool 獨立元件（Mutex+VecDeque 緩衝池，max_bytes 預算）+ 4 項單測

## round 6 重點
- OpenH264Decoder：`reset()` 先清 pending/pts_queue/eof/sps_pps_sent 再重建 decoder；單包解碼失敗改 `warn!`+跳過（不再殺播放）+ 3 單測
- SymphoniaAudioDecoder：`reset()` 顯式清 pending/eof；新增 `map_sample_format`（Symphonia→rsmpeg_util）+ 3 單測（含 reset 清幀）
- MasterClock：新增 `pause()/resume()/is_paused()/seek_to()`，pause 凍結位置（wall + audio-master 雙路徑）+ 4 單測

## round 7 重點
- SyncController（player/src/sync.rs）：A/V drift 決策 Render/Drop/Duplicate（預設 40ms 容差）+ 4 單測
- rsmpeg-scale：新增 `yuv420p_frame_to_bgr24`（BGR 序、3 bytes/pixel，複用 BT.601 數學）+ 2 單測
- rsmpeg-filter：新增 `GrayscaleFilter`（RGBA→灰階，保留 alpha，符合 Filter trait）+ 3 單測

## round 8 重點
- rsmpeg-util：PixelFormat 新增 `is_yuv()`/`is_rgb()`/`channels()`（`planes()` 已存在）+ 4 單測
- rsmpeg-codec：`new_audio` 已存在，補 `new_audio_sets_fields` 單測（+1）
- rsmpeg-resample：新增 `channel.rs`（stereo↔mono f32/i16mix 4 助手）+ 9 單測

## round 9 重點
- rsmpeg-filter：新增 `MirrorFilter`（RGBA 水平翻轉，保留 alpha）+ 3 單測
- rsmpeg-scale：新增 `yuv420p_frame_to_rgb24`（R,G,B 序、3 bytes/pixel，BT.601）+ 2 單測
- rsmpeg-format：新增 `time_util.rs`（`samples_to_ms`/`ms_to_samples`/`samples_to_secs`，timescale 零安全）+ 4 單測

## round 10 重點
- rsmpeg-filter：新增 `CropFilter`（RGBA 子矩形裁切，越界自動 clamp）+ 3 單測
- rsmpeg-scale：新增 `nv12_frame_to_rgba`（semi-planar NV12→RGBA，BT.601）+ 2 單測
- rsmpeg-codec：新增 `CodecParameters::for_video`/`for_audio` 建構子（原僅有 `new`）+ 1 單測

## Clippy 全清 + 主迴圈接入 + round 11
- **Clippy 全清**：`cargo clippy --workspace --all-targets --all-features -- -D warnings` 通過（0 warning，~30 項修正，含 rational should_implement_trait 用 allow、resampler needless_range_loop 用 allow、crop/mp4_demuxer 等 idiomatic 修正；未改 public API）
- **SyncController 接入**（`demux_worker.rs`）：`WorkerSync` + `sync_decision()` 輔助；依 A/V drift 執行 Drop（丟幀）/ Duplicate（重送上一幀）/ Render，預設啟用 + 4 單測
- **FramePool 接入**（`native_pipeline.rs`）：`OnceLock` 64MiB pool 作為 YUV→RGBA 暫存緩衝，事件內容 byte-identical，暫存 recycle 重用 + 1 單測
- rsmpeg-filter：新增 `RotateFilter`（RGBA 90° 順時針旋轉）+ 4 單測
- rsmpeg-scale：新增 `yuv422p_frame_to_rgba`（4:2:2→RGBA，BT.601）+ 2 單測
- rsmpeg-resample：新增 `apply_gain_f32`/`apply_gain_i16`（音量增益 + clamp）+ 4 單測

## round 12 重點
- rsmpeg-filter：新增 `BoxBlurFilter`（RGBA 3×3 盒狀模糊，alpha 不模糊）+ 3 單測
- rsmpeg-scale：新增 `rgb24_frame_to_rgba`（packed RGB24→RGBA，A=255）+ 2 單測
- rsmpeg-format：新增 `duration.rs`（`samples_to_duration`/`duration_to_samples`，timescale 零安全）+ 4 單測

## 已知限制
- ring 播放估算為近似；低/高水位、silence-on-underflow 未做
- VFR 仍依賴解碼幀 PTS，若上游未帶 PTS 始終退回固定幀率
- demux_worker 的 audio position 用 `audio_play_start.elapsed()` 近似（非 MasterClock），僅作 >40ms 漂移二級校正
- `map_sample_format` 為未來多格式輸出的 building block，尚未接入解碼路徑
- Clippy 已全清（`-D warnings` 通過）
