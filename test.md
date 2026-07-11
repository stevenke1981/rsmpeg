# rsmpeg 測試紀錄

分支：`feat/native-playback-pipeline`  
日期：2026-07-11（第三刀）

## 指令

```text
cargo fmt --all
cargo test --workspace
cargo build --release -p rsmpeg-cli -p rsmpeg-player -p rsmpeg-format
```

## 結果摘要

| 項目 | 結果 |
|------|------|
| `cargo test --workspace` | **PASS**（含 rsmpeg-format 10 tests、rsmpeg-player 19 tests） |
| `cargo build --release -p rsmpeg-cli -p rsmpeg-player -p rsmpeg-format` | **PASS**（~5s incremental） |

## 本輪新增／強化測試

### rsmpeg-format MP4
- `build_sample_index_stts_stsz_stco`：sample offset / DTS / keyframe
- `build_sample_index_ctts_pts`：B-frame 風格 PTS ≠ DTS
- `demux_minimal_mp4_packets`：合成 MP4 連續讀出 3 個 packet
- `seek_to_mid_resets_cursor`：毫秒 seek 後 cursor 正確
- `extended_size_and_parent_bounds`：64-bit box header

### 整合行為（手動驗收建議）
```text
cargo run --release -p rsmpeg-cli -- play sample.mp4
cargo run --release -p rsmpeg-cli -- gui sample.mp4
```

## 已知限制

- GUI/CLI 播放仍以 Symphonia demux 為主；native MP4 `read_frame` 已可獨立吐 packet，尚未接到 player worker
- fragmented MP4（moof/traf）僅警告，不 demux
- edit list（edts/elst）尚未修正 timeline
- B-frame decoder reorder 尚未完整
