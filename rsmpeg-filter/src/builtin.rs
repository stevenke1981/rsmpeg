use crate::filter::Filter;
use crate::pad::Pad;

/// Scale video filter — changes frame dimensions.
pub struct ScaleFilter {
    pub width: u32,
    pub height: u32,
}

impl Filter for ScaleFilter {
    fn name(&self) -> &'static str {
        "scale"
    }
    fn description(&self) -> &'static str {
        "Scale video frames to specified dimensions"
    }
    fn inputs(&self) -> Vec<Pad> {
        vec![Pad::input("default", rsmpeg_util::MediaType::Video)]
    }
    fn outputs(&self) -> Vec<Pad> {
        vec![Pad::output("default", rsmpeg_util::MediaType::Video)]
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Trim filter — passes frames within a time window.
pub struct TrimFilter {
    pub start_pts: i64,
    pub end_pts: i64,
}

impl Filter for TrimFilter {
    fn name(&self) -> &'static str {
        "trim"
    }
    fn description(&self) -> &'static str {
        "Pass frames whose PTS falls within [start, end)"
    }
    fn inputs(&self) -> Vec<Pad> {
        vec![Pad::input("default", rsmpeg_util::MediaType::Video)]
    }
    fn outputs(&self) -> Vec<Pad> {
        vec![Pad::output("default", rsmpeg_util::MediaType::Video)]
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Null filter — passthrough (used for debugging or graph structure).
pub struct NullFilter;

impl Filter for NullFilter {
    fn name(&self) -> &'static str {
        "null"
    }
    fn description(&self) -> &'static str {
        "Passthrough (no-op)"
    }
    fn inputs(&self) -> Vec<Pad> {
        vec![Pad::input("default", rsmpeg_util::MediaType::Video)]
    }
    fn outputs(&self) -> Vec<Pad> {
        vec![Pad::output("default", rsmpeg_util::MediaType::Video)]
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Overlay filter — composite one video onto another.
pub struct OverlayFilter {
    pub x: i32,
    pub y: i32,
}

impl Filter for OverlayFilter {
    fn name(&self) -> &'static str {
        "overlay"
    }
    fn description(&self) -> &'static str {
        "Overlay one video on top of another"
    }
    fn inputs(&self) -> Vec<Pad> {
        vec![
            Pad::input("main", rsmpeg_util::MediaType::Video),
            Pad::input("overlay", rsmpeg_util::MediaType::Video),
        ]
    }
    fn outputs(&self) -> Vec<Pad> {
        vec![Pad::output("default", rsmpeg_util::MediaType::Video)]
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Transpose filter — rotate/flip video.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransposeDir {
    Clockwise,
    CounterClockwise,
    ClockwiseFlip,
    CounterClockwiseFlip,
}

pub struct TransposeFilter {
    pub direction: TransposeDir,
}

impl Filter for TransposeFilter {
    fn name(&self) -> &'static str {
        "transpose"
    }
    fn description(&self) -> &'static str {
        "Transpose (rotate/flip) video frames"
    }
    fn inputs(&self) -> Vec<Pad> {
        vec![Pad::input("default", rsmpeg_util::MediaType::Video)]
    }
    fn outputs(&self) -> Vec<Pad> {
        vec![Pad::output("default", rsmpeg_util::MediaType::Video)]
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
