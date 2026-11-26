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
            .find(|m| m.name() == name || m.extensions().contains(&name))
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
    use crate::muxers::*;
    // Recover from a poisoned lock (another thread panicked while holding it)
    // instead of aborting start-up.
    let mut registry = match global_format_registry().write() {
        Ok(g) => g,
        Err(poisoned) => {
            tracing::error!("format registry lock poisoned; recovering to finish registration");
            poisoned.into_inner()
        }
    };

    // Make repeated calls harmless instead of stacking duplicate formats.
    if registry.demuxers().is_empty() {
        registry.register_demuxer(Box::new(MP4Demuxer::default()));
        registry.register_demuxer(Box::new(MKVDemuxer::default()));
        registry.register_demuxer(Box::new(AVIDemuxer::default()));
        registry.register_demuxer(Box::new(FLACDemuxer::default()));
        registry.register_demuxer(Box::new(WAVDemuxer::default()));
        registry.register_demuxer(Box::new(RawVideoDemuxer));
        registry.register_demuxer(Box::new(Y4mDemuxer::default()));
        tracing::info!("Registered 7 built-in demuxers");
    } else {
        tracing::debug!("built-in demuxers already registered; skipping");
    }

    if registry.muxers().is_empty() {
        registry.register_muxer(Box::new(WAVMuxer::default()));
        registry.register_muxer(Box::new(Y4mMuxer::default()));
        tracing::info!("Registered 2 built-in muxers");
    } else {
        tracing::debug!("built-in muxers already registered; skipping");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demuxers::Y4mDemuxer;
    use crate::muxers::{WAVMuxer, Y4mMuxer};

    #[test]
    fn find_muxer_matches_by_name_and_extension() {
        let mut reg = FormatRegistry::new();
        reg.register_muxer(Box::new(WAVMuxer::default()));
        reg.register_muxer(Box::new(Y4mMuxer::default()));
        assert_eq!(reg.find_muxer("wav").unwrap().name(), "wav");
        assert_eq!(reg.find_muxer("y4m").unwrap().name(), "y4m");
        assert!(reg.find_muxer("mp4").is_none());
    }

    #[test]
    fn register_builtin_formats_populates_demuxers_and_muxers_idempotently() {
        register_builtin_formats();
        register_builtin_formats();
        let reg = global_format_registry().read().unwrap();
        assert_eq!(reg.demuxers().len(), 7);
        assert_eq!(reg.muxers().len(), 2);
        assert!(reg.find_demuxer("y4m").is_some());
        assert!(reg.find_muxer("wav").is_some());
        assert!(reg.find_muxer("y4m").is_some());
        assert_eq!(
            reg.probe_demuxer(b"YUV4MPEG2 W8 H8\n").map(|d| d.name()),
            Some("y4m")
        );
        let _ = Y4mDemuxer::default();
    }
}
