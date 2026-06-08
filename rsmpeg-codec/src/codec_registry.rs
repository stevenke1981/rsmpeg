use crate::codec::Codec;
use std::sync::{OnceLock, RwLock};

pub struct CodecRegistry {
    codecs: Vec<Box<dyn Codec>>,
}

impl CodecRegistry {
    pub fn new() -> Self {
        CodecRegistry { codecs: Vec::new() }
    }
    pub fn register(&mut self, codec: Box<dyn Codec>) {
        self.codecs.push(codec);
    }
    pub fn find_by_id(&self, _id: crate::codec_id::CodecId) -> Option<&dyn Codec> {
        None
    }
    pub fn find_by_name(&self, _name: &str) -> Option<&dyn Codec> {
        None
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
    tracing::info!("No built-in codecs registered yet");
}
