//! Guard: the domain crate depends on nothing that does I/O.
//!
//! Every table that links code into the domain is checked: `[dependencies]`, `[build-dependencies]`
//! and their `[target.'cfg(…)'.…]` twins. `[dev-dependencies]` is exempt, because tests may do I/O
//! (this file reads the manifest).
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

const LINKED_SECTIONS: &[&str] = &["dependencies", "build-dependencies"];

/// Every dependency table that links into the domain, top-level and per target.
fn linked_dependency_tables(doc: &toml::Table) -> Vec<toml::Table> {
    let mut scopes = vec![doc];
    if let Some(targets) = doc.get("target").and_then(|t| t.as_table()) {
        scopes.extend(targets.values().filter_map(|cfg| cfg.as_table()));
    }
    scopes
        .into_iter()
        .flat_map(|scope| {
            LINKED_SECTIONS
                .iter()
                .filter_map(|s| scope.get(*s).and_then(|d| d.as_table()))
        })
        .cloned()
        .collect()
}

#[test]
fn domain_has_no_io_dependencies() {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).expect("manifest");
    let doc: toml::Table = text.parse().expect("toml");
    let package = doc
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str());
    assert_eq!(package, Some("domain"), "the deps guard read the wrong manifest");
    let tables = linked_dependency_tables(&doc);
    let mut names: Vec<String> = Vec::new();
    for deps in &tables {
        names.extend(deps.keys().cloned());
        // A renamed dependency (`gst = { package = "gstreamer" }`) is checked by its real package name too.
        for v in deps.values() {
            if let Some(pkg) = v
                .as_table()
                .and_then(|t| t.get("package"))
                .and_then(|p| p.as_str())
            {
                names.push(pkg.to_string());
            }
        }
    }
    let bad: Vec<&String> = names
        .iter()
        .filter(|n| FORBIDDEN_PREFIXES.iter().any(|p| n.starts_with(p)))
        .collect();
    eprintln!(
        "deps guard checked {} dependency names in {} tables",
        names.len(),
        tables.len()
    );
    assert!(bad.is_empty(), "domain must not depend on I/O crates: {bad:?}");
}
