use crate::codec::Codec;
use crate::codec_id::CodecId;
use std::sync::{OnceLock, RwLock};

pub struct CodecRegistry {
    codecs: Vec<Box<dyn Codec>>,
}

impl CodecRegistry {
    pub fn new() -> Self {
        CodecRegistry { codecs: Vec::new() }
    }

    pub fn register(&mut self, codec: Box<dyn Codec>) {
        tracing::debug!("Registering codec: {} ({:?})", codec.name(), codec.id());
        self.codecs.push(codec);
    }

    pub fn find_by_id(&self, id: CodecId) -> Option<&dyn Codec> {
        self.codecs
            .iter()
            .find(|c| c.id() == id)
            .map(|c| c.as_ref())
    }

    pub fn find_by_name(&self, name: &str) -> Option<&dyn Codec> {
        self.codecs
            .iter()
            .find(|c| c.name() == name)
            .map(|c| c.as_ref())
    }

    pub fn list(&self) -> Vec<&dyn Codec> {
        self.codecs.iter().map(|c| c.as_ref()).collect()
    }

    pub fn len(&self) -> usize {
        self.codecs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.codecs.is_empty()
    }
}

impl Default for CodecRegistry {
    fn default() -> Self {
        Self::new()
    }
}

pub fn global_codec_registry() -> &'static RwLock<CodecRegistry> {
    static REGISTRY: OnceLock<RwLock<CodecRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(CodecRegistry::new()))
}

pub fn register_builtin_codecs() {
    use crate::codec_impls::*;
    use rsmpeg_util::SampleFormat;

    let mut registry = global_codec_registry()
        .write()
        .expect("codec registry lock poisoned");

    // Register raw video codec
    registry.register(Box::new(RawVideoCodec));

    // Register PCM audio codecs for common formats
    for sample_fmt in &[
        SampleFormat::U8,
        SampleFormat::S16,
        SampleFormat::S32,
        SampleFormat::F32,
    ] {
        if let Some(codec) = PCMAudioCodec::new(*sample_fmt) {
            registry.register(Box::new(codec));
        }
    }

    tracing::info!("Registered {} built-in codecs", registry.len());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::Codec;
    use crate::codec::CodecCapabilities;
    use crate::codec::Decoder;
    use crate::codec::Encoder;
    use crate::codec_parameters::CodecParameters;
    use crate::frame::Frame;
    use crate::packet::Packet;
    use rsmpeg_util::{MediaType, RsResult};

    struct TestCodec;

    impl Codec for TestCodec {
        fn id(&self) -> CodecId {
            CodecId::Mp3
        }
        fn media_type(&self) -> MediaType {
            MediaType::Audio
        }
        fn name(&self) -> &'static str {
            "test_mp3"
        }
        fn long_name(&self) -> &'static str {
            "Test MP3 Codec"
        }
        fn capabilities(&self) -> CodecCapabilities {
            CodecCapabilities::decoder()
        }
        fn create_decoder(&self) -> RsResult<Box<dyn Decoder>> {
            Ok(Box::new(TestDecoder))
        }
        fn create_encoder(&self) -> RsResult<Box<dyn Encoder>> {
            Err(rsmpeg_util::RsError::Unsupported(
                "Encoding not supported".into(),
            ))
        }
    }

    struct TestDecoder;

    impl Decoder for TestDecoder {
        fn codec_id(&self) -> CodecId {
            CodecId::Mp3
        }
        fn decode(&mut self, _packet: &Packet) -> RsResult<Vec<Frame>> {
            Ok(vec![])
        }
        fn flush(&mut self) -> RsResult<Vec<Frame>> {
            Ok(vec![])
        }
        fn get_parameters(&self) -> CodecParameters {
            CodecParameters::new(CodecId::Mp3)
        }
    }

    #[test]
    fn test_registry_find() {
        let mut registry = CodecRegistry::new();
        registry.register(Box::new(TestCodec));
        assert_eq!(registry.len(), 1);
        assert!(registry.find_by_id(CodecId::Mp3).is_some());
        assert!(registry.find_by_id(CodecId::H264).is_none());
        assert!(registry.find_by_name("test_mp3").is_some());
    }

    #[test]
    fn test_global_registry() {
        let registry = global_codec_registry();
        let r = registry.read().unwrap();
        assert!(r.is_empty());
    }

    #[test]
    fn test_codec_caps() {
        let dec = CodecCapabilities::decoder();
        assert!(dec.can_decode);
        assert!(!dec.can_encode);
    }
}
