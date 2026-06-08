use crate::buffer::{BufferSink, BufferSrc};
use crate::filter::{Filter, FilterContext};
use rsmpeg_util::RsResult;
use std::collections::HashMap;

/// A link between two filter pads in the filter graph.
#[derive(Debug, Clone)]
pub struct FilterLink {
    pub src_filter_index: usize,
    pub src_pad: usize,
    pub dst_filter_index: usize,
    pub dst_pad: usize,
}

/// Directed acyclic graph (DAG) of filter instances.
pub struct FilterGraph {
    pub filters: Vec<FilterContext>,
    pub links: Vec<FilterLink>,
    pub named_nodes: HashMap<String, usize>,
}

impl FilterGraph {
    pub fn new() -> Self {
        FilterGraph {
            filters: Vec::new(),
            links: Vec::new(),
            named_nodes: HashMap::new(),
        }
    }

    pub fn add_filter(&mut self, name: &str, filter: Box<dyn Filter>) -> usize {
        let index = self.filters.len();
        self.named_nodes.insert(name.to_string(), index);
        self.filters.push(FilterContext::new(filter));
        index
    }

    pub fn link(
        &mut self,
        src_idx: usize,
        src_pad: usize,
        dst_idx: usize,
        dst_pad: usize,
    ) -> RsResult<()> {
        // Mark pads as connected
        if let Some(src) = self.filters.get_mut(src_idx) {
            if src_pad < src.outputs.len() {
                src.outputs[src_pad].connected = true;
            }
        }
        if let Some(dst) = self.filters.get_mut(dst_idx) {
            if dst_pad < dst.inputs.len() {
                dst.inputs[dst_pad].connected = true;
            }
        }

        self.links.push(FilterLink {
            src_filter_index: src_idx,
            src_pad,
            dst_filter_index: dst_idx,
            dst_pad,
        });
        Ok(())
    }

    pub fn add_buffer_src(&mut self) -> usize {
        let index = self.filters.len();
        self.filters
            .push(FilterContext::new(Box::new(BufferSrc::new())));
        index
    }

    pub fn add_buffer_sink(&mut self) -> usize {
        let index = self.filters.len();
        self.filters
            .push(FilterContext::new(Box::new(BufferSink::new())));
        index
    }

    pub fn nb_filters(&self) -> usize {
        self.filters.len()
    }
    pub fn nb_links(&self) -> usize {
        self.links.len()
    }

    /// Validate that all pads are connected properly.
    pub fn validate(&self) -> RsResult<()> {
        for (idx, fctx) in self.filters.iter().enumerate() {
            for (p_idx, pad) in fctx.inputs.iter().enumerate() {
                if !pad.connected {
                    tracing::warn!(
                        "Filter[{}] ({}) input pad[{}] '{}' is not connected",
                        idx,
                        fctx.name(),
                        p_idx,
                        pad.pad_name
                    );
                }
            }
        }
        Ok(())
    }

    /// Dump the graph structure for debugging.
    pub fn dump(&self) -> String {
        let mut s = String::new();
        s.push_str("FilterGraph:\n");
        for (idx, fctx) in self.filters.iter().enumerate() {
            s.push_str(&format!("  [{}] {}\n", idx, fctx.name()));
            for (p, l) in fctx.inputs.iter().enumerate() {
                s.push_str(&format!(
                    "    input {}: {} (connected: {})\n",
                    p, l.pad_name, l.connected
                ));
            }
            for (p, l) in fctx.outputs.iter().enumerate() {
                s.push_str(&format!(
                    "    output {}: {} (connected: {})\n",
                    p, l.pad_name, l.connected
                ));
            }
        }
        s.push_str("  Links:\n");
        for link in &self.links {
            s.push_str(&format!(
                "    [{}]:{} -> [{}]:{}\n",
                link.src_filter_index, link.src_pad, link.dst_filter_index, link.dst_pad
            ));
        }
        s
    }
}

impl Default for FilterGraph {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin::{NullFilter, ScaleFilter};

    #[test]
    fn test_empty_graph() {
        let g = FilterGraph::new();
        assert_eq!(g.nb_filters(), 0);
        assert_eq!(g.nb_links(), 0);
    }

    #[test]
    fn test_add_filters() {
        let mut g = FilterGraph::new();
        g.add_filter("src", Box::new(crate::buffer::BufferSrc::new()));
        g.add_filter("null", Box::new(NullFilter));
        g.add_filter("sink", Box::new(crate::buffer::BufferSink::new()));
        assert_eq!(g.nb_filters(), 3);
    }

    #[test]
    fn test_simple_graph() {
        let mut g = FilterGraph::new();
        let src = g.add_filter("src", Box::new(crate::buffer::BufferSrc::new()));
        let scale = g.add_filter(
            "scale",
            Box::new(ScaleFilter {
                width: 640,
                height: 480,
            }),
        );
        let sink = g.add_filter("sink", Box::new(crate::buffer::BufferSink::new()));
        g.link(src, 0, scale, 0).unwrap();
        g.link(scale, 0, sink, 0).unwrap();
        assert_eq!(g.nb_links(), 2);
    }

    #[test]
    fn test_dump() {
        let mut g = FilterGraph::new();
        let src = g.add_filter("src", Box::new(crate::buffer::BufferSrc::new()));
        let sink = g.add_filter("sink", Box::new(crate::buffer::BufferSink::new()));
        g.link(src, 0, sink, 0).unwrap();
        let dump = g.dump();
        assert!(dump.contains("buffer"));
        assert!(dump.contains("buffersink"));
    }
}
