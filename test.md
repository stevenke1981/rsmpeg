# rsmpeg 測試紀錄

分支：`feat/native-playback-pipeline`  
日期：2026-07-11（第六刀 multi-agent round 2）

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
| release build | **PASS** |

## 本輪重點
- demux_worker 無 `openh264::` / `write_rgba8`
- native VideoScheduler pacing
- `frame_to_s16_device` identity + rate-change tests
- CI: ubuntu + windows + soft clippy

## 已知限制
- Resampler 非 identity 路徑仍可能輸出靜音（stub）
- fallback demux 未接 VideoScheduler
- 無真實媒體 CI 素材
