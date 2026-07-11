# rsmpeg 重構驗收摘要（第八刀 — multi-agent round 4）

分支：`feat/native-playback-pipeline`

## 本輪完成

### E — PcmRingBuffer（Phase 5.3）
- 新增 `rsmpeg-player/src/audio_ring_buffer.rs`：`PcmRingBuffer`
  - 固定容量（樣本數）、`push`/`consume`/`clear`/`len`/`is_full`/`capacity`/`stats`
  - overflow / underflow 統計
  - 5 個單元測試（容量內 push / overflow / underflow / clear / is_full）
- `demux_worker` 音訊節流改以 **樣本數估算**（200 ms 目標）+ 保留 rodio `sink.len()` backstop，永不 stall
- Seek 時清空 ring（防止 paused+force_one_frame 卡死）

### F — ScalerCache（Phase 6.1 效能）
- 新增 `rsmpeg-player/src/scaler_cache.rs`：thread-local `HashMap<(w,h), Scaler>` 重用
- `video_convert` 新增 `yuv420p_frame_to_rgba_cached`
- `native_pipeline` 主路徑與 flush 路徑皆改用 cached（輸出 byte-identical，不再每 frame new Scaler）

### G — drop_before_seek（Phase 8.3）
- `VideoScheduler::drop_before_seek(frame_pts, target) -> bool`（`frame_pts < target`）
- native + fallback 在 Seek 後丟棄 PTS < target 的視訊 frame（首幀 >= target 才顯示）
- `lib.rs` 註冊 `audio_ring_buffer` / `scaler_cache` 兩個新模組

## 驗收

```text
cargo test --workspace          # PASS
cargo build --release -p rsmpeg-cli -p rsmpeg-player  # PASS
cargo fmt --all -- --check      # PASS
```

| crate | tests |
|-------|-------|
| rsmpeg-player | **60**（含 5 ring / 2 scheduler / 1 cached / 1 scaler 新測試） |
| rsmpeg-codec | 27 |
| rsmpeg-scale | 8 |
| rsmpeg-util | 12 |
| rsmpeg-resample | 11 |
| rsmpeg-format | 10 |
| rsmpeg-filter | 4 |

## 里程碑狀態（更新）
- M4 Resample：linear SRC ✅
- M4 Scale：ScalerCache 重用 ✅
- M5 Scheduler：VideoScheduler + drop_before_seek ✅
- M5 AudioClock：MasterClock audio-only ✅
- Phase 5.3 PCM ring buffer：核心完成（overflow/underflow/seek-clear）
- Phase 8.3 seek 丟棄 pre-target frame：native + fallback ✅

## 已知限制
- ring buffer 播放樣本估算為 wall-clock 近似（rodio backstop 兜底，無 regression）
- 低/高水位、silence-on-underflow、長時間穩定測試尚未做
- B-frame timestamp reorder 仍依賴 decoder 內部佇列
- Clippy 仍為 soft，有預存 style warning
