//! Guard: the application layer never depends on, or names, the adapters crate.
use std::fs;
use std::path::{Path, PathBuf};

#[path = "../../domain/tests/support/graph.rs"]
mod graph;
use graph::{Graph, Kind};

/// The crate app must never reach. The source scan builds its patterns from this at run time, so
/// this file's own text never matches them.
const CRATE: &str = "adapters";
/// Every directory cargo compiles app code from, plus `build.rs`.
const SOURCE_DIRS: &[&str] = &["src", "tests", "benches", "examples"];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read_dir") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Identifiers, `::`, and single punctuation characters, in order, each with its line number.
/// Comments and strings are NOT skipped: a comment that names the crate's path is a finding here too.
fn tokens(text: &str) -> Vec<(usize, String)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let (mut i, mut line) = (0, 1);
    while i < chars.len() {
        let c = chars[i];
        if c.is_alphanumeric() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            out.push((line, chars[start..i].iter().collect()));
        } else if c == ':' && chars.get(i + 1) == Some(&':') {
            out.push((line, "::".to_string()));
            i += 2;
        } else {
            if c == '\n' {
                line += 1;
            } else if !c.is_whitespace() {
                out.push((line, c.to_string()));
            }
            i += 1;
        }
    }
    out
}

#[test]
fn app_dependency_graph_has_no_adapters() {
    let graph = Graph::load();
    // Every kind at app itself: a dev-dependency on adapters builds (cargo allows dev cycles), so
    // only this refuses it. Past app, normal and build edges, e.g. a testkit that took adapters.
    let reached = graph.closure("app", &[Kind::Normal, Kind::Build, Kind::Dev]);
    eprintln!(
        "boundary guard checked {} packages in app's closure of every kind",
        reached.len()
    );
    assert!(
        reached.contains("domain"),
        "graph traversal is broken: app does not reach domain"
    );
    assert!(
        !reached.contains(CRATE),
        "app must not reach {CRATE}, directly or transitively"
    );
}

#[test]
fn app_sources_never_name_adapters() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for dir in SOURCE_DIRS {
        let dir = root.join(dir);
        if dir.is_dir() {
            rust_files(&dir, &mut files);
        }
    }
    let build = root.join("build.rs");
    if build.is_file() {
        files.push(build);
    }
    eprintln!(
        "boundary scan checked {} files under {SOURCE_DIRS:?} and build.rs",
        files.len()
    );
    // Anti-vacuity: the scan must cover tests/, which this very file proves by being in it.
    assert!(
        files.iter().any(|f| f.ends_with("tests/boundary.rs")),
        "boundary scan did not reach tests/ (checked {} files)",
        files.len()
    );
    let mut hits = Vec::new();
    for f in &files {
        let toks = tokens(&fs::read_to_string(f).expect("read"));
        let tok = |j: Option<usize>| j.and_then(|j| toks.get(j)).map(|(_, t)| t.as_str());
        for (i, (line, t)) in toks.iter().enumerate() {
            let names_crate = t == CRATE
                && (tok(Some(i + 1)) == Some("::")
                    || tok(i.checked_sub(1)) == Some("::")
                    || tok(i.checked_sub(1)) == Some("use")
                    || (tok(i.checked_sub(1)) == Some("crate") && tok(i.checked_sub(2)) == Some("extern")));
            if names_crate {
                hits.push(format!("{}:{line}", f.display()));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "app code names the {CRATE} crate:\n{}",
        hits.join("\n")
    );
}
