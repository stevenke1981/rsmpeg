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

    // Never panic during start-up registration: if the lock is poisoned the
    // registry is unusable anyway, so recover the guard and log instead of
    // aborting the process.
    let mut registry = match global_codec_registry().write() {
        Ok(g) => g,
        Err(poisoned) => {
            tracing::error!("codec registry lock poisoned; recovering to finish registration");
            poisoned.into_inner()
        }
    };

    // Registration is idempotent by construction (fresh registry per process),
    // but guard against a second call adding duplicates.
    if !registry.is_empty() {
        tracing::debug!("built-in codecs already registered; skipping");
        return;
    }

    // Register pure-Rust video codecs
    registry.register(Box::new(RawVideoCodec));
    registry.register(Box::new(MjpegCodec));

    // Register pure-Rust lossless audio codec
    registry.register(Box::new(FlacCodec));

    // Register PCM audio codecs for common formats
    for sample_fmt in &[
        SampleFormat::U8,
        SampleFormat::S16,
        SampleFormat::S32,
        SampleFormat::F32,
        SampleFormat::F64,
    ] {
        if let Some(codec) = PCMAudioCodec::new(*sample_fmt) {
            registry.register(Box::new(codec));
        }
    }

    // Register subtitle codecs
    registry.register(Box::new(SrtCodec));
    registry.register(Box::new(WebVttCodec));

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
        fn send_packet(&mut self, _packet: Option<&Packet>) -> RsResult<()> {
            Ok(())
        }
        fn receive_frame(&mut self) -> RsResult<crate::codec::DecodeStatus> {
            Ok(crate::codec::DecodeStatus::NeedMoreInput)
        }
        fn reset(&mut self) -> RsResult<()> {
            Ok(())
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
        // Hold a single read guard: other tests in this binary may call
        // `register_builtin_codecs()` at any time (the registry is
        // process-global), so taking two guards and comparing could observe
        // two different states.
        let guard = global_codec_registry().read().expect("codec registry lock");
        // Both views must agree inside one guard snapshot.
        let listed = guard.list().len();
        assert_eq!(listed, guard.len(), "list() and len() must agree");
        assert_eq!(
            guard.is_empty(),
            listed == 0,
            "is_empty() must agree with the entry count"
        );
    }

    #[test]
    fn builtin_registration_includes_every_packed_pcm_format() {
        register_builtin_codecs();
        let guard = global_codec_registry().read().expect("codec registry lock");
        for name in ["pcm_u8", "pcm_s16le", "pcm_s32le", "pcm_f32le", "pcm_f64le"] {
            let codec = guard
                .find_by_name(name)
                .unwrap_or_else(|| panic!("{name} must be registered"));
            assert_eq!(codec.id(), CodecId::Pcm);
            assert!(codec.create_decoder().is_ok());
            assert!(codec.create_encoder().is_ok());
        }
    }

    #[test]
    fn test_codec_caps() {
        let dec = CodecCapabilities::decoder();
        assert!(dec.can_decode);
        assert!(!dec.can_encode);
    }
}
