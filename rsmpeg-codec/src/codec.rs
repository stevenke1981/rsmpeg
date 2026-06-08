use crate::codec_id::CodecId;
use rsmpeg_util::MediaType;

/// Capabilities of a codec.
#[derive(Debug, Clone)]
pub struct CodecCapabilities {
    pub can_decode: bool,
    pub can_encode: bool,
    pub lossless: bool,
    pub intra_only: bool,
}

/// Codec trait — all codec implementations implement this.
pub trait Codec: Send + Sync {
    fn id(&self) -> CodecId;
    fn media_type(&self) -> MediaType;
    fn name(&self) -> &'static str;
    fn long_name(&self) -> &'static str;
    fn capabilities(&self) -> CodecCapabilities;
}
