# rsmpeg 重構驗收摘要（第五刀 — multi-agent catch-up）

分支：`feat/native-playback-pipeline`

## 本輪完成（並行 subagent）

### Stream A — Codec foundation
- `Decoder` 改為 **send_packet / receive_frame / reset** + `DecodeStatus`
- `Frame::new_video` 正確 YUV/RGB plane 配置（`PixelFormat::plane_sizes`）
- Raw/PCM 已遷移

### Stream B/C — Decoder backends
- `backend/openh264_dec.rs`：`OpenH264Decoder` 實作 trait，輸出 YUV420P
- `backend/symphonia_audio.rs`：packet-in AAC/PCM/MP3，無 FormatReader demux

### Stream D — Scale + sync scaffolding
- `rsmpeg-scale` 真實 **YUV420P → RGBA/RGB24**（BT.601 limited）
- `VideoScheduler`（Wait / Display / DropLate）
- `video_convert::yuv420p_frame_to_rgba`
- Clock 增加 audio-sample helpers

### Stream E — CI
- `.github/workflows/ci.yml`：Ubuntu stable、fmt check、`cargo test --workspace`

### Integration
- **native_pipeline** 不再直接呼叫 `openh264::` / `symphonia::`
- 路徑：`rsmpeg-format demux → Decoder backends → rsmpeg-scale → UI`

## 驗收

```text
cargo test --workspace   # PASS（player 44 tests, codec 27, scale 8, util 12）
cargo build --release -p rsmpeg-cli -p rsmpeg-player  # PASS
```

## 仍未完成
- demux_worker Symphonia **fallback** 仍直接呼叫 OpenH264/Symphonia（非 native 路徑）
- VideoScheduler 尚未完全取代 Instant pacing
- rsmpeg-resample 尚未接入播放音訊
- B-frame PTS reorder 仍為 FIFO best-effort
- Clippy -D warnings / Windows CI job

## 下一刀
1. 將 demux_worker fallback 也改走 backend
2. VideoScheduler 接入 native pacing
3. AudioClock master + rodio samples played
4. 真實 H.264+AAC 手動播放驗證
