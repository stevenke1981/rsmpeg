use crate::codec_id::CodecId;
use crate::codec_parameters::CodecParameters;
use crate::frame::Frame;
use crate::packet::Packet;
use rsmpeg_util::{MediaType, RsResult};

/// Capabilities of a codec.
#[derive(Debug, Clone)]
pub struct CodecCapabilities {
    pub can_decode: bool,
    pub can_encode: bool,
    pub lossless: bool,
    pub intra_only: bool,
}

impl CodecCapabilities {
    pub const fn decoder() -> Self {
        CodecCapabilities {
            can_decode: true,
            can_encode: false,
            lossless: false,
            intra_only: false,
        }
    }
    pub const fn encoder() -> Self {
        CodecCapabilities {
            can_decode: false,
            can_encode: true,
            lossless: false,
            intra_only: false,
        }
    }
    pub const fn decoder_encoder() -> Self {
        CodecCapabilities {
            can_decode: true,
            can_encode: true,
            lossless: false,
            intra_only: false,
        }
    }
}

/// Codec descriptor — metadata about a codec.
pub trait Codec: Send + Sync {
    fn id(&self) -> CodecId;
    fn media_type(&self) -> MediaType;
    fn name(&self) -> &'static str;
    fn long_name(&self) -> &'static str;
    fn capabilities(&self) -> CodecCapabilities;
    /// Create a new decoder instance for this codec.
    fn create_decoder(&self) -> RsResult<Box<dyn Decoder>>;
    /// Create a new encoder instance for this codec.
    fn create_encoder(&self) -> RsResult<Box<dyn Encoder>>;
}

/// Result of a non-blocking `receive_frame` call (FFmpeg send/receive model).
#[derive(Debug)]
pub enum DecodeStatus {
    /// A decoded frame is ready.
    Frame(Frame),
    /// Decoder needs more input packets before producing output.
    NeedMoreInput,
    /// End of stream — no more frames will be produced until `reset`.
    EndOfStream,
}

/// Decoder trait — converts Packets into Frames (send/receive model).
///
/// Callers typically:
/// 1. `send_packet(Some(packet))` for each input packet
/// 2. Loop `receive_frame()` until `NeedMoreInput`
/// 3. `send_packet(None)` then drain with `receive_frame` / `flush` at EOS
/// 4. `reset()` after seek
pub trait Decoder: Send {
    fn codec_id(&self) -> CodecId;

    /// Feed a packet into the decoder. `None` signals end-of-stream (flush).
    fn send_packet(&mut self, packet: Option<&Packet>) -> RsResult<()>;

    /// Pull the next decoded frame, or a status indicating more input / EOS.
    fn receive_frame(&mut self) -> RsResult<DecodeStatus>;

    /// Clear internal state (reorder buffers, pending frames, EOS flag).
    /// Required after seek and before reusing a drained decoder.
    fn reset(&mut self) -> RsResult<()>;

    /// Get codec parameters (dimensions, sample rate, etc.)
    fn get_parameters(&self) -> CodecParameters;

    /// Convenience: send one packet and collect all immediately available frames.
    ///
    /// Loops `send_packet` + `receive_frame` until `NeedMoreInput` or `EndOfStream`.
    fn decode(&mut self, packet: &Packet) -> RsResult<Vec<Frame>> {
        self.send_packet(Some(packet))?;
        let mut frames = Vec::new();
        loop {
            match self.receive_frame()? {
                DecodeStatus::Frame(f) => frames.push(f),
                DecodeStatus::NeedMoreInput | DecodeStatus::EndOfStream => break,
            }
        }
        Ok(frames)
    }

    /// Flush remaining frames at end of stream.
    ///
    /// Sends `None` then drains until `EndOfStream` / `NeedMoreInput`.
    fn flush(&mut self) -> RsResult<Vec<Frame>> {
        self.send_packet(None)?;
        let mut frames = Vec::new();
        loop {
            match self.receive_frame()? {
                DecodeStatus::Frame(f) => frames.push(f),
                DecodeStatus::NeedMoreInput | DecodeStatus::EndOfStream => break,
            }
        }
        Ok(frames)
    }
}

/// Encoder trait — converts Frames into Packets.
pub trait Encoder: Send {
    fn codec_id(&self) -> CodecId;
    /// Encode a frame into one or more packets.
    fn encode(&mut self, frame: &Frame) -> RsResult<Vec<Packet>>;
    /// Flush remaining packets at end of stream.
    fn flush(&mut self) -> RsResult<Vec<Packet>>;
    /// Get codec parameters.
    fn get_parameters(&self) -> CodecParameters;
}
