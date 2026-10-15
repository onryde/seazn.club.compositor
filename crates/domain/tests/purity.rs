//! Guard: the domain crate's source never reaches for the clock, env, filesystem, network or threads.
use std::fs;
use std::path::{Path, PathBuf};

const FORBIDDEN: &[&str] = &[
    "SystemTime::now",
    "Instant::now",
    "std::env",
    "std::fs",
    "std::net",
    "std::thread",
    "env!(",
    "option_env!(",
    "std::process",
];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read_dir domain/src") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The file with `//` comments dropped and every whitespace character removed, so `std :: fs` and a
/// `use` split over several lines read the same as their one-line spelling.
fn compact_code(text: &str) -> String {
    text.lines()
        .map(|line| line.split("//").next().unwrap_or(""))
        .flat_map(str::chars)
        .filter(|c| !c.is_whitespace())
        .collect()
}

/// `use std::{fs, env};` never spells `std::fs`. Expand each `std::{…}` group in compacted code into
/// one `std::<item>` path per top-level item, so the FORBIDDEN needles see it.
fn expand_std_groups(compact: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut rest = compact;
    while let Some(at) = rest.find("std::{") {
        rest = &rest[at + "std::{".len()..];
        let mut depth = 1usize;
        let mut item = String::new();
        for ch in rest.chars() {
            match ch {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
            if depth == 0 || (depth == 1 && ch == ',') {
                paths.push(format!("std::{item}"));
                item.clear();
                if depth == 0 {
                    break;
                }
            } else {
                item.push(ch);
            }
        }
    }
    paths
}

#[test]
fn domain_source_has_no_clock_env_fs_net_thread_or_process() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    eprintln!("purity scan checked {} files", files.len());
    assert!(
        !files.is_empty(),
        "purity scan checked 0 files: the scan itself is broken"
    );
    let mut hits = Vec::new();
    for f in &files {
        let text = fs::read_to_string(f).expect("read source");
        let mut seen = Vec::new();
        for (i, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            for bad in FORBIDDEN {
                if code.contains(bad) {
                    hits.push(format!("{}:{}: {bad}", f.display(), i + 1));
                    seen.push(*bad);
                }
            }
        }
        // Second pass for what no single line spells out: a `use` split over lines, or a grouped import.
        let compact = compact_code(&text);
        let mut haystacks = expand_std_groups(&compact);
        haystacks.push(compact);
        for bad in FORBIDDEN.iter().filter(|bad| !seen.contains(*bad)) {
            if haystacks.iter().any(|h| h.contains(bad)) {
                hits.push(format!(
                    "{}: {bad} (split across lines or inside a `std::{{…}}` group)",
                    f.display()
                ));
            }
        }
    }
    assert!(hits.is_empty(), "domain must stay pure:\n{}", hits.join("\n"));
}
