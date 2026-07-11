# rsmpeg 重構驗收摘要（第七刀 — multi-agent round 3）

分支：`feat/native-playback-pipeline`

## 本輪完成

### A — rsmpeg-resample 真正實作（取代 stub）
- `Resampler::resample` 現為 **linear interpolation** SRC
  - S16 ↔ F32 互通
  - 取樣率轉換（長度由 `estimate_output_samples` 決定）
  - 聲道 remap（經 `channel_mapping`；相同 layout 為 identity，相異則取 dominant 係數）
  - 短 plane / 不支援格式回傳明確錯誤（不再靜音）
- 移除 `channel_mapping` 未使用警告（dead_code 消失）
- `audio_convert::frame_to_s16_device` 的 resample 路徑現在產生 **非靜音** 真實 PCM

### B — MasterClock 接 native audio-only position
- native path 在無視訊時，以累積已送出的 sample 數驅動 `MasterClock`
- `position` 改由 `MasterClock::now()` 取得（視訊路徑不變）

### C — demux_worker fallback 現代化
- 視訊 pacing 改由 `VideoScheduler`（Wait/Display/DropLate），移除手寫 `LATE_DROP_SEC` 邏輯
- Seek 時 `video_scheduler.reset_stats()`
- 音訊改用 `frame_to_s16_device`（移除本地 `s16_plane_to_i16` 重複實作）

### D — CI 擴充 macOS
- 新增 `test-macos` job（macos-latest, stable, `cargo test --workspace`）

## 驗收

```text
cargo test --workspace          # PASS
cargo build --release -p rsmpeg-cli -p rsmpeg-player  # PASS
cargo fmt --all -- --check      # PASS（已格式化）
```

| crate | tests |
|-------|-------|
| rsmpeg-player | 50 |
| rsmpeg-codec | 27 |
| rsmpeg-scale | 8 |
| rsmpeg-util | 12 |
| rsmpeg-resample | 11（含 4 個新 resampler 測試） |
| rsmpeg-format | 10 |
| rsmpeg-filter | 4 |

## 里程碑狀態（更新）
- M3 Decoder pipeline：native + fallback ✅
- M4 Scale：native + fallback ✅
- M4 Resample：**linear interpolation 實作完成**（非 stub）
- M5 Scheduler：native + fallback 皆用 VideoScheduler ✅
- M5 AudioClock：native audio-only 已接 MasterClock ✅
- CI：Ubuntu + Windows + macOS ✅

## 已知限制
- Resampler 仍為 linear（非 sinc/高品質），無 flush 延遲回報
- Clippy 全 workspace 仍有預存 style warning（CI 為 soft，不阻擋）
- 無真實媒體 CI 素材；同步以單元測試與邏輯驗證為主
- B-frame timestamp reorder 仍依賴 decoder 內部佇列（best-effort）
