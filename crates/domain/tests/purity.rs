//! Guard: the domain crate reaches into `std`, `core` or `alloc` only through an ALLOWLIST of pure
//! modules, and never pulls in source from outside `domain/src` where this scan cannot see it.
use std::fs;
use std::path::{Path, PathBuf};

/// The std/core/alloc modules the domain may name. Everything else is refused, so a module this
/// list never thought of (env, fs, io, net, os, process, sync, thread, time, …) is refused too.
/// Adding one is a decision: its own commit, with the reason.
const ALLOWED_MODULES: &[&str] = &[
    "borrow",
    "cmp",
    "collections",
    "convert",
    "default",
    "fmt",
    "hash",
    "iter",
    "marker",
    "num",
    "ops",
    "option",
    "result",
    "str",
    "string",
    "vec",
];

/// The crates whose paths the allowlist governs.
const STD_ROOTS: &[&str] = &["std", "core", "alloc"];

/// Clock types refused as bare names too, so no import route or alias can bring them in. Time is
/// `Ms`, passed in.
const BARE_NAMES: &[&str] = &["Instant", "SystemTime"];

/// Hash containers seeded from the OS at runtime: iteration order changes run to run, and the
/// domain's decisions must be deterministic. Refused as bare names, so no alias or glob brings them
/// in; `std::collections` itself stays allowed for `BTreeMap` / `BTreeSet`.
const SEEDED_NAMES: &[&str] = &["HashMap", "HashSet", "RandomState"];

/// The crate-root attribute that keeps the domain off stderr (C12 logs there, from adapters and
/// app). It is an attribute, not a `[lints]` entry, because cargo refuses a member's own lint table
/// beside `workspace = true`, and `forbid` so no inner `#[allow]` can lift it.
const STDERR_FORBID: &str = "#![forbid(clippy::print_stderr)]";

/// Macros that read the build environment, or splice in a file this scan never reads.
const FORBIDDEN_MACROS: &[&str] = &["env", "option_env", "include"];

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

/// Identifiers, `::` and single punctuation characters of `code_only` output, each with its line.
/// A raw identifier reads as its plain name (`r#fs` is `fs`).
fn tokens(code: &str) -> Vec<(usize, String)> {
    let c: Vec<char> = code.chars().collect();
    let (mut i, mut line, mut out) = (0, 1, Vec::new());
    while i < c.len() {
        let raw_ident = c[i] == 'r'
            && c.get(i + 1) == Some(&'#')
            && c.get(i + 2).is_some_and(|&n| is_ident(n))
            && (i == 0 || !is_ident(c[i - 1]));
        if raw_ident {
            i += 2;
        } else if is_ident(c[i]) {
            let start = i;
            while i < c.len() && is_ident(c[i]) {
                i += 1;
            }
            out.push((line, c[start..i].iter().collect()));
        } else if c[i] == ':' && c.get(i + 1) == Some(&':') {
            out.push((line, "::".to_string()));
            i += 2;
        } else {
            if c[i] == '\n' {
                line += 1;
            } else if !c[i].is_whitespace() {
                out.push((line, c[i].to_string()));
            }
            i += 1;
        }
    }
    out
}

/// Why the item that starts a `std::{…}` group (at `toks[j]`) is refused, if it is.
fn group_item_refusal(toks: &[(usize, String)], j: usize) -> Option<String> {
    let at = |k: usize| toks.get(k).map(|(_, t)| t.as_str());
    match at(j) {
        Some("self") if at(j + 1) == Some("as") => Some("`self as …` aliases the whole crate".into()),
        Some("self") => None,
        Some(m) if ALLOWED_MODULES.contains(&m) => None,
        Some(m) => Some(format!("`{m}` is not on the allowlist")),
        None => Some("an unterminated group".into()),
    }
}

/// Every refusal in one file's tokens, with its line.
fn refusals(toks: &[(usize, String)]) -> Vec<(usize, String)> {
    let at = |k: usize| toks.get(k).map(|(_, t)| t.as_str());
    let mut out = Vec::new();
    for (k, (line, t)) in toks.iter().enumerate() {
        let t = t.as_str();
        if STD_ROOTS.contains(&t) && at(k + 1) == Some("as") {
            out.push((*line, format!("`{t} as …` (an alias hides every path)")));
        }
        if STD_ROOTS.contains(&t) && at(k + 1) == Some("::") {
            match at(k + 2) {
                Some(m) if ALLOWED_MODULES.contains(&m) => {}
                Some("{") => {
                    // Walk the group: check the first segment of every top-level item.
                    let (mut j, mut depth, mut item_start) = (k + 3, 1usize, true);
                    while j < toks.len() && depth > 0 {
                        if item_start && depth == 1 {
                            if let Some(why) = group_item_refusal(toks, j) {
                                out.push((toks[j].0, format!("`{t}::{{…}}`: {why}")));
                            }
                            item_start = false;
                        }
                        match toks[j].1.as_str() {
                            "{" => depth += 1,
                            "}" => depth -= 1,
                            "," if depth == 1 => item_start = true,
                            _ => {}
                        }
                        j += 1;
                    }
                }
                Some(m) if m.chars().all(is_ident) => {
                    out.push((*line, format!("`{t}::{m}` is not on the allowlist")))
                }
                other => out.push((
                    *line,
                    format!(
                        "`{t}::{}` (a glob or macro splice the scan cannot read)",
                        other.unwrap_or("")
                    ),
                )),
            }
        }
        if BARE_NAMES.contains(&t) {
            out.push((*line, format!("`{t}` (a clock type; time is `Ms`)")));
        }
        if SEEDED_NAMES.contains(&t) {
            out.push((
                *line,
                format!("`{t}` (OS-seeded, nondeterministic order; use BTreeMap / BTreeSet)"),
            ));
        }
        if FORBIDDEN_MACROS.contains(&t) && at(k + 1) == Some("!") {
            out.push((*line, format!("`{t}!`")));
        }
        // `#[path = …]` / `#![…]` / `cfg_attr(…, path = …)` load a module from outside domain/src.
        if t == "#" {
            let open = if at(k + 1) == Some("!") { k + 2 } else { k + 1 };
            if at(open) == Some("[") {
                let mut depth = 0usize;
                for (j, (tline, tok)) in toks.iter().enumerate().skip(open) {
                    match tok.as_str() {
                        "[" => depth += 1,
                        "]" => depth -= 1,
                        "path" if at(j + 1) == Some("=") => {
                            out.push((*tline, "`path = …` attribute".to_string()))
                        }
                        _ => {}
                    }
                    if depth == 0 {
                        break;
                    }
                }
            }
        }
    }
    out
}

#[test]
fn domain_source_has_no_clock_env_fs_net_thread_or_process() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    assert!(
        !files.is_empty(),
        "purity scan checked 0 files: the scan itself is broken"
    );
    let (mut hits, mut std_paths) = (Vec::new(), 0);
    for f in &files {
        let toks = tokens(&code_only(&fs::read_to_string(f).expect("read source")));
        std_paths += toks
            .windows(2)
            .filter(|w| STD_ROOTS.contains(&w[0].1.as_str()) && w[1].1 == "::")
            .count();
        for (line, why) in refusals(&toks) {
            hits.push(format!("{}:{line}: {why}", f.display()));
        }
    }
    let root = fs::read_to_string(src.join("lib.rs")).expect("read domain/src/lib.rs");
    let root_code: String = code_only(&root).chars().filter(|c| !c.is_whitespace()).collect();
    if !root_code.contains(STDERR_FORBID) {
        hits.push(format!(
            "{}: crate root lacks `{STDERR_FORBID}`",
            src.join("lib.rs").display()
        ));
    }
    eprintln!(
        "purity scan checked {} files, {std_paths} std/core/alloc paths",
        files.len()
    );
    assert!(hits.is_empty(), "domain must stay pure:\n{}", hits.join("\n"));
}
