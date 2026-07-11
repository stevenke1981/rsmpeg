# rsmpeg 測試紀錄

分支：`feat/native-playback-pipeline`  
日期：2026-07-11（第七刀 multi-agent round 3）

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
| rsmpeg-player | **50** tests |
| rsmpeg-codec | 27 |
| rsmpeg-scale | 8 |
| rsmpeg-util | 12 |
| rsmpeg-resample | **11**（新增 4：S16 upsample 非靜音 / DC passthrough / F32→S16 / short-plane error） |
| rsmpeg-format | 10 |
| rsmpeg-filter | 4 |
| release build | **PASS** |
| fmt --check | **PASS** |

## 本輪重點
- resampler 真正 SRC（S16/F32、rate、channel remap），不再靜音
- native audio-only 用 MasterClock 驅動 position
- demux_worker 用 VideoScheduler + frame_to_s16_device
- CI 增加 macOS job

## 已知限制
- Clippy 仍有預存 style warning（soft）
- 無真實媒體 CI
