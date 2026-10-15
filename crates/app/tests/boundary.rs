//! Guard: the application layer never depends on, or names, the adapters crate.
use std::fs;
use std::path::{Path, PathBuf};

const DEPENDENCY_SECTIONS: &[&str] = &["dependencies", "dev-dependencies", "build-dependencies"];

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

/// Every dependency table in the manifest, named for the failure message: the three top-level
/// sections and their `[target.'cfg(…)'.…]` twins. A dev-dependency on adapters is no cargo cycle
/// error (cargo allows dev-dependency cycles), so this test is the only thing that refuses it.
fn dependency_tables(doc: &toml::Table) -> Vec<(String, toml::Table)> {
    let mut scopes = vec![(String::new(), doc)];
    if let Some(targets) = doc.get("target").and_then(|t| t.as_table()) {
        for (cfg, body) in targets {
            if let Some(body) = body.as_table() {
                scopes.push((format!("target.{cfg}."), body));
            }
        }
    }
    let mut tables = Vec::new();
    for (prefix, scope) in scopes {
        for section in DEPENDENCY_SECTIONS {
            if let Some(t) = scope.get(*section).and_then(|d| d.as_table()) {
                tables.push((format!("{prefix}{section}"), t.clone()));
            }
        }
    }
    tables
}

#[test]
fn app_manifest_has_no_adapters_dependency() {
    let text = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).expect("manifest");
    let doc: toml::Table = text.parse().expect("toml");
    let package = doc
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str());
    assert_eq!(package, Some("app"), "the boundary guard read the wrong manifest");
    let tables = dependency_tables(&doc);
    let mut checked = 0;
    for (section, deps) in &tables {
        for (key, v) in deps {
            checked += 1;
            // A renamed dependency (`live = { package = "adapters" }`) is caught by its real package name.
            let package = v
                .as_table()
                .and_then(|t| t.get("package"))
                .and_then(|p| p.as_str());
            assert!(
                key != "adapters" && package != Some("adapters"),
                "app/{section} must not include adapters (entry `{key}`)"
            );
        }
    }
    eprintln!(
        "boundary guard checked {checked} dependency entries in {} tables",
        tables.len()
    );
    assert!(
        checked > 0,
        "boundary guard checked 0 dependency entries: app depends on domain at least"
    );
}

#[test]
fn app_source_never_names_adapters() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    eprintln!("boundary scan checked {} files", files.len());
    assert!(!files.is_empty(), "boundary scan checked 0 files");
    for f in &files {
        let text = fs::read_to_string(f).expect("read");
        assert!(!text.contains("adapters::"), "{} names adapters::", f.display());
    }
}
