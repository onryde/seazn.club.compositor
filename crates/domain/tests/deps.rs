//! Guard: the domain crate links nothing that does I/O, directly or transitively.
//!
//! Read from the resolved graph (`support/graph.rs`), so a workspace-inherited, renamed,
//! target-specific, optional, build-time or transitive dependency is seen under its real package
//! name. Dev-dependencies are exempt: tests may do I/O (this one runs `cargo metadata`).
mod support {
    pub mod graph;
}
use std::collections::BTreeSet;
use support::graph::{Graph, Kind};

/// A hand-written `cargo metadata` document with every edge kind at the root (`r`) and one hop past
/// it (`n`, `d`), so the kind handling is pinned without planting anything in the real workspace.
fn fixture() -> Graph {
    let edge = |pkg: &str, kinds: &[Option<&str>]| {
        let kinds: Vec<_> = kinds
            .iter()
            .map(|k| serde_json::json!({ "kind": k, "target": null }))
            .collect();
        serde_json::json!({ "name": pkg, "pkg": pkg, "dep_kinds": kinds })
    };
    let node = |id: &str, deps: Vec<serde_json::Value>| serde_json::json!({ "id": id, "deps": deps });
    let names = ["r", "n", "b", "d", "multi", "t", "nn", "nb", "nd", "dn", "dd"];
    let mut nodes = vec![
        node(
            "r",
            vec![
                edge("n", &[None]),
                edge("b", &[Some("build")]),
                edge("d", &[Some("dev")]),
                edge("multi", &[Some("dev"), Some("build")]),
                edge("t", &[None]),
            ],
        ),
        node(
            "n",
            vec![
                edge("nn", &[None]),
                edge("nb", &[Some("build")]),
                edge("nd", &[Some("dev")]),
            ],
        ),
        node("d", vec![edge("dn", &[None]), edge("dd", &[Some("dev")])]),
    ];
    nodes.extend(["b", "multi", "t", "nn", "nb", "nd", "dn", "dd"].map(|id| node(id, vec![])));
    Graph::from_metadata(&serde_json::json!({
        "packages": names.map(|n| serde_json::json!({ "id": n, "name": n })),
        "workspace_members": ["r", "n", "d"],
        "resolve": { "nodes": nodes },
    }))
}

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|n| n.to_string()).collect()
}

/// Expected sets follow cargo's rule, not this code: the root's own edges count when their kind is
/// asked for, and a dependency's dev-dependencies are never built for its dependents (`nd`, `dd`).
/// A build-dependency past the root (`nb`) is built, so it counts.
#[test]
fn graph_closure_follows_edge_kinds_at_and_past_the_root() {
    let g = fixture();
    let cases: [(&[Kind], &[&str]); 3] = [
        (&[Kind::Normal], &["n", "nb", "nn", "t"]),
        (
            &[Kind::Normal, Kind::Build],
            &["b", "multi", "n", "nb", "nn", "t"],
        ),
        (
            &[Kind::Normal, Kind::Build, Kind::Dev],
            &["b", "d", "dn", "multi", "n", "nb", "nn", "t"],
        ),
    ];
    for (kinds, expected) in &cases {
        assert_eq!(
            g.closure("r", kinds),
            set(expected),
            "closure of r over {kinds:?}"
        );
    }
    eprintln!("graph fixture checked {} closures", cases.len());
}

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
