# rsmpeg 重構驗收摘要（Phase 1 + Phase 2 scaffold）

分支：`feat/native-playback-pipeline`

## 完成項目

### Phase 1.1 Pause / Resume
- Pause 時呼叫 rodio `Sink::pause`，並**停止 demux/decode**（不繼續累積音訊 queue）
- Resume 時 `Sink::play` + 重錨定 wall clock
- `rsmpeg-player` 的 `PlaybackClock` 支援 pause 凍結 / resume 連續
- 單元測試：20 次 pause/resume、clock freeze

### Phase 1.2 Codec 判斷
- 移除「`CODEC_TYPE_NULL` = H.264」假設
- 新增 `codec_detect`：fourcc / extradata 指紋
- 僅 `H264` 建立 OpenH264
- HEVC / VP9 / AV1 等顯示明確警告並 audio-only fallback

### Phase 1.3 AVCC / Annex B
- `H264BitstreamFormat::{Avcc, AnnexB}`（cli + `CodecParameters`）
- 轉換改為 fallible：損毀 NAL 回錯誤，不再默默回空 buffer
- 已是 Annex B 的 packet 不再二次轉換
- 1/2/4 byte NAL length 測試

### Phase 1.4 移除整檔 avcC 掃描
- `extract_avcc_streaming`：只 seek 讀 box header + avcC payload
- MP4 demuxer 解析 `minf → stbl → stsd → avc1/avc3 → avcC`，寫入 `stream.codec_params.extradata`

### Phase 2 scaffold：`rsmpeg-player`
- Workspace 新 crate
- `Player` / `PlayerBuilder` / `PlayerCommand` / `PlayerEvent` / `PlayerState`
- Bounded command channel + bounded event queue
- Generation id（seek 遞增）
- `MasterClock` / `PlaybackClock` / `BoundedQueue`

## 驗收指令

```text
cargo test --workspace   # 64 tests PASS
cargo fmt --all
```

## 未完成（下一切片）

- GUI/CLI 完全改持有 `rsmpeg_player::Player`（Phase 9）
- native demux `read_frame` 真正吐 packet（Phase 3）
- OpenH264 / Symphonia 包成 rsmpeg Decoder backend（Phase 4）
- A/V sync scheduler（Phase 7）
- CI / feature gate（Phase 10–12）

## 風險

- MP4 `hdlr` 與 `minf` 解析順序若異常，codec tag 可能缺失（可再以 streaming avcC 補）
- 真實 A/V pause 整合測試尚未用媒體檔跑過
