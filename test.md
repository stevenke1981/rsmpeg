# rsmpeg 測試紀錄

分支：`feat/native-playback-pipeline`  
日期：2026-07-11（第四刀）

## 指令

```text
cargo fmt --all
cargo test --workspace
cargo build --release -p rsmpeg-cli -p rsmpeg-player -p rsmpeg-format
```

## 結果摘要

| 項目 | 結果 |
|------|------|
| `cargo test --workspace` | **PASS**（rsmpeg-player 20 tests、rsmpeg-format 10 tests） |
| `cargo build --release -p rsmpeg-cli -p rsmpeg-player -p rsmpeg-format` | **PASS** |

## 本輪新增／強化測試

### rsmpeg-player native pipeline
- `extract_asc_from_minimal_esds_like`：esds → AudioSpecificConfig
- 既有 player 測試仍通過（missing file / seek / pause-resume）

### 整合行為（手動驗收建議）
```text
cargo run --release -p rsmpeg-cli -- play sample.mp4
# 應出現 Warning: using native demux (mp4)
# 若 native 失敗會 fallback Symphonia
```

## 已知限制

- Native path：MP4/WAV demux via `rsmpeg-format`；H.264→OpenH264；AAC/PCM→Symphonia **decode-only**
- Symphonia 仍作 demux fallback（非 MP4、fragmented、無 sample table）
- B-frame reorder、AudioClock master、rsmpeg-scale 尚未接入
- AAC 需可解析 esds ASC；失敗時可能靜音但視訊可播
