//! Guard: the domain crate's source never reaches for the clock, env, filesystem, network, threads
//! or OS, and never pulls in source from outside `domain/src` where this scan cannot see it.
use std::fs;
use std::path::{Path, PathBuf};

/// Paths and macros refused anywhere in code (comments and string contents are not code).
const FORBIDDEN: &[&str] = &[
    "SystemTime::now",
    "Instant::now",
    "std::env",
    "std::fs",
    "std::net",
    "std::os",
    "std::thread",
    "std::process",
    // A glob of std brings `fs`, `env`, … into scope under their bare names.
    "std::*",
    "env!(",
    "option_env!(",
    // Splices a file this scan never reads.
    "include!",
];

/// Clock types refused as bare names, so an alias (`use std::time::Instant as Clock`) cannot hide
/// them. Time is `Ms`, passed in.
const BARE_NAMES: &[&str] = &["Instant", "SystemTime"];

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

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Where the raw string starting at `i` (`r"…"`, `r#"…"#`, `br"…"`) ends, if one starts there.
fn raw_string_end(c: &[char], i: usize) -> Option<usize> {
    let prefix_ok = match i.checked_sub(1).map(|p| c[p]) {
        None => true,
        Some('b') => i < 2 || !is_ident(c[i - 2]),
        Some(p) => !is_ident(p),
    };
    if c[i] != 'r' || !prefix_ok {
        return None;
    }
    let hashes = c[i + 1..].iter().take_while(|&&h| h == '#').count();
    let open = i + 1 + hashes;
    if c.get(open) != Some(&'"') {
        return None;
    }
    let mut j = open + 1;
    while j < c.len() {
        if c[j] == '"' && c[j + 1..].iter().take(hashes).filter(|&&h| h == '#').count() == hashes {
            return Some(j + 1 + hashes);
        }
        j += 1;
    }
    Some(c.len())
}

/// The source with comments removed and every string and char literal emptied. Newlines are kept,
/// so line numbers survive. `//` inside a string is not a comment, block comments nest, and a
/// lifetime (`'a`) is not a char literal.
fn code_only(text: &str) -> String {
    let c: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let keep_newlines = |out: &mut String, s: &[char]| out.extend(s.iter().filter(|&&ch| ch == '\n'));
    let mut i = 0;
    while i < c.len() {
        let next = c.get(i + 1).copied();
        if c[i] == '/' && next == Some('/') {
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
        } else if c[i] == '/' && next == Some('*') {
            let (start, mut depth) = (i, 0usize);
            while i < c.len() {
                if c[i] == '/' && c.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if c[i] == '*' && c.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            keep_newlines(&mut out, &c[start..i.min(c.len())]);
        } else if let Some(end) = raw_string_end(&c, i) {
            keep_newlines(&mut out, &c[i..end]);
            out.push_str("\"\"");
            i = end;
        } else if c[i] == '"' {
            let start = i;
            i += 1;
            while i < c.len() && c[i] != '"' {
                i += if c[i] == '\\' { 2 } else { 1 };
            }
            i += 1;
            keep_newlines(&mut out, &c[start..i.min(c.len())]);
            out.push_str("\"\"");
        } else if c[i] == '\'' && next == Some('\\') {
            i += 3;
            while i < c.len() && c[i] != '\'' {
                i += 1;
            }
            i += 1;
            out.push_str("''");
        } else if c[i] == '\'' && c.get(i + 2) == Some(&'\'') {
            i += 3;
            out.push_str("''");
        } else {
            out.push(c[i]);
            i += 1;
        }
    }
    out
}

/// Identifiers in order.
fn idents(code: &str) -> Vec<&str> {
    code.split(|c: char| !is_ident(c))
        .filter(|t| !t.is_empty())
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

/// Whether any attribute (`#[…]`, `#![…]`, `cfg_attr` included) sets `path = …`, which loads a
/// module file from outside `domain/src`.
fn has_path_attribute(compact: &str) -> bool {
    let mut rest = compact;
    while let Some(at) = rest.find('#') {
        rest = &rest[at + 1..];
        let body = rest.strip_prefix('!').unwrap_or(rest);
        let Some(body) = body.strip_prefix('[') else {
            continue;
        };
        let mut depth = 1usize;
        let end = body
            .char_indices()
            .find(|&(_, ch)| {
                match ch {
                    '[' => depth += 1,
                    ']' => depth -= 1,
                    _ => {}
                }
                depth == 0
            })
            .map_or(body.len(), |(j, _)| j);
        let attr = &body[..end];
        let sets_path = attr
            .match_indices("path=")
            .any(|(j, _)| attr[..j].chars().next_back().is_none_or(|p| !is_ident(p)));
        if sets_path {
            return true;
        }
    }
    false
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
        let code = code_only(&fs::read_to_string(f).expect("read source"));
        let mut seen = Vec::new();
        for (i, line) in code.lines().enumerate() {
            for bad in FORBIDDEN.iter().filter(|bad| line.contains(*bad)) {
                hits.push(format!("{}:{}: {bad}", f.display(), i + 1));
                seen.push(*bad);
            }
            for name in idents(line).into_iter().filter(|t| BARE_NAMES.contains(t)) {
                hits.push(format!(
                    "{}:{}: `{name}` (a clock type; time is `Ms`)",
                    f.display(),
                    i + 1
                ));
            }
        }
        // Second pass for what no single line spells out: a path split across lines or spaced
        // out, a grouped import, a `std as` alias, a `path` attribute.
        let compact: String = code.chars().filter(|c| !c.is_whitespace()).collect();
        let mut haystacks = expand_std_groups(&compact);
        haystacks.push(compact.clone());
        for bad in FORBIDDEN.iter().filter(|bad| !seen.contains(*bad)) {
            if haystacks.iter().any(|h| h.contains(bad)) {
                hits.push(format!(
                    "{}: {bad} (split across lines or inside a `std::{{…}}` group)",
                    f.display()
                ));
            }
        }
        if idents(&code).windows(2).any(|w| w == ["std", "as"]) {
            hits.push(format!(
                "{}: `std as …` (an alias of std hides every path above)",
                f.display()
            ));
        }
        if has_path_attribute(&compact) {
            hits.push(format!(
                "{}: `#[path = …]` (loads a module from outside domain/src)",
                f.display()
            ));
        }
    }
    assert!(hits.is_empty(), "domain must stay pure:\n{}", hits.join("\n"));
}
