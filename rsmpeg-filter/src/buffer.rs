use crate::filter::Filter;

/// A source filter that yields pre-inserted frames (for graph feeding).
pub struct BufferSrc {
    pub name: &'static str,
}

impl Default for BufferSrc {
    fn default() -> Self {
        Self::new()
    }
}

impl BufferSrc {
    pub fn new() -> Self {
        BufferSrc { name: "buffer" }
    }
}

impl Filter for BufferSrc {
    fn name(&self) -> &'static str {
        "buffer"
    }
    fn description(&self) -> &'static str {
        "Buffer source: feed frames into filter graph"
    }
    fn inputs(&self) -> Vec<crate::pad::Pad> {
        vec![]
    }
    fn outputs(&self) -> Vec<crate::pad::Pad> {
        vec![crate::pad::Pad::output(
            "default",
            rsmpeg_util::MediaType::Video,
        )]
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// A sink filter that collects output frames.
pub struct BufferSink {
    pub name: &'static str,
    pub frames: Vec<rsmpeg_codec::Frame>,
}

impl Default for BufferSink {
    fn default() -> Self {
        Self::new()
    }
}

impl BufferSink {
    pub fn new() -> Self {
        BufferSink {
            name: "buffersink",
            frames: Vec::new(),
        }
    }
    pub fn frames(&self) -> &[rsmpeg_codec::Frame] {
        &self.frames
    }
    pub fn take_frames(&mut self) -> Vec<rsmpeg_codec::Frame> {
        std::mem::take(&mut self.frames)
    }
}

impl Filter for BufferSink {
    fn name(&self) -> &'static str {
        "buffersink"
    }
    fn description(&self) -> &'static str {
        "Buffer sink: collect output frames from filter graph"
    }
    fn inputs(&self) -> Vec<crate::pad::Pad> {
        vec![crate::pad::Pad::input(
            "default",
            rsmpeg_util::MediaType::Video,
        )]
    }
    fn outputs(&self) -> Vec<crate::pad::Pad> {
        vec![]
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
