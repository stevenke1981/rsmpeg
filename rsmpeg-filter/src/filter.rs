use crate::pad::Pad;
use rsmpeg_codec::Frame;
use std::any::Any;

/// Result of a filter's process_frame call.
#[derive(Debug)]
pub enum FilterResult {
    /// Filter produced a frame (or passthrough).
    Frame(Frame),
    /// Filter needs more input before producing output.
    NeedMoreInput,
    /// Filter is done (EOF on this link).
    Done,
}

/// A filter instance with its internal state.
pub trait Filter: Send {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn inputs(&self) -> Vec<Pad>;
    fn outputs(&self) -> Vec<Pad>;
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

/// Mutable filter context wrapping a concrete filter.
pub struct FilterContext {
    pub filter: Box<dyn Filter>,
    pub inputs: Vec<FilterLinkState>,
    pub outputs: Vec<FilterLinkState>,
    pub metadata: std::collections::HashMap<String, String>,
}

/// State of a single filter link.
#[derive(Debug)]
pub struct FilterLinkState {
    pub pad_name: String,
    pub connected: bool,
}

impl FilterContext {
    pub fn new(filter: Box<dyn Filter>) -> Self {
        let inputs = filter
            .inputs()
            .iter()
            .map(|p| FilterLinkState {
                pad_name: p.name.clone(),
                connected: false,
            })
            .collect();
        let outputs = filter
            .outputs()
            .iter()
            .map(|p| FilterLinkState {
                pad_name: p.name.clone(),
                connected: false,
            })
            .collect();

        FilterContext {
            filter,
            inputs,
            outputs,
            metadata: std::collections::HashMap::new(),
        }
    }

    pub fn name(&self) -> &'static str {
        self.filter.name()
    }
    pub fn description(&self) -> &'static str {
        self.filter.description()
    }
}
