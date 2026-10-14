# C01: Workspace scaffold, rulebook, CI check, guard tests

**Repo:** `seazn.club.compositor`. Branch `feat/c01-scaffold` from `main`.
**Depends on:** nothing.

## Context

The repo is empty apart from `README.md` and `docs/`. This task lays down:
- the Cargo workspace with five crates
- the rulebook (`AGENTS.md`), using `seazn.club.capture/AGENTS.md`'s shape adapted for Rust
- the CI check job
- the three **guard tests** that enforce the layering, so it is enforced rather than just documented

No GStreamer dependency yet. That arrives in C09, together with the CI apt step.

## Files

- Create: `Cargo.toml`, `rust-toolchain.toml`, `.gitignore`, `rustfmt.toml`
- Create: `crates/domain/{Cargo.toml,src/lib.rs}`, `crates/domain/tests/{purity.rs,deps.rs}`
- Create: `crates/app/{Cargo.toml,src/lib.rs}`, `crates/app/tests/boundary.rs`
- Create: `crates/adapters/{Cargo.toml,src/lib.rs}`, `crates/testkit/{Cargo.toml,src/lib.rs}`,
  `crates/compositor/{Cargo.toml,src/main.rs}`
- Create: `AGENTS.md`, `CLAUDE.md`, `docs/_RULES.md`, `_FINDINGS.md` (header only, unless C00 already
  wrote it, in which case leave it alone)
- Create: `.claude/agents/{scout,implementer,reviewer}.md`, copied from
  `/Users/ashokhein/github/seazn.club.capture/.claude/agents/` and retargeted (step 9)
- Create: `.github/workflows/check.yml`

## Interfaces

- **Produces:** crate names `domain`, `app`, `adapters`, `testkit`, `compositor`. Every later task
  adds modules to these. `domain` exports nothing yet.

## Steps

- [ ] **Step 1: Workspace files.**

  `Cargo.toml`:
  ```toml
  [workspace]
  resolver = "2"
  members = ["crates/domain", "crates/app", "crates/adapters", "crates/testkit", "crates/compositor"]

  [workspace.package]
  edition = "2021"
  rust-version = "1.91"
  publish = false
  license = "UNLICENSED"

  [workspace.lints.rust]
  unsafe_code = "forbid"
  missing_debug_implementations = "warn"

  [workspace.lints.clippy]
  dbg_macro = "deny"
  print_stdout = "deny"
  todo = "deny"
  ```

  `rust-toolchain.toml`:
  ```toml
  [toolchain]
  channel = "1.91.0"
  components = ["rustfmt", "clippy"]
  ```

  `rustfmt.toml`: `max_width = 110`

  `.gitignore`: `/target`, `*.png` (evidence PNGs never live in the repo), `.DS_Store`, `harness/out/`

- [ ] **Step 2: Crate manifests.**

  `crates/domain/Cargo.toml`:
  ```toml
  [package]
  name = "domain"
  version = "0.1.0"
  edition.workspace = true
  rust-version.workspace = true
  publish.workspace = true

  [lints]
  workspace = true

  [dependencies]

  [dev-dependencies]
  proptest = "1"
  toml = "0.8"
  ```

  `crates/app/Cargo.toml`: the same header with `name = "app"`, plus:
  ```toml
  [dependencies]
  domain = { path = "../domain" }

  [dev-dependencies]
  testkit = { path = "../testkit" }
  toml = "0.8"
  ```

  `crates/testkit/Cargo.toml`: `name = "testkit"`, with deps `domain = { path = "../domain" }` and
  `app = { path = "../app" }`.

  `crates/adapters/Cargo.toml`: `name = "adapters"`, with deps `domain` and `app` (path).

  `crates/compositor/Cargo.toml`: `name = "compositor"`, with deps `domain`, `app` and `adapters`
  (path), plus a `[[bin]] name = "compositor" path = "src/main.rs"` section.

  `testkit` depends on `app`, because fakes implement app's port traits. `app`'s dev-dependency on
  `testkit` is the conventional Rust cycle and is allowed for dev-deps.

- [ ] **Step 3: Skeleton sources.**
  - `crates/domain/src/lib.rs`:
    ```rust
    //! Pure domain of the Seazn relay compositor. No I/O, no clock, no env: time is passed in as `Ms`.
    //! Purity is enforced by `tests/purity.rs` and `tests/deps.rs`.
    #![forbid(unsafe_code)]
    ```
  - `crates/app/src/lib.rs`: `//! Application layer: event loop and effect application through ports.`
  - `crates/adapters/src/lib.rs`: `//! Live adapters: GStreamer, CEF, HTTP, /proc.`
  - `crates/testkit/src/lib.rs`: `//! Fakes for every port, plus the scenario builder.`
  - `crates/compositor/src/main.rs`: `fn main() {}`

- [ ] **Step 4: Write the failing guard tests.**

  `crates/domain/tests/purity.rs`:
  ```rust
  //! Guard: the domain crate's source never reaches for the clock, env, filesystem, network or threads.
  use std::fs;
  use std::path::{Path, PathBuf};

  const FORBIDDEN: &[&str] = &[
      "SystemTime::now", "Instant::now", "std::env", "std::fs", "std::net", "std::thread",
      "env!(", "option_env!(", "std::process",
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

  #[test]
  fn domain_source_has_no_clock_env_fs_net_thread_or_process() {
      let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
      let mut files = Vec::new();
      rust_files(&src, &mut files);
      eprintln!("purity scan checked {} files", files.len());
      assert!(!files.is_empty(), "purity scan checked 0 files: the scan itself is broken");
      let mut hits = Vec::new();
      for f in &files {
          let text = fs::read_to_string(f).expect("read source");
          for (i, line) in text.lines().enumerate() {
              let code = line.split("//").next().unwrap_or("");
              for bad in FORBIDDEN {
                  if code.contains(bad) {
                      hits.push(format!("{}:{}: {bad}", f.display(), i + 1));
                  }
              }
          }
      }
      assert!(hits.is_empty(), "domain must stay pure:\n{}", hits.join("\n"));
  }
  ```

  `crates/domain/tests/deps.rs`:
  ```rust
  //! Guard: the domain crate depends on nothing that does I/O.
  const FORBIDDEN_PREFIXES: &[&str] =
      &["gstreamer", "gst", "glib", "cef", "ureq", "reqwest", "hyper", "tokio", "tiny_http", "crossbeam", "cairo"];

  #[test]
  fn domain_has_no_io_dependencies() {
      let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).expect("manifest");
      let doc: toml::Table = text.parse().expect("toml");
      let deps = doc.get("dependencies").and_then(|d| d.as_table()).cloned().unwrap_or_default();
      let mut names: Vec<String> = deps.keys().cloned().collect();
      // A renamed dependency (`gst = { package = "gstreamer" }`) is checked by its real package name too.
      for v in deps.values() {
          if let Some(pkg) = v.as_table().and_then(|t| t.get("package")).and_then(|p| p.as_str()) {
              names.push(pkg.to_string());
          }
      }
      let bad: Vec<&String> =
          names.iter().filter(|n| FORBIDDEN_PREFIXES.iter().any(|p| n.starts_with(p))).collect();
      eprintln!("deps guard checked {} dependency names", names.len());
      assert!(bad.is_empty(), "domain must not depend on I/O crates: {bad:?}");
  }
  ```
  (With zero dependencies the count is 0. That's legitimate here: the guard is over the manifest, and
  the mutation in step 6 proves it bites.)

  `crates/app/tests/boundary.rs`:
  ```rust
  //! Guard: the application layer never depends on, or names, the adapters crate.
  use std::fs;
  use std::path::{Path, PathBuf};

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

  #[test]
  fn app_manifest_has_no_adapters_dependency() {
      let text = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).expect("manifest");
      let doc: toml::Table = text.parse().expect("toml");
      for section in ["dependencies", "dev-dependencies"] {
          let has = doc.get(section).and_then(|d| d.as_table()).is_some_and(|t| t.contains_key("adapters"));
          assert!(!has, "app/{section} must not include adapters");
      }
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
  ```

- [ ] **Step 5: Run.** `cargo test --workspace`.
  Expected: PASS, with 4 tests (purity 1, deps 1, boundary 2). Read the printed counts:
  `purity scan checked 1 files` and `boundary scan checked 1 files`.

- [ ] **Step 6: Mutate each guard once, and record it in the commit body.**
  1. Add `pub fn t() -> u64 { std::time::Instant::now().elapsed().as_millis() as u64 }` to
     `domain/src/lib.rs`. Run `cargo test -p domain --test purity`. Expected **FAIL**. Revert.
  2. Add `ureq = "2"` under `[dependencies]` in `domain/Cargo.toml`. Run
     `cargo test -p domain --test deps`. Expected **FAIL**. Revert.
  3. Add `adapters = { path = "../adapters" }` to app's `[dependencies]`. Run
     `cargo test -p app --test boundary`. Expected **FAIL**. This may fail as a cargo cycle error
     first; either failure counts, but note which one. Revert.

- [ ] **Step 7: CI.** `.github/workflows/check.yml`:
  ```yaml
  name: check
  on:
    push: { branches: [main] }
    pull_request: {}
  permissions: { contents: read }
  jobs:
    check:
      runs-on: ubuntu-24.04
      timeout-minutes: 20
      steps:
        - uses: actions/checkout@v4
        - uses: dtolnay/rust-toolchain@master
          with: { toolchain: "1.91.0", components: "rustfmt, clippy" }
        - uses: Swatinem/rust-cache@v2
        - run: cargo fmt --all --check
        - run: cargo clippy --workspace --all-targets -- -D warnings
        - run: cargo test --workspace --no-fail-fast 2>&1 | tee test.log
        - name: Refuse a vacuous run
          run: |
            total=$(grep -Eo 'test result: ok\. [0-9]+ passed' test.log | awk '{s+=$4} END {print s+0}')
            echo "tests passed: $total"
            test "$total" -gt 0
  ```

- [ ] **Step 8: Rulebook.** `CLAUDE.md` contains exactly `@AGENTS.md`. `AGENTS.md` has these sections.
  Write them out in full prose, about 150–250 lines, using the facts given here:
  1. **What this repo is.** The Seazn relay compositor: one per match, on Fly. Link to the spec, the
     plan and `docs/_INDEX.md`. Read `_INDEX.md` first.
  2. **Scope lock.** R2 is ADR 0003. Anything in spec §12 (R4) is refused, not half-built.
  3. **Layering, enforced.** domain → app → adapters → compositor. The three guard tests and what each
     one forbids. A violation is fixed, never allow-listed.
  4. **Domain rules.**
     - `decide(event, now)` is the only place state changes.
     - Every sub-machine is a table.
     - Time is `Ms` passed in.
     - Thresholds live only in `config.rs`, each with its source.
     - "Cannot happen" becomes a guard plus a test that reaches it.
  5. **Testing.**
     - The four questions: second call, empty input, after interruption, other mode.
     - Expected values come from `Thresholds::default()` or the PDF, never from the code under test.
     - Every property or sweep prints its checked count, and 0 counts as a failure.
     - Mutate each guard once.
     - No snapshot tests.
     - Fakes come before live adapters.
  6. **Tooling.**
     - `cargo test --workspace`; judge green by `test result: ok. N passed`, with N > 0.
     - Harness commands (C13).
     - `GST_DEBUG=3` for pipeline issues.
  7. **Secrets.** Never in argv, logs or errors. `adapters::redact` is mandatory on every URL that gets
     logged.
  8. **Cross-repo.**
     - Contracts are vendored by hand (C07) and never fetched in CI.
     - `seazn.club` changes are W tasks, made in a `seazn.club` worktree.
  9. **Orchestration.**
     - Main thread orchestrates.
     - Every dispatch carries: files, acceptance, what not to touch, the verify command, and an output
       cap.
     - Never accept "done, tests pass" without raw counts.
  10. **Findings.** `_FINDINGS.md` F-ids. Docs cite F-ids and shared-register IDs (C14, P1…), never
      re-derive them.

  `docs/_RULES.md` gets a short file. It points at `AGENTS.md`, restates the plan's wave gates, and
  lists these repo traps:
  - SRT push from Fly never starts a CF broadcast (shared register C14)
  - a secret in argv leaks through `ps`
  - `cargo test` from the wrong cwd silently runs a different workspace; confirm the path

- [ ] **Step 9: Agents.** Copy the three files from
  `/Users/ashokhein/github/seazn.club.capture/.claude/agents/`. In each one, replace capture-specific
  commands (pnpm, vitest, gradle) with `cargo …`. Keep the `model:` frontmatter exactly as it is in
  the source; never change models.

- [ ] **Step 10: Verify and commit.**
  ```bash
  cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
  git add -A && git commit -m "feat(scaffold): workspace, rulebook, CI check and layering guards (C01)"
  ```
  The commit body lists the three mutations and their red results.

## Acceptance

- `cargo test --workspace` shows 4 passed, with the counts printed.
- All 3 mutations went red, as recorded in the commit body.
- `check.yml` is valid, and its "refuse vacuous" step fails when total = 0.

## Do not touch

`docs/specs`, `docs/decisions`, `docs/plans`, `docs/tasks`.

## Output cap

Final message under 15 lines: test counts, mutation results, deviations.
