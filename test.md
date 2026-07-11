# rsmpeg 測試紀錄

分支：`feat/native-playback-pipeline`  
日期：2026-07-11（第五刀 multi-agent）

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
| rsmpeg-codec | 27 tests |
| rsmpeg-player | 44 tests |
| rsmpeg-scale | 8 tests |
| rsmpeg-util | 12 tests |
| release build | **PASS** |

## 本輪新增測試重點
- Frame YUV plane sizes
- Decoder send/receive (Raw/PCM)
- OpenH264Decoder construct/reset
- SymphoniaAudioDecoder PCM roundtrip
- Scaler YUV420P→RGBA colour approx
- VideoScheduler wait/display/drop

## 已知限制
- Fallback demux_worker 尚未完全 backend 化
- Scheduler 統計尚未接到 UI
- 無真實媒體檔 CI 整合測試
