# rsmpeg 重構驗收摘要（第四刀）

分支：`feat/native-playback-pipeline`

## 本輪完成

### Milestone 2 收尾：Player 接上 native MP4 demux
- 新增 `rsmpeg-player/src/native_pipeline.rs`
- `prefer_native_pipeline`（預設 true）時優先：
  1. `FormatContext::open_input` + `read_header` + `read_frame`
  2. H.264 → OpenH264（avcC extradata / AVCC packet）
  3. AAC/PCM → Symphonia **decode-only**（不 demux 同一檔）
  4. Seek 走 `FormatContext::seek`（ms → keyframe）
- Native 不可用時自動 fallback 既有 Symphonia demux 路徑
- 發出 `using native demux (mp4)` / fallback 警告事件

### 與第三刀銜接
- 依賴已完成的 MP4 sample-table demux

## 驗收

```text
cargo fmt --all
cargo test --workspace          # PASS
cargo build --release -p rsmpeg-cli -p rsmpeg-player -p rsmpeg-format  # PASS
```

## 下一刀建議

1. Phase 4：OpenH264 / Symphonia 包成 rsmpeg `Decoder` trait
2. Phase 6：YUV → RGBA 改走 `rsmpeg-scale`
3. Phase 7：AudioClock + VideoScheduler
4. 真實 H.264+AAC 素材整合測試（非合成 box）
