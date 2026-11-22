use crate::format::{InputFormat, OutputFormat};
use std::sync::{OnceLock, RwLock};

pub struct FormatRegistry {
    demuxers: Vec<Box<dyn InputFormat>>,
    muxers: Vec<Box<dyn OutputFormat>>,
}

impl FormatRegistry {
    pub fn new() -> Self {
        FormatRegistry {
            demuxers: Vec::new(),
            muxers: Vec::new(),
        }
    }

    pub fn register_demuxer(&mut self, fmt: Box<dyn InputFormat>) {
        tracing::debug!("Registering demuxer: {}", fmt.name());
        self.demuxers.push(fmt);
    }

    pub fn register_muxer(&mut self, fmt: Box<dyn OutputFormat>) {
        tracing::debug!("Registering muxer: {}", fmt.name());
        self.muxers.push(fmt);
    }

    pub fn find_demuxer(&self, name: &str) -> Option<&dyn InputFormat> {
        self.demuxers
            .iter()
            .find(|d| d.name() == name)
            .map(|d| d.as_ref())
    }

    pub fn find_muxer(&self, name: &str) -> Option<&dyn OutputFormat> {
        self.muxers
            .iter()
            .find(|m| m.name() == name)
            .map(|m| m.as_ref())
    }

    pub fn demuxers(&self) -> Vec<&dyn InputFormat> {
        self.demuxers.iter().map(|d| d.as_ref()).collect()
    }

    pub fn muxers(&self) -> Vec<&dyn OutputFormat> {
        self.muxers.iter().map(|m| m.as_ref()).collect()
    }

    pub fn probe_demuxer(&self, buf: &[u8]) -> Option<&dyn InputFormat> {
        self.demuxers
            .iter()
            .filter_map(|d| {
                let s = d.probe(buf);
                if s as u8 > 0 {
                    Some((s, d.as_ref()))
                } else {
                    None
                }
            })
            .max_by_key(|(score, _)| *score)
            .map(|(_, d)| d)
    }
}

impl Default for FormatRegistry {
    fn default() -> Self {
        Self::new()
    }
}

pub fn global_format_registry() -> &'static RwLock<FormatRegistry> {
    static REGISTRY: OnceLock<RwLock<FormatRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(FormatRegistry::new()))
}

pub fn register_builtin_formats() {
    use crate::demuxers::*;
    // Recover from a poisoned lock (another thread panicked while holding it)
    // instead of aborting start-up.
    let mut registry = match global_format_registry().write() {
        Ok(g) => g,
        Err(poisoned) => {
            tracing::error!("format registry lock poisoned; recovering to finish registration");
            poisoned.into_inner()
        }
    };

    // Make repeated calls harmless instead of stacking duplicate demuxers.
    if !registry.demuxers().is_empty() {
        tracing::debug!("built-in formats already registered; skipping");
        return;
    }

    registry.register_demuxer(Box::new(MP4Demuxer::default()));
    registry.register_demuxer(Box::new(MKVDemuxer));
    registry.register_demuxer(Box::new(AVIDemuxer));
    registry.register_demuxer(Box::new(FLACDemuxer));
    registry.register_demuxer(Box::new(WAVDemuxer::default()));
    registry.register_demuxer(Box::new(RawVideoDemuxer));
    tracing::info!("Registered 6 built-in demuxers");
}
