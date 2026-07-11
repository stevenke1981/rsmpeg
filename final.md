# rsmpeg 重構驗收摘要（第二刀）

分支：`feat/native-playback-pipeline`

## 本輪完成

### Phase 2 深化：統一播放核心真正跑起來
- `rsmpeg-player` 背景 `demux_worker`：Symphonia demux + OpenH264 + rodio
- Host（CLI/GUI）**只** `send_command` / `poll_event`，不解碼
- Bounded command/event channel；VideoFrame 可丟棄
- Generation id 用於 seek
- Features：`backend-symphonia` / `backend-openh264` / `audio-rodio`

### Phase 9 部分：CLI + GUI 遷移
- GUI `MediaApp` 持有 `Player`，移除舊 `engine.rs` / `state.rs`
- CLI `play` 改走 `Player` + minifb 顯示
- Stop/換檔不在 UI thread join 卡住（detach Shutdown）

### Phase 3 部分：WAV demux 真正 `read_frame`
- `WAVDemuxer` 有狀態，連續輸出 PCM `Packet`
- `FormatContext::seek` 對外 API

### Phase 1 工具上移
- `codec_detect` / `h264_bitstream` 遷入 `rsmpeg-player`（cli re-export）

## 驗收

```text
cargo test --workspace          # PASS
cargo build --release -p rsmpeg-cli -p rsmpeg-player  # PASS
```

## 下一刀建議

1. MP4 sample index + `read_frame` 真正吐 H.264/AAC packet（Phase 3.2–3.3）
2. OpenH264 / Symphonia 包成 rsmpeg `Decoder` trait backend（Phase 4）
3. AudioClock + VideoScheduler（Phase 7）
4. GUI 顯示 codec / dropped frames stats（Phase 9.1 剩餘）
