# rsmpeg 重構驗收摘要（第六刀 — multi-agent round 2）

分支：`feat/native-playback-pipeline`

## 本輪完成

### F — demux_worker fallback backend 化
- Symphonia 仍負責 **demux**
- 解碼改走 `OpenH264Decoder` + `SymphoniaAudioDecoder`
- RGBA 改走 `yuv420p_frame_to_rgba`（`rsmpeg-scale`）
- `openh264::` / `write_rgba8` 已從 demux_worker 移除

### G — VideoScheduler 接入 native path
- 取代 `LATE_DROP_SEC` 手寫 pacing
- Wait / Display / DropLate + seek 時 reset stats

### H — audio_convert + resample 掛點
- `frame_to_s16_device`（identity S16 或經 rsmpeg-resample）
- native_pipeline 音訊輸出改走此 helper
- 註：Resampler 本體仍為 stub 品質（長度正確、非 identity 可能靜音）

### I — CI 擴充
- Ubuntu：fmt + test（硬失敗）
- Windows：test（45m timeout，硬失敗）
- Clippy soft（continue-on-error）

## 驗收

```text
cargo test --workspace   # PASS（player 50 tests）
cargo build --release -p rsmpeg-cli -p rsmpeg-player  # PASS
```

## 里程碑狀態（更新）
- M3 Decoder pipeline：**native + fallback 皆走 Decoder trait**
- M4 Scale：native + fallback 皆走 rsmpeg-scale
- M4 Resample：API 掛上，演算法仍待實作
- M5 Scheduler：native 已用；fallback demux_worker 仍 Instant pacing

## 下一刀
1. 實作真正的 Resampler（非 zero-fill）
2. demux_worker 也接 VideoScheduler
3. AudioClock master 驅動 position
4. B-frame PTS reorder
