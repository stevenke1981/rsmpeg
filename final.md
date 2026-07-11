# rsmpeg 重構驗收摘要（第三刀）

分支：`feat/native-playback-pipeline`

## 本輪完成

### Phase 3.2–3.3：原生 MP4 sample-table demux
- 有狀態 `MP4Demuxer`：解析 `moov` 後建立 sample index
- 支援 `stts` / `ctts` / `stsc` / `stsz` / `stco` / `co64` / `stss`
- `read_frame` 依 DTS 交錯多軌輸出真實 `Packet`（pts/dts/duration/flags/pos/time_base）
- `seek(timestamp_ms)` 對齊最近 keyframe（有 stss 時）
- `avc1`/`avc3` + `avcC` extradata；`mp4a` → `CodecId::Aac`
- fragmented MP4（moof）明確警告，不假成功
- 單元測試：合成 MP4 連續 packet、CTTS PTS、extended-size box

### Codec
- 新增 `CodecId::Aac`

## 驗收

```text
cargo fmt --all
cargo test --workspace          # PASS
cargo build --release -p rsmpeg-cli -p rsmpeg-player -p rsmpeg-format  # PASS
```

## 下一刀建議

1. 將 player worker 切換為 native MP4 demux → OpenH264/Symphonia decode
2. edit list（edts/elst）與 multi-chunk stsc 邊界案例
3. OpenH264 / Symphonia 包成 rsmpeg `Decoder` trait backend（Phase 4）
4. AudioClock + VideoScheduler（Phase 7）
