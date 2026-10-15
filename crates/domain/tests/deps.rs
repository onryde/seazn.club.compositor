//! Guard: the domain crate links nothing that does I/O, directly or transitively.
//!
//! Read from the resolved graph (`support/graph.rs`), so a workspace-inherited, renamed,
//! target-specific, optional, build-time or transitive dependency is seen under its real package
//! name. Dev-dependencies are exempt: tests may do I/O (this one runs `cargo metadata`).
mod support {
    pub mod graph;
}
use support::graph::{Graph, Kind};

const FORBIDDEN_PREFIXES: &[&str] = &[
    "gstreamer",
    "gst",
    "glib",
    "cef",
    "ureq",
    "reqwest",
    "hyper",
    "tokio",
    "tiny_http",
    "crossbeam",
    "cairo",
];

#[test]
fn domain_has_no_io_dependencies() {
    let graph = Graph::load();
    // Anti-vacuity: the domain has no dependencies today, so its closure is legitimately empty.
    // Prove the traversal itself reaches through the graph: the binary must reach the domain.
    let binary = graph.closure("compositor", &[Kind::Normal, Kind::Build]);
    assert!(
        binary.contains("domain"),
        "graph traversal is broken: compositor does not reach domain"
    );
    let linked = graph.closure("domain", &[Kind::Normal, Kind::Build]);
    let bad: Vec<&String> = linked
        .iter()
        .filter(|n| FORBIDDEN_PREFIXES.iter().any(|p| n.starts_with(p)))
        .collect();
    eprintln!(
        "deps guard checked {} packages in domain's normal+build closure (traversal sanity: compositor reaches {})",
        linked.len(),
        binary.len()
    );
    assert!(
        bad.is_empty(),
        "domain must not link I/O crates, directly or transitively: {bad:?}"
    );
}
