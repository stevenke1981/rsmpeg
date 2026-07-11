# rsmpeg 測試紀錄

分支：`feat/native-playback-pipeline`  
日期：2026-07-11（第九刀 multi-agent round 5）

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
| rsmpeg-player | **66**（+6：B-frame reorder 2 / FramePool 4） |
| rsmpeg-codec | 27 |
| rsmpeg-scale | 8 |
| rsmpeg-util | 12 |
| rsmpeg-resample | 11 |
| rsmpeg-format | 10 |
| rsmpeg-filter | 4 |
| release build | **PASS** |
| fmt --check | **PASS** |

## round 4 重點
- PcmRingBuffer 元件 + demux_worker 樣本數節流（rodio backstop 保留）
- ScalerCache 按解析度重用 Scaler（native + flush 路徑）
- VideoScheduler::drop_before_seek + native/fallback seek 丟棄 pre-target frame

## round 5 重點
- demux_worker：`abs_pos` 改為以解碼幀 PTS 為準（VFR 正確），無 PTS 才退回固定 1/30
- demux_worker：Seek 綁定 `mode`，僅 `SeekMode::Precise` 啟用 drop_before_seek 丟幀
- OpenH264Decoder：`take_display_order` 按 PTS 升冪出幀（B-frame 顯示序），缺 PTS 退回 FIFO
- FramePool 獨立元件（Mutex+VecDeque 緩衝池，max_bytes 預算）+ 4 項單測

## 已知限制
- ring 播放估算為近似；低/高水位、silence-on-underflow 未做
- VFR 仍依賴解碼幀 PTS，若上游未帶 PTS 始終退回固定幀率
- FramePool 尚未接入事件路徑（目前 RGBA buffer 直接 move 進 PlayerEvent，需未來事件重構）
- Clippy 仍有預存 style warning（soft）
