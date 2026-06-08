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

/// Decoder trait — converts Packets into Frames.
pub trait Decoder: Send {
    fn codec_id(&self) -> CodecId;
    /// Decode a packet into one or more frames.
    /// Returns empty vec if more data is needed.
    fn decode(&mut self, packet: &Packet) -> RsResult<Vec<Frame>>;
    /// Flush remaining frames at end of stream.
    fn flush(&mut self) -> RsResult<Vec<Frame>>;
    /// Get codec parameters (dimensions, sample rate, etc.)
    fn get_parameters(&self) -> CodecParameters;
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
