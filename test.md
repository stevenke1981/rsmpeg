# rsmpeg 測試紀錄

分支：`feat/native-playback-pipeline`  
日期：2026-07-11（第二刀）

## 指令

```text
cargo fmt --all
cargo test --workspace
cargo build --release -p rsmpeg-cli -p rsmpeg-player
```

## 結果摘要

| 項目 | 結果 |
|------|------|
| `cargo test --workspace` | **PASS**（含 rsmpeg-player 19 tests） |
| `cargo build --release -p rsmpeg-cli -p rsmpeg-player` | **PASS**（~48s） |

## 本輪新增／強化測試

### rsmpeg-player
- open missing file → Error event
- seek bumps generation
- 20× pause/resume commands（非阻塞 command channel）
- clock pause/resume/seek
- AVCC/Annex B / codec_detect（自 cli 遷入）

### 整合行為（手動驗收建議）
```text
cargo run --release -p rsmpeg-cli -- play sample.mp4
cargo run --release -p rsmpeg-cli -- gui sample.mp4
```

## 已知限制

- GUI/CLI 已共用 `rsmpeg_player::Player` worker，但 demux 仍以 Symphonia 為主（native MP4 `read_frame` 僅 WAV 真正吐 packet）
- Seek 命令在 worker 內 coarse seek；B-frame reorder 尚未完整
- `prefer_native_pipeline` flag 已預留，尚未切換完整 native packet path
