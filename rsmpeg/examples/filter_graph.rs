//! Demonstrate constructing and dumping a filter graph.

fn main() {
    let mut graph = rsmpeg::filter::FilterGraph::new();
    let src = graph.add_filter("src", Box::new(rsmpeg::filter::buffer::BufferSrc::new()));
    let scale = graph.add_filter(
        "scale",
        Box::new(rsmpeg::filter::builtin::ScaleFilter {
            width: 640,
            height: 480,
        }),
    );
    let sink = graph.add_filter("sink", Box::new(rsmpeg::filter::buffer::BufferSink::new()));
    graph.link(src, 0, scale, 0).unwrap();
    graph.link(scale, 0, sink, 0).unwrap();
    graph.validate().unwrap();
    println!("{}", graph.dump());
}
