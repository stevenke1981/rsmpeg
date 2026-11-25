# rsmpeg 重構驗收摘要（第八刀 — multi-agent round 4）

## 2026-07-23 — AcmeUI Native GUI 與實機播放

- `eframe/egui` 已從 `rsmpeg-cli` 與 lockfile 移除，GUI 改為 AcmeUI Native
  `Application`、`WidgetNode`/Taffy、One Dark semantic theme 與 wgpu renderer。
- AcmeUI Native 新增 backend-neutral RGBA8 image primitive、contain fit、revision cache、
  painter-order、clip、texture-limit 與 GPU recovery CPU backing。
- GUI 保留 `rsmpeg-player` 背景 demux/decode 邊界，8 ms 輪詢 latest frame；支援開檔、
  拖放、播放/暫停、停止、75 ms seek preview、音量、Tab/Enter/方向鍵與 Replay。
- 播放結束保留最後畫面；視窗支援 320x220 起的 compact responsive geometry。
- 實際以 4 秒 640x360 H.264 baseline + AAC MP4 驗證，release binary 可完整播放並顯示
  最後幀。產物：`target/release/rsmpeg.exe`。

AcmeUI Native 已固定到公開 revision `f34a5a6`，乾淨 clone 與 CI 不再依賴本機 E 槽。
已知限制：Acme real-GPU recovery ignored smoke 在本機曾以 access violation 結束，
不能宣稱已通過。

## 2026-07-23 — 專案檢視與控制面改善

- `git pull --ff-only`：本機 `master` 已是最新。
- 修正 GUI stroke 寬度的明確 `f32` 型別，恢復新版 Rust 的 `-D warnings` gate。
- `Player::send_command` 現在區分 command queue 滿載與 playback worker 斷線，
  避免把 worker 結束誤診為背壓。
- 新增 worker 斷線回歸測試；既有 queue-full 測試持續通過。

驗收：`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --all-features -- -D warnings`
與 `cargo test --workspace --all-targets` 皆通過；`scripts/build-release.ps1 -CliOnly`
成功產出 `target/release/rsmpeg.exe`。

## 2026-07-12 — GUI timeline preview seek

- 根因：拖曳期間只更新 GUI 本地時間，放開時才 seek；seek 後重設視訊 PTS 基準，首張
  preview frame 被標記為 0:00，且舊 generation 事件可覆寫最新畫面。
- 修正：GUI 每 75 ms 發出節流 preview seek、放開時送出最終 seek；保留拖曳 target；
  player 丟棄舊 generation 的媒體事件；native/fallback 保留全域 PTS 並允許 forced preview。
- CI clippy gate 現以 `--all-features` 執行，和本機 release 驗收一致。

## 2026-07-12 — P0 playback hardening follow-up

- Audio-only position 改由 `AudioPlaybackClock` 的單調時間軸估算，不再使用已排入 rodio
  的樣本總數；Pause、Resume、Seek 與變速均維持連續語意。
- Native 與 fallback video pacing wait 不再消耗/遺失 Pause、Seek 或 SetVolume 命令。
- 移除尚未有安全後端實作的選音/選視訊 track 命令；新增受限於 0.25–4.0 的變速 API。
- CI 的 clippy 改為 required，並加入 `--no-default-features` workspace check。

驗收：`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --all-features -- -D warnings`、
`cargo check --workspace --no-default-features`、`cargo test --workspace` 皆通過；
`scripts/build-release.ps1 -RunTests` 成功產出 `target/release/rsmpeg.exe`。

分支：`feat/native-playback-pipeline`

## 本輪完成

### E — PcmRingBuffer（Phase 5.3）
- 新增 `rsmpeg-player/src/audio_ring_buffer.rs`：`PcmRingBuffer`
  - 固定容量（樣本數）、`push`/`consume`/`clear`/`len`/`is_full`/`capacity`/`stats`
  - overflow / underflow 統計
  - 5 個單元測試（容量內 push / overflow / underflow / clear / is_full）
- `demux_worker` 音訊節流改以 **樣本數估算**（200 ms 目標）+ 保留 rodio `sink.len()` backstop，永不 stall
- Seek 時清空 ring（防止 paused+force_one_frame 卡死）

### F — ScalerCache（Phase 6.1 效能）
- 新增 `rsmpeg-player/src/scaler_cache.rs`：thread-local `HashMap<(w,h), Scaler>` 重用
- `video_convert` 新增 `yuv420p_frame_to_rgba_cached`
- `native_pipeline` 主路徑與 flush 路徑皆改用 cached（輸出 byte-identical，不再每 frame new Scaler）

### G — drop_before_seek（Phase 8.3）
- `VideoScheduler::drop_before_seek(frame_pts, target) -> bool`（`frame_pts < target`）
- native + fallback 在 Seek 後丟棄 PTS < target 的視訊 frame（首幀 >= target 才顯示）
- `lib.rs` 註冊 `audio_ring_buffer` / `scaler_cache` 兩個新模組

## 驗收

```text
cargo test --workspace          # PASS
cargo build --release -p rsmpeg-cli -p rsmpeg-player  # PASS
cargo fmt --all -- --check      # PASS
```

| crate | tests |
|-------|-------|
| rsmpeg-player | **60**（含 5 ring / 2 scheduler / 1 cached / 1 scaler 新測試） |
| rsmpeg-codec | 27 |
| rsmpeg-scale | 8 |
| rsmpeg-util | 12 |
| rsmpeg-resample | 11 |
| rsmpeg-format | 10 |
| rsmpeg-filter | 4 |

## 里程碑狀態（更新）
- M4 Resample：linear SRC ✅
- M4 Scale：ScalerCache 重用 ✅
- M5 Scheduler：VideoScheduler + drop_before_seek ✅
- M5 AudioClock：MasterClock audio-only ✅
- Phase 5.3 PCM ring buffer：核心完成（overflow/underflow/seek-clear）
- Phase 8.3 seek 丟棄 pre-target frame：native + fallback ✅

## 已知限制
- ring buffer 播放樣本估算為 wall-clock 近似（rodio backstop 兜底，無 regression）
- 低/高水位、silence-on-underflow、長時間穩定測試尚未做
- B-frame timestamp reorder 仍依賴 decoder 內部佇列
- Clippy 仍為 soft，有預存 style warning
