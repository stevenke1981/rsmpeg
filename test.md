# rsmpeg 測試紀錄

分支：`feat/native-playback-pipeline`  
日期：2026-07-11

## 指令

```text
cargo fmt --all
cargo test --workspace
cargo clippy --workspace --all-targets --all-features
```

## 結果摘要

| 套件 | 單元測試 | 結果 |
|------|----------|------|
| rsmpeg-cli | 10 | PASS |
| rsmpeg-codec | 19 | PASS |
| rsmpeg-format | 5 | PASS |
| rsmpeg-player | 9 | PASS |
| rsmpeg-filter | 4 | PASS |
| rsmpeg-resample | 7 | PASS |
| rsmpeg-scale | 3 | PASS |
| rsmpeg-util | 7 | PASS |
| **合計** | **64** | **PASS** |

## Phase 1 相關新增測試

### codec_detect
- fourcc → H.264 / HEVC / VP9 / AV1 mapping
- 字幕 fourcc 不視為視訊
- avcC extradata 指紋
- HEVC 不得走 OpenH264

### h264_bitstream
- NAL length size 1 / 2 / 4
- 已是 Annex B 不再二次包裝
- 損毀 AVCC packet 回傳錯誤（非空 buffer）
- 零長度 NAL 回傳錯誤
- **串流** avcC 擷取：略過 mdat decoy，不整檔載入

### rsmpeg-player
- Builder 需要 input
- play / pause 狀態
- seek 遞增 generation
- 連續 pause/resume 20 次
- PlaybackClock pause 凍結時間
- BoundedQueue drop-oldest / drop-newest

## 尚未覆蓋（後續）

- 真實媒體檔 Pause 10 秒整合測試（需測試素材 + 音訊裝置）
- 真實 HEVC 檔案 playback 整合測試
- 10GB MP4 開檔效能測試（本輪以「不整檔 `fs::read`」靜態保證）
- GUI 端 rsmpeg-player 遷移後的 E2E

## 已知限制

- GUI / CLI 仍使用 Symphonia demux + OpenH264（尚未完全切到 native demux packet 路徑）
- `rsmpeg-player` 控制面 API 已就緒，decode worker 尚未接入
- MP4 demuxer 已可解析 `stsd/avcC` 進 extradata，但 `read_frame` 仍回 `None`（Phase 3）
