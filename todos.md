# rsmpeg 改善開發待辦清單

> 專案：`stevenke1981/rsmpeg`  
> 目標：讓播放、解碼、重採樣與縮放功能真正走 `rsmpeg` 自己的  
> `demux → decode → resample/scale → sync → output` 管線。  
> 原則：CLI 與 GUI 共用同一套播放核心，不再各自維護重複的 Symphonia/OpenH264 播放流程。

---

## 0. 執行原則

- [x] 所有修改先建立獨立分支，例如 `feat/native-playback-pipeline`
- [x] 每個階段完成後執行 `cargo fmt --all`
- [ ] 每個階段完成後執行 `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- [x] 每個階段完成後執行 `cargo test --workspace`
- [x] 所有播放器 queue 必須有容量上限，禁止無限增長
- [x] 禁止 UI 執行緒直接執行 demux、decode、resample、scale 或等待播放執行緒
- [x] 不支援的 codec 必須回報明確錯誤，不得誤送到錯誤 decoder
- [x] Seek、Pause、Stop、換檔均不得造成 GUI 卡死
- [x] 優先確保播放正確性，再進行效能優化
- [ ] 所有時間戳統一使用明確的 `pts + time_base`
- [x] 所有 decoder 在 seek 後都必須 reset 或重建
- [x] 所有測試結果與已知限制記錄到 `test.md` 與 `final.md`

---

# Phase 1：修正現有播放器的高優先級錯誤

## 1.1 修正 Pause／Resume 音訊不同步

目標檔案：

- `rsmpeg-cli/src/gui/engine.rs`
- `rsmpeg-cli/src/gui/state.rs`
- `rsmpeg-cli/src/gui/ui.rs`
- `rsmpeg-cli/src/playback.rs`

待辦：

- [x] Pause 時呼叫音訊輸出端的 `pause`
- [x] Resume 時呼叫音訊輸出端的 `play`
- [x] Resume 後重新錨定播放時鐘
- [x] Pause 時不得繼續累積音訊 queue
- [x] Pause 後影片與聲音必須在合理延遲內同時停止
- [x] Resume 後不得出現聲音先行或畫面追趕數秒的情況
- [ ] 新增 pause 10 秒後恢復的整合測試
- [x] 新增連續 pause/resume 20 次的穩定性測試

驗收：

- [x] Pause 後畫面與音訊皆停止
- [ ] Resume 後 A/V 差距不持續擴大
- [x] Pause 狀態下記憶體使用量不持續增加

---

## 1.2 修正 codec 判斷

待辦：

- [x] 移除「未知 track 等於 H.264 視訊」的判斷方式
- [x] 建立容器 codec tag 到 `CodecId` 的明確 mapping
- [x] 只有 `CodecId::H264` 才能建立 OpenH264 decoder
- [x] HEVC、VP9、AV1、MPEG-2、MJPEG 等未支援 codec 顯示明確錯誤
- [x] 未支援視訊時允許繼續播放可支援的音訊
- [x] 字幕、附件與資料軌不得被誤判為視訊
- [ ] 多視訊軌時支援選擇預設軌或指定軌
- [x] 新增 codec detection 單元測試

驗收：

- [x] HEVC 檔案不再送入 OpenH264
- [x] 不支援 codec 不得大量輸出重複 decode error
- [x] 音訊可支援時能正常進入 audio-only fallback

---

## 1.3 修正 AVCC／Annex B 判斷

待辦：

- [x] 在 `CodecParameters` 增加 H.264 bitstream 格式資訊
- [x] 新增 `H264BitstreamFormat::Avcc`
- [x] 新增 `H264BitstreamFormat::AnnexB`
- [x] MP4/H.264 由 `avcC` 決定 NAL length size
- [x] MPEG-TS 或 raw H.264 不得重複執行 AVCC 轉 Annex B
- [ ] MKV/H.264 使用 CodecPrivate 判斷格式
- [x] AVCC packet 轉換遇到損毀 NAL 必須回傳錯誤或警告
- [x] 禁止默默回傳空 buffer 造成難以追蹤的黑畫面
- [x] 新增 1、2、4 byte NAL length size 測試
- [x] 新增已是 Annex B 的 packet 測試
- [x] 新增損毀 AVCC packet 測試

---

## 1.4 移除整檔掃描 `avcC`

待辦：

- [x] 移除播放器中的 `std::fs::read(path)` 整檔讀取
- [x] 不再由播放器自行掃描 MP4 二進位尋找 `avcC`
- [x] 由 MP4 demuxer 解析 `stsd → avc1/avc3 → avcC`
- [x] 將 avcC payload 存入 `Stream.codec_params.extradata`
- [x] 多視訊軌時每個 stream 必須保存自己的 extradata
- [x] 支援 extended-size ISOBMFF box
- [x] 支援 box size 為 0 的合法情況
- [x] 遇到 fragmented MP4 時回報目前支援狀態
- [x] 測試大型 MP4 開檔時不得按檔案大小配置記憶體

驗收：

- [x] 10 GB 以上 MP4 開檔不會嘗試讀入全部檔案
- [x] SPS/PPS 由 stream codec parameters 取得
- [ ] 多視訊軌不會使用錯誤的 avcC

---

# Phase 2：建立統一的 rsmpeg 播放核心

## 2.1 新增 `rsmpeg-player` crate

新增結構：

```text
rsmpeg-player/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── player.rs
    ├── command.rs
    ├── event.rs
    ├── clock.rs
    ├── queue.rs
    ├── demux_worker.rs
    ├── audio/
    │   ├── mod.rs
    │   ├── decoder.rs
    │   ├── resampler.rs
    │   ├── ring_buffer.rs
    │   └── output.rs
    ├── video/
    │   ├── mod.rs
    │   ├── decoder.rs
    │   ├── scheduler.rs
    │   ├── converter.rs
    │   └── frame_pool.rs
    └── backend/
        ├── mod.rs
        ├── native.rs
        ├── symphonia.rs
        └── openh264.rs
```

待辦：

- [x] 將 `rsmpeg-player` 加入 workspace
- [x] 建立 `Player`
- [x] 建立 `PlayerBuilder`
- [x] 建立 `PlayerCommand`
- [x] 建立 `PlayerEvent`
- [x] 建立 `PlayerState`
- [x] 建立 `PlayerError`
- [x] GUI 與 CLI 只透過 `rsmpeg-player` 播放
- [x] 移除 CLI 與 GUI 重複的播放實作
- [ ] 外部後端全部透過 adapter 接入
- [x] native pipeline 設為主要實作
- [x] 外部後端只作為可選 fallback
- [x] 播放器不得直接依賴容器特定解析函式

建議 API：

```rust
let player = Player::builder()
    .input(path)
    .prefer_native_pipeline(true)
    .build()?;

player.play()?;
player.pause()?;
player.seek(Duration::from_secs(60))?;
player.set_volume(0.8)?;

while let Some(event) = player.poll_event() {
    match event {
        PlayerEvent::VideoFrame(frame) => renderer.render(frame),
        PlayerEvent::PositionChanged(position) => update_position(position),
        PlayerEvent::Ended => break,
        PlayerEvent::Error(error) => show_error(error),
        _ => {}
    }
}
```

---

## 2.2 建立命令與事件通道

待辦：

- [x] 使用 bounded command channel
- [x] 使用 bounded event channel
- [x] 使用 bounded video frame channel
- [ ] 使用 bounded audio frame channel
- [x] 建立 `Play`
- [x] 建立 `Pause`
- [x] 建立 `Stop`
- [x] 建立 `Seek`
- [x] 建立 `SetVolume`
- [x] 建立 `SelectAudioTrack`
- [x] 建立 `SelectVideoTrack`
- [x] 建立 `SetPlaybackRate`
- [x] 建立 `Shutdown`
- [x] 命令必須包含 generation 或 sequence id
- [x] 舊 generation 的 frame 與事件必須可丟棄
- [x] UI 不再直接修改大型 `Arc<Mutex<PlaybackState>>`
- [ ] 只保留適合原子操作的狀態為 atomic
- [x] 複合狀態透過 snapshot event 傳遞

---

# Phase 3：讓 rsmpeg-format 真正執行 demux

## 3.1 重構 `FormatContext`

待辦：

- [ ] `open_input` 後建立實際 demuxer instance
- [ ] `read_header` 不再使用硬編碼 format-name match
- [ ] Format registry 能建立新的 demuxer instance
- [ ] 為 demuxer 增加 factory trait
- [ ] `read_frame` 必須真正回傳 packet
- [ ] `seek` 必須透過 `FormatContext` 對外提供
- [ ] 增加 `flush`
- [ ] 增加 stream selection
- [ ] 增加 metadata 與 chapter 介面
- [ ] 增加可取得 container start time
- [ ] 增加 duration 的可靠來源
- [ ] 所有錯誤保留 container、stream、offset 上下文

建議介面：

```rust
pub trait Demuxer: Send {
    fn read_header(&mut self, io: &mut IOContext) -> RsResult<DemuxerInfo>;
    fn read_packet(&mut self, io: &mut IOContext) -> RsResult<Option<Packet>>;
    fn seek(&mut self, io: &mut IOContext, request: SeekRequest) -> RsResult<SeekResult>;
    fn flush(&mut self);
}
```

---

## 3.2 重寫 MP4 box parser

待辦：

- [x] 建立 `BoxHeader`
- [x] 支援 32-bit box size
- [x] 支援 64-bit extended size
- [x] 支援 size=0
- [x] 每個 parser 必須接受 `parent_end`
- [x] parser 不得越過父 box 邊界
- [x] parser 發現損毀 box 時回傳帶 offset 的錯誤
- [x] 支援未知 box 安全跳過
- [ ] 建立 box depth 限制
- [ ] 建立最大 box size 防護
- [ ] 為 nested box parser 加入 fuzz 測試

必須解析：

- [x] `ftyp`
- [x] `moov`
- [ ] `mvhd`
- [x] `trak`
- [ ] `tkhd`
- [x] `mdia`
- [x] `mdhd`
- [x] `hdlr`
- [x] `minf`
- [x] `stbl`
- [x] `stsd`
- [x] `stts`
- [x] `ctts`
- [x] `stsc`
- [x] `stsz`
- [x] `stco`
- [x] `co64`
- [x] `stss`
- [ ] `edts`
- [ ] `elst`
- [x] `avc1`
- [x] `avc3`
- [x] `avcC`
- [x] `mp4a`
- [x] `esds`

後續支援：

- [ ] `moof`
- [ ] `traf`
- [ ] `tfhd`
- [ ] `tfdt`
- [ ] `trun`
- [ ] fragmented MP4 sample index

---

## 3.3 建立 MP4 sample index

待辦：

- [x] 根據 `stsc` 建立 chunk 到 sample mapping
- [x] 根據 `stsz` 建立 sample size
- [x] 根據 `stco/co64` 建立 sample offset
- [x] 根據 `stts` 建立 DTS
- [x] 根據 `ctts` 建立 PTS
- [x] 根據 `stss` 標記 keyframe
- [ ] 根據 edit list 修正 timeline
- [x] 產生完整 `Packet`
- [x] Packet 必須正確填入 `pts`
- [x] Packet 必須正確填入 `dts`
- [x] Packet 必須正確填入 `duration`
- [x] Packet 必須正確填入 `stream_index`
- [x] Packet 必須正確填入 `flags`
- [x] Packet 必須正確填入 `pos`
- [x] Packet 必須正確填入 `time_base`
- [x] 支援多音訊與多視訊軌交錯輸出
- [x] packet 依 DTS 順序輸出
- [x] 不得假設所有 sample duration 相同

驗收：

- [x] H.264 + AAC MP4 可由 rsmpeg-format 連續讀出 packet（合成 + sample index；player 尚未切 native path）
- [x] B-frame MP4 的 PTS 與 DTS 不相同且正確（ctts 單元測試）
- [x] keyframe flag 正確
- [x] seek 可定位到目標前最近 keyframe

---

## 3.4 實作 Matroska／WebM demux

待辦：

- [ ] 建立 EBML variable integer parser
- [ ] 解析 Segment
- [ ] 解析 Info
- [ ] 解析 Tracks
- [ ] 解析 Cluster
- [ ] 解析 SimpleBlock
- [ ] 解析 BlockGroup
- [ ] 解析 Cues
- [ ] 解析 CodecPrivate
- [ ] 建立 H.264 codec mapping
- [ ] 建立 VP8／VP9／AV1 codec mapping
- [ ] 建立 Opus／Vorbis／AAC codec mapping
- [ ] 產生正確 PTS
- [ ] 標記 keyframe
- [ ] 支援 seek 到 Cue
- [ ] 移除 placeholder stream 行為

---

## 3.5 完成 WAV／FLAC／AVI demux

待辦：

- [x] 確認 WAV `read_frame` 可連續產生 PCM packet
- [ ] 確認 FLAC `read_frame` 可產生 frame packet
- [ ] 完成 AVI idx1／OpenDML index
- [ ] 解析 AVI stream header
- [ ] 解析 AVI codec tag
- [ ] 正確計算 AVI PTS／duration
- [ ] 為所有 demuxer 實作真正 seek
- [ ] 移除所有 `Ok(None)` 骨架實作
- [ ] 移除所有「假成功」seek 實作

---

# Phase 4：讓 rsmpeg-codec 真正執行 decode

## 4.1 重構 Decoder trait

待辦：

- [x] 將一次性 `decode(packet) -> Vec<Frame>` 改成 send/receive 模型
- [x] 新增 `send_packet`
- [x] 新增 `receive_frame`
- [x] 新增 `drain`（via receive loop / default decode）
- [x] 新增 `reset`
- [x] 新增 `flush`
- [ ] 新增 `FormatChanged`
- [x] 新增 `NeedMoreInput`
- [x] 新增 `EndOfStream`
- [x] 支援一 packet 多 frame
- [x] 支援多 packet 一 frame
- [ ] 支援 decoder reorder
- [x] seek 後可清除 reorder queue（reset）
- [ ] 解碼錯誤帶入 packet PTS、DTS、stream index
- [ ] 不得將正常 B-frame reorder 視為 decode error

建議介面：

```rust
pub enum DecodeStatus<T> {
    Frame(T),
    NeedMoreInput,
    EndOfStream,
    FormatChanged(CodecParameters),
}

pub trait Decoder: Send {
    fn codec_id(&self) -> CodecId;
    fn send_packet(&mut self, packet: Option<&Packet>) -> RsResult<()>;
    fn receive_frame(&mut self) -> RsResult<DecodeStatus<Frame>>;
    fn reset(&mut self) -> RsResult<()>;
    fn parameters(&self) -> &CodecParameters;
}
```

---

## 4.2 修正 `Frame` 記憶體模型

待辦：

- [x] 移除所有 plane 都配置 `width * height` 的錯誤邏輯
- [x] 建立 `PixelFormatDescriptor`（via `plane_sizes` helper）
- [x] 描述 plane count
- [x] 描述 chroma subsampling
- [x] 描述 packed／planar
- [x] 描述 bytes per component
- [x] 描述 bit depth
- [ ] 描述 alignment
- [x] 正確配置 YUV420P
- [x] 正確配置 YUV422P
- [x] 正確配置 YUV444P
- [x] 正確配置 NV12/NV21
- [x] 正確配置 RGB24/BGR24
- [x] 正確配置 RGBA/BGRA/ARGB
- [x] 正確配置 Gray8/Gray16
- [x] 正確配置 10-bit／12-bit
- [x] 支援獨立 stride（linesize）
- [ ] 支援 coded size 與 display size
- [ ] 支援 crop rectangle
- [ ] 支援 sample aspect ratio
- [ ] 支援 color range
- [ ] 支援 color primaries
- [ ] 支援 transfer characteristic
- [ ] 支援 matrix coefficients
- [x] Frame 增加 PTS
- [ ] Frame 增加 DTS 或 best-effort timestamp
- [x] Frame 增加 duration
- [x] Frame 增加 time base
- [ ] Frame 增加 seek generation
- [ ] 使用 `Arc<[u8]>` 或 frame pool 減少複製

---

## 4.3 將 OpenH264 包裝為 rsmpeg decoder backend

待辦：

- [x] 在 `rsmpeg-codec` 或獨立 backend crate 建立 `H264Decoder`（player `backend/openh264_dec`）
- [x] 實作 rsmpeg `Decoder` trait
- [x] 從 `CodecParameters.extradata` 取得 SPS/PPS
- [x] 支援 AVCC input
- [x] 支援 Annex B input
- [x] 建立 packet timestamp reorder queue（FIFO best-effort）
- [x] decoder 輸出 frame 時附上正確 timestamp
- [x] 處理一 packet 無 frame
- [x] 處理 decoder flush
- [ ] 處理 resolution change
- [ ] 處理 SPS/PPS 更新
- [x] seek 後 reset decoder
- [x] 不得在 player 層直接呼叫 OpenH264 API（native + fallback demux_worker 皆隔離）
- [x] feature 名稱建議為 `codec-openh264`（現為 `backend-openh264`）

---

## 4.4 將 Symphonia 音訊 decoder 包裝為 rsmpeg backend

待辦：

- [x] 建立 `SymphoniaAudioDecoder`
- [x] 實作 rsmpeg `Decoder` trait
- [x] 支援 MP3
- [x] 支援 AAC
- [ ] 支援 FLAC
- [ ] 支援 Vorbis
- [ ] 支援 Opus（若後端可用）
- [ ] 支援 ALAC
- [x] 支援 PCM
- [x] 將 Symphonia audio buffer 轉成 rsmpeg `Frame`
- [x] 保留 sample format
- [x] 保留 channel layout
- [x] 保留 sample rate
- [x] 保留 frame PTS 與 duration
- [x] decoder 不得依賴 Symphonia 自己重新 demux 同一檔案
- [x] 音訊 decoder 只接收 rsmpeg-format 產生的 packet（native path）
- [x] feature 名稱建議為 `codec-symphonia`（現為 `backend-symphonia`）

---

## 4.5 擴充 Codec Registry

待辦：

- [ ] Codec registry 支援同一 CodecId 的多個 backend
- [ ] 支援 backend priority
- [ ] 支援 software／hardware 標記
- [ ] 支援 runtime capability probe
- [ ] 支援使用者指定 backend
- [ ] 支援 fallback
- [ ] 建立 decode-only codec registry
- [ ] 建立 encode-only codec registry
- [ ] 避免 `open()` 自動先嘗試 decoder 再 encoder 的模糊行為
- [ ] API 明確區分 `open_decoder` 與 `open_encoder`

---

# Phase 5：整合 rsmpeg-resample 音訊管線

## 5.1 建立標準音訊 frame

待辦：

- [ ] Frame 支援 interleaved 與 planar
- [ ] Frame 支援完整 ChannelLayout
- [ ] Frame 支援 U8、S16、S32、F32、F64
- [ ] Frame 支援每聲道獨立 plane
- [ ] Frame 具有 samples per channel
- [ ] Frame 具有 sample rate
- [ ] Frame 具有 PTS 與 duration
- [ ] 新增 audio frame validation

---

## 5.2 完成 Resampler

待辦：

- [ ] 將輸入 sample format 轉成裝置格式
- [ ] 支援 sample rate conversion
- [ ] 支援 mono → stereo
- [ ] 支援 stereo → mono
- [ ] 支援 5.1 → stereo downmix
- [ ] 支援 channel mapping matrix
- [ ] 支援 planar ↔ interleaved
- [ ] 支援 drift compensation
- [ ] 支援 flush 剩餘 samples
- [ ] 支援重新配置輸入格式
- [x] 實作 linear interpolation sample rate conversion（S16/F32 互通）
- [x] 支援 mono → stereo / stereo → mono（channel mapping）
- [ ] 將重採樣延遲回報給 audio clock
- [ ] 增加高品質與低延遲模式
- [x] 增加重採樣品質測試
- [x] 增加聲道 mapping 測試

---

## 5.3 建立 PCM ring buffer

待辦：

- [ ] 不再以 rodio source 數量當作 queue 長度
- [ ] 建立固定容量 sample ring buffer
- [ ] 容量以毫秒或 sample 數表示
- [ ] 預設目標 buffer 約 100–250 ms
- [ ] 設定低水位
- [ ] 設定高水位
- [ ] 防止 overflow
- [ ] 偵測 underflow
- [ ] underflow 時輸出 silence 並回報統計
- [ ] seek 時清空 ring buffer
- [ ] pause 時停止讀取
- [ ] resume 時重新預填
- [ ] 支援取得已播放 sample 數
- [ ] 支援取得 queued sample 數
- [ ] 建立 thread-safe lock-free 或低鎖設計
- [ ] 新增長時間記憶體穩定測試

---

## 5.4 建立 AudioClock

待辦：

- [ ] 以已播放 sample 數計算音訊時間
- [ ] 扣除裝置 buffer latency
- [ ] 扣除 resampler delay
- [ ] 加入 stream start PTS
- [ ] seek 後重設 clock
- [ ] pause 時 clock 停止
- [ ] resume 時 clock 連續
- [ ] 音訊裝置不存在時切換為 wall clock
- [x] audio-only 播放的 UI position 必須由 AudioClock 更新（MasterClock 接 native path）
- [ ] 允許查詢 clock drift 與 underflow 次數

---

# Phase 6：整合 rsmpeg-scale 視訊管線

## 6.1 將 decoder frame 交給 Scaler

待辦：

- [x] 播放器不得直接呼叫 OpenH264 的 `write_rgba8`（native path）
- [x] OpenH264 backend 輸出原始 YUV frame
- [x] 使用 `rsmpeg-scale` 進行 YUV → RGBA
- [x] 支援 YUV420P
- [ ] 支援 YUV422P
- [ ] 支援 YUV444P
- [ ] 支援 NV12
- [ ] 支援 10-bit 輸入
- [x] 支援 BT.601
- [ ] 支援 BT.709
- [ ] 支援 BT.2020
- [x] 支援 limited range
- [x] 支援 full range
- [ ] 支援 gamma／transfer metadata
- [x] 對未知色彩資訊使用合理預設並記錄警告
- [x] Scaler 可重用 context
- [ ] 解析度不變時不得每 frame 重建 scaler（video_convert 目前每 frame new）
- [ ] resolution change 時可重新配置
- [x] 新增色彩準確度測試
- [ ] 新增 stride 非等於 width 的測試

---

## 6.2 建立 frame pool

待辦：

- [ ] 建立可重複使用的 video frame buffer pool
- [ ] 建立可重複使用的 RGBA output pool
- [ ] 解析度相同時重用 buffer
- [ ] queue 滿時釋放或回收 buffer
- [ ] 不再每 frame 配置新的大型 `Vec<u8>`
- [ ] 記錄 frame allocation 次數
- [ ] 1080p 播放時 allocation 次數應明顯下降
- [ ] 4K 播放時記憶體使用量保持有界

---

# Phase 7：建立正確 A/V 同步

## 7.1 建立 MasterClock

待辦：

- [ ] 有音訊時使用 AudioClock
- [ ] 無音訊時使用 monotonic wall clock
- [ ] 外部同步模式保留擴充介面
- [ ] clock 支援 start
- [ ] clock 支援 pause
- [ ] clock 支援 resume
- [ ] clock 支援 seek
- [ ] clock 支援 playback rate
- [ ] clock 不得受系統牆鐘跳動影響
- [ ] clock 所有計算使用單調時間

---

## 7.2 建立 VideoScheduler

待辦：

- [x] 根據 frame PTS 與 master clock 決定顯示時間（API 已建；native + fallback demux_worker 皆使用 VideoScheduler）
- [x] 視訊過早時等待
- [x] 視訊輕微落後時立即顯示
- [x] 視訊嚴重落後時丟棄非關鍵 frame
- [ ] 不得在 demux 執行緒 sleep
- [ ] 不得在 audio decode 執行緒 sleep
- [x] frame drop threshold 可設定
- [ ] 支援 variable frame rate
- [ ] 不得固定假設 30 FPS
- [ ] missing PTS 時使用 duration／frame rate 推導
- [ ] 連續 missing PTS 時記錄警告
- [ ] 更新 UI position 時使用 master clock，而非最近 video frame
- [x] 統計 displayed、dropped、late、early frame

---

## 7.3 修正 B-frame timestamp

待辦：

- [ ] decoder backend 建立 timestamp reorder queue
- [ ] 保存輸入 packet PTS
- [ ] 保存輸入 packet DTS
- [ ] 保存 packet duration
- [ ] decoder 輸出 frame 時取得對應時間戳
- [ ] 優先使用 decoder 提供的 timestamp
- [ ] 無法取得時使用 best-effort timestamp
- [ ] 不得直接使用目前輸入 packet 的 PTS 當輸出 frame PTS
- [ ] 新增含 B-frame MP4 測試
- [ ] 新增解碼延遲超過一 packet 的測試
- [ ] 新增 decoder drain 後剩餘 frame timestamp 測試

---

# Phase 8：完成 Seek 管線

## 8.1 建立 SeekRequest／SeekResult

待辦：

- [ ] 支援 coarse seek
- [ ] 支援 precise seek
- [ ] 支援指定 stream
- [ ] 支援 backward keyframe seek
- [ ] 回報實際 seek 到的位置
- [ ] 回報是否使用 index
- [ ] 回報 seek 失敗原因

---

## 8.2 Seek generation

待辦：

- [ ] 每次 seek 產生新的 generation id
- [ ] demux packet 帶 generation id
- [ ] decoded audio frame 帶 generation id
- [ ] decoded video frame 帶 generation id
- [ ] renderer 丟棄舊 generation frame
- [ ] audio output 丟棄舊 generation samples
- [ ] 快速連續 seek 只保留最後一次
- [ ] 新增 10 秒內連續 seek 50 次測試

---

## 8.3 Seek flush 與精確定位

Seek 時必須：

- [ ] 暫停 packet 輸出
- [ ] 清空 demux packet queue
- [ ] 清空 video packet queue
- [ ] 清空 audio packet queue
- [ ] 清空 decoded video queue
- [ ] 清空 decoded audio queue
- [ ] 清空 PCM ring buffer
- [ ] reset video decoder
- [ ] reset audio decoder
- [ ] reset resampler
- [ ] reset scaler pending state
- [ ] reset A/V clocks
- [ ] demux seek 到 target 前最近 keyframe
- [ ] 解碼 target 前必要的 reference frames
- [ ] 丟棄 PTS < target 的視訊 frame
- [ ] 丟棄或裁切 target 前的音訊 samples
- [ ] 顯示第一個 PTS >= target 的畫面
- [ ] 恢復播放或保持 pause 狀態
- [ ] paused seek 時只解出一張預覽 frame
- [ ] seek 完成後發送 `SeekCompleted` 事件

驗收：

- [ ] seek 到 60 秒不會播放 57 秒的聲音
- [ ] paused seek 可正確更新預覽畫面
- [ ] seek 後不出現舊畫面閃回
- [ ] seek 後 A/V 同步重新建立

---

# Phase 9：GUI 與 CLI 遷移

## 9.1 GUI

待辦：

- [x] `MediaApp` 改為持有 `rsmpeg_player::Player`
- [x] UI 只發送 `PlayerCommand`
- [x] UI 只接收 `PlayerEvent`
- [x] 不再直接持有 decoder thread handle
- [x] Stop 不得在 UI thread 執行 blocking join
- [x] 換檔採用非阻塞 shutdown
- [ ] 顯示目前 codec
- [ ] 顯示解析度與 FPS
- [ ] 顯示音訊格式
- [ ] 顯示目前 A/V drift
- [ ] 顯示 dropped frames
- [ ] 顯示不支援 codec 原因
- [ ] 支援音訊軌選擇
- [ ] 支援視訊軌選擇
- [ ] 支援字幕軌選擇預留
- [ ] 支援播放速度
- [ ] 支援 frame stepping 預留
- [ ] 支援 fullscreen
- [ ] 支援保持畫面比例
- [ ] 可選擇是否放大超過原始解析度

---

## 9.2 CLI

待辦：

- [x] `rsmpeg play` 使用 `rsmpeg-player`
- [x] CLI 與 GUI 使用相同 demux／decode／sync
- [ ] 新增 `--audio-track`
- [ ] 新增 `--video-track`
- [ ] 新增 `--no-audio`
- [ ] 新增 `--no-video`
- [ ] 新增 `--decoder`
- [ ] 新增 `--native-only`
- [ ] 新增 `--allow-fallback`
- [ ] 新增 `--stats`
- [ ] 新增 `--start`
- [ ] 新增 `--duration`
- [ ] 新增 `--volume`
- [ ] 不支援 codec 時 exit code 必須非 0
- [ ] audio-only CLI 不建立視窗
- [ ] video-only CLI 不等待不存在的 audio sink

---

# Phase 10：Feature、授權與專案定位

## 10.1 Cargo features

建議：

```toml
[features]
default = ["player", "gui", "codec-symphonia", "codec-openh264"]

player = ["dep:rsmpeg-player"]
gui = ["dep:eframe", "dep:rfd"]
codec-symphonia = ["dep:symphonia"]
codec-openh264 = ["dep:openh264"]
audio-rodio = ["dep:rodio"]
native-only = []
```

待辦：

- [ ] 將外部 decoder 全部 feature-gate
- [ ] native pipeline 可獨立編譯
- [ ] `--no-default-features` 可通過編譯
- [ ] feature 組合加入 CI
- [ ] README 清楚標示哪些 backend 為 pure Rust
- [ ] README 清楚標示 OpenH264 的 native library 性質
- [ ] 修正「完全無 C dependencies」與實作不一致
- [ ] 統一 workspace license 與 README license
- [ ] 明確定義 safe Rust API 與內部 backend 的關係
- [ ] 不再宣稱尚未完成的功能為完整 FFmpeg equivalent

---

# Phase 11：測試素材與自動化驗收

## 11.1 建立 media test corpus

加入或由測試腳本產生：

- [ ] H.264 baseline MP4 + AAC
- [ ] H.264 main/high profile MP4 + AAC
- [ ] H.264 MP4 含 B-frame
- [ ] H.264 MKV
- [ ] Annex B raw H.264
- [ ] Variable frame rate MP4
- [ ] 非零起始 PTS MP4
- [ ] fragmented MP4
- [ ] 多音軌 MP4
- [ ] 多視訊軌 MP4
- [ ] MP3
- [ ] FLAC
- [ ] WAV PCM
- [ ] Ogg Vorbis
- [ ] 5.1 AAC
- [ ] 損毀 MP4 box
- [ ] 損毀 H.264 packet
- [ ] 中途解析度變更樣本
- [ ] 10-bit 視訊樣本
- [ ] 長時間同步測試樣本

注意：

- [ ] 測試素材必須確認授權
- [ ] 大型素材不直接提交 Git
- [ ] 提供可重現的素材產生腳本
- [ ] 使用 checksum 驗證下載素材

---

## 11.2 單元測試

- [ ] Rational 與時間換算
- [ ] MP4 box boundary
- [ ] extended-size box
- [ ] stts 展開
- [ ] ctts 正負 offset
- [ ] stsc/stsz/stco sample mapping
- [ ] keyframe index
- [ ] AVCC extradata
- [ ] AVCC packet conversion
- [ ] Annex B detection
- [ ] EBML integer
- [ ] codec mapping
- [ ] frame plane allocation
- [ ] audio channel mapping
- [ ] resampler output sample count
- [ ] audio clock
- [ ] video scheduler
- [ ] seek generation
- [ ] bounded queue 行為
- [ ] decoder flush
- [ ] decoder reset

---

## 11.3 整合測試

- [ ] native demux → H.264 decode → scale → frame
- [ ] native demux → AAC decode → resample → PCM
- [ ] MP4 A/V 同步播放
- [ ] audio-only 播放
- [ ] video-only 播放
- [ ] Pause／Resume
- [ ] Stop
- [ ] 換檔
- [ ] Seek
- [ ] paused seek preview
- [ ] 快速連續 seek
- [ ] decoder error recovery
- [ ] 不支援 codec fallback
- [ ] UI frame queue 滿載
- [ ] audio ring buffer underflow
- [ ] audio ring buffer overflow 防護

---

## 11.4 效能與穩定性測試

- [ ] 1080p H.264 播放 30 分鐘
- [ ] 4K H.264 播放
- [ ] VFR 播放
- [ ] 低效能 CPU 播放
- [ ] 高 bitrate 播放
- [ ] 長時間 pause
- [ ] 長時間 audio-only
- [ ] 長時間 video-only
- [ ] 記憶體不得持續增長
- [ ] queue 長度保持有界
- [ ] Stop 後所有 worker 可結束
- [ ] Drop Player 不會 deadlock
- [ ] GUI 關閉不會卡住
- [ ] decoder panic 不造成整個 UI crash

---

# Phase 12：CI 與品質管控

待辦：

- [x] 新增 GitHub Actions
- [x] Windows stable Rust
- [x] Ubuntu stable Rust
- [x] macOS stable Rust
- [x] `cargo fmt --check`
- [x] `cargo clippy --workspace --all-targets --all-features -- -D warnings`（soft / continue-on-error）
- [x] `cargo test --workspace`
- [ ] `cargo test --workspace --no-default-features`
- [ ] 常用 feature 組合測試
- [ ] `cargo deny check`
- [ ] dependency license check
- [ ] `cargo audit`
- [ ] 建立 fuzz target
- [ ] MP4 box parser fuzz
- [ ] EBML parser fuzz
- [ ] AVCC parser fuzz
- [ ] packet queue state-machine 測試
- [ ] seek state-machine 測試

---

# Phase 13：文件更新

待辦：

- [ ] 更新 README 專案定位
- [ ] 更新 README-TW
- [ ] 新增播放架構圖
- [ ] 新增 demux/decode/resample/scale 資料流圖
- [ ] 新增支援容器矩陣
- [ ] 新增支援 codec 矩陣
- [ ] 標示 native 與 external backend
- [ ] 標示硬體解碼尚未支援或支援狀態
- [ ] 新增 player API 範例
- [ ] 新增 decoder backend 開發指南
- [ ] 新增 demuxer 開發指南
- [ ] 新增測試素材產生指南
- [ ] 新增效能調校指南
- [ ] 新增已知限制
- [ ] 修正 license 說明
- [ ] 更新 changelog

---

# 建議里程碑

## Milestone 1：播放正確性修復

- [ ] Pause／Resume 音訊同步
- [ ] codec 明確識別
- [ ] AVCC／Annex B 正確判斷
- [ ] 移除整檔 avcC 掃描
- [ ] CLI／GUI 共用基本播放 API

完成標準：

- [ ] 現有 H.264 MP4 播放穩定
- [ ] Pause／Resume／Stop 不再明顯錯亂
- [ ] 不支援 codec 有清楚錯誤訊息

---

## Milestone 2：原生 MP4 demux

- [x] MP4 sample table 完成
- [x] `read_frame` 產生真實 packet
- [x] PTS／DTS／duration／keyframe 正確
- [x] MP4 seek 可用

完成標準：

- [x] 播放器不再依賴 Symphonia demux MP4（native 優先；失敗才 fallback）
- [x] rsmpeg-format 可獨立輸出 MP4 packet

---

## Milestone 3：統一 decoder pipeline

- [x] H.264 backend 實作 rsmpeg Decoder
- [x] 音訊 backend 實作 rsmpeg Decoder
- [x] Frame 模型修正
- [ ] decoder timestamp reorder 正確（FIFO best-effort only）

完成標準：

- [x] Player 只接觸 rsmpeg Decoder trait（native + fallback decode）
- [x] Player 不直接呼叫 OpenH264 或 Symphonia decoder API（demux fallback 仍用 Symphonia FormatReader）

---

## Milestone 4：Resample／Scale 整合

- [x] 音訊走 rsmpeg-resample（`frame_to_s16_device` 掛接；linear interpolation 實作，非 stub）
- [x] 視訊走 rsmpeg-scale（native + fallback YUV420P→RGBA）
- [ ] audio ring buffer
- [ ] frame pool

完成標準：

- [ ] 播放資料流完整經過 rsmpeg 自有模組
- [ ] 不再由外部 decoder 直接輸出 UI 格式

---

## Milestone 5：完整 A/V 同步與 Seek

- [ ] AudioClock
- [ ] VideoScheduler
- [ ] precise seek
- [ ] seek generation
- [ ] queue flush

完成標準：

- [ ] 連續播放 30 分鐘無持續性 drift
- [ ] seek 後不播放目標前音訊
- [ ] 快速 seek 不出現舊 frame

---

# 最終 Definition of Done

只有全部滿足以下條件，才能宣告播放管線完成：

- [x] 容器資料由 `rsmpeg-format` demux（MP4/WAV native path；其他 fallback）
- [ ] 壓縮 packet 由 `rsmpeg-codec` decoder 解碼
- [ ] 音訊 frame 經過 `rsmpeg-resample`
- [ ] 視訊 frame 經過 `rsmpeg-scale`
- [x] 播放核心位於 `rsmpeg-player`
- [x] CLI 與 GUI 共用同一套 player

- [ ] 有音訊時以 AudioClock 作主時鐘
- [ ] 支援 bounded queues
- [ ] 支援正確 Pause／Resume
- [ ] 支援正確 Stop
- [ ] 支援 coarse seek
- [ ] 支援 precise seek
- [ ] 支援 B-frame timestamp reorder
- [ ] 支援 VFR
- [ ] 支援 decoder flush 與 reset
- [ ] 支援不支援 codec 的清楚錯誤
- [ ] 播放器不會整檔讀入大型媒體
- [ ] 1080p 長時間播放記憶體保持穩定
- [ ] A/V drift 不持續累積
- [ ] 全 workspace 測試通過
- [ ] Clippy 無警告
- [ ] README 與實際功能一致
