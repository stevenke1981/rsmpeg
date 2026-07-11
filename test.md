# rsmpeg 測試紀錄

分支：`feat/native-playback-pipeline`  
日期：2026-07-11（第八刀 multi-agent round 4）

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
| rsmpeg-player | **60**（+10：ring 5 / scheduler 2 / cached convert 1 / scaler_cache 1 / ?） |
| rsmpeg-codec | 27 |
| rsmpeg-scale | 8 |
| rsmpeg-util | 12 |
| rsmpeg-resample | 11 |
| rsmpeg-format | 10 |
| rsmpeg-filter | 4 |
| release build | **PASS** |
| fmt --check | **PASS** |

## 本輪重點
- PcmRingBuffer 元件 + demux_worker 樣本數節流（rodio backstop 保留）
- ScalerCache 按解析度重用 Scaler（native + flush 路徑）
- VideoScheduler::drop_before_seek + native/fallback seek 丟棄 pre-target frame

## 已知限制
- ring 播放估算為近似；低/高水位、silence-on-underflow 未做
- Clippy 仍有預存 style warning（soft）
