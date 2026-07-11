# rsmpeg 測試紀錄

分支：`feat/native-playback-pipeline`  
日期：2026-07-11（第十二刀 multi-agent round 8）

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
| rsmpeg-player | **80**（+4：sync controller 4） |
| rsmpeg-codec | 28（+1：new_audio 單測） |
| rsmpeg-scale | 10（+2：bgr24） |
| rsmpeg-util | 16（+4：pixel_format helpers） |
| rsmpeg-resample | 20（+9：channel mix helpers） |
| rsmpeg-format | 10 |
| rsmpeg-filter | 7（+3：grayscale） |
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

## round 6 重點
- OpenH264Decoder：`reset()` 先清 pending/pts_queue/eof/sps_pps_sent 再重建 decoder；單包解碼失敗改 `warn!`+跳過（不再殺播放）+ 3 單測
- SymphoniaAudioDecoder：`reset()` 顯式清 pending/eof；新增 `map_sample_format`（Symphonia→rsmpeg_util）+ 3 單測（含 reset 清幀）
- MasterClock：新增 `pause()/resume()/is_paused()/seek_to()`，pause 凍結位置（wall + audio-master 雙路徑）+ 4 單測

## round 7 重點
- SyncController（player/src/sync.rs）：A/V drift 決策 Render/Drop/Duplicate（預設 40ms 容差）+ 4 單測
- rsmpeg-scale：新增 `yuv420p_frame_to_bgr24`（BGR 序、3 bytes/pixel，複用 BT.601 數學）+ 2 單測
- rsmpeg-filter：新增 `GrayscaleFilter`（RGBA→灰階，保留 alpha，符合 Filter trait）+ 3 單測

## round 8 重點
- rsmpeg-util：PixelFormat 新增 `is_yuv()`/`is_rgb()`/`channels()`（`planes()` 已存在）+ 4 單測
- rsmpeg-codec：`new_audio` 已存在，補 `new_audio_sets_fields` 單測（+1）
- rsmpeg-resample：新增 `channel.rs`（stereo↔mono f32/i16mix 4 助手）+ 9 單測

## 已知限制
- ring 播放估算為近似；低/高水位、silence-on-underflow 未做
- VFR 仍依賴解碼幀 PTS，若上游未帶 PTS 始終退回固定幀率
- FramePool / SyncController 尚未接入播放主迴圈（demux_worker / native_pipeline）
- `map_sample_format` 為未來多格式輸出的 building block，尚未接入解碼路徑
- Clippy 仍有預存 style warning（soft）
