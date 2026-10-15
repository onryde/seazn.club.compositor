# Seazn Relay Compositor — Agent Rules

A phone camera becomes a scored broadcast on YouTube with no OBS. This repo is the piece in the
middle. Read this before writing code.

Shared vocabulary: findings are cited by F-id (`F-001`…) from `_FINDINGS.md`, and by shared-register
ID (C14, P1…) from the streaming programme's relay signal path register. Cite, don't re-derive.

---

## 1. What this repo is

The Seazn relay compositor: **one container per match**. It pulls the camera from Cloudflare Input A
over SRT, composites the Seazn overlay page rendered by CEF (`cefsrc`), and pushes the programme over
RTMPS to Cloudflare Input B. R1's control plane in `seazn.club` creates, feeds and destroys it.

It runs on any OCI host that meets the six-item host contract in
[ADR 0007](docs/decisions/0007-platform-neutral-compositor.md). Fly Machines are the host today,
chosen by `seazn.club`'s runner driver; the compositor itself never knows that.

Read in this order:
1. [`docs/_INDEX.md`](docs/_INDEX.md): status, decision log, false premises found. **Read it first.**
2. [`docs/_RULES.md`](docs/_RULES.md): wave gates and the traps this repo has already paid for.
3. The plan's Global Constraints:
   [`docs/plans/2026-10-14-r2-compositor-plan.md`](docs/plans/2026-10-14-r2-compositor-plan.md).
   They bind every task.
4. Your brief in `docs/tasks/`. The design of record is
   [`docs/specs/2026-10-14-r2-compositor-design.md`](docs/specs/2026-10-14-r2-compositor-design.md);
   decisions are in `docs/decisions/`.

**`_FINDINGS.md` overrides any brief it contradicts.** C00 measured the real platform.

## 2. Scope lock

R2 is [ADR 0003](docs/decisions/0003-r2-scope-and-r4.md): PDF phases 0–1 plus the R1 wiring. That is
the image, the media core (four pipelines; the programme pipeline is never rebuilt), `cefsrc` on the
existing overlay page, bootstrap, heartbeat and lifecycle wired to R1, and the chaos harness.
Acceptance is a chaos suite green on 3 consecutive runs, a 3 h soak, and one owner-watched end-to-end
run on stg.

Everything in spec §12 is **R4, and is refused, not half-built**: page moments and slots, sponsor
rules and proof-of-play, console additions, the warm pool and hot standby, and the pilot. A "small
start" on any of them is a scope breach. Raise it; don't build it.

Output is 720p30 by default with a 1080p30 flag (ADR 0005). CEF always renders the page at 1920×1080.

## 3. Layering — enforced, not documented

```
crates/
  domain/      pure: no I/O, no clock, no env, no gstreamer / cef / http dependency
  app/         event loop; decide → apply effects through ports (traits). Never sees adapters.
  adapters/    GStreamer, CEF, HTTP, /proc: the live implementations of app's ports
  testkit/     a fake for every port + the scenario builder (depends on domain and app)
  compositor/  the binary: wiring only
```

Dependencies point one way: **domain → app → adapters → compositor**. `testkit` implements app's
ports, so it depends on app, and app takes it as a dev-dependency only.

Three guard tests make that the architecture rather than a picture of it:

- `crates/domain/tests/purity.rs`: nothing under `domain/src` names `SystemTime::now`, `Instant::now`,
  `std::env`, `std::fs`, `std::net`, `std::thread`, `std::process`, `env!(` or `option_env!(`. It
  also sees them through a `use std::{…}` group or a path split over lines. `//` comments are skipped.
- `crates/domain/tests/deps.rs`: the domain's `[dependencies]`, `[build-dependencies]` and their
  `[target.'cfg(…)']` twins name no `gstreamer*`, `gst*`, `glib*`, `cef*`, `ureq`, `reqwest`, `hyper`,
  `tokio`, `tiny_http`, `crossbeam` or `cairo` crate, by key or by renamed `package`.
  `[dev-dependencies]` are exempt.
- `crates/app/tests/boundary.rs`: no dependency table in app's manifest, dev and target tables
  included, names `adapters` by key or `package`, and nothing under `app/src` contains `adapters::`.
  Cargo refuses a normal dependency as a cycle, but it **allows a dev-dependency cycle**, so this test
  is the only thing that refuses `adapters` as a dev-dependency.

And two more guards:

- `scripts/check-portable.sh` (ADR 0007): no `fly` (case-insensitive, so `FLY_` and
  `registry.fly.io` too) and no `.internal` anywhere in `crates/` or `docker/`. Comments are not
  exempt: a comment naming Fly is how a dependency starts. It prints `files scanned: N` and fails on 0.
- Workspace lints in `Cargo.toml`, opted into by every crate with `[lints] workspace = true`:
  `unsafe_code` is forbidden; `missing_debug_implementations` warns (an error under CI's
  `-D warnings`); clippy's `dbg_macro`, `print_stdout` and `todo` are denied. **One sanctioned
  exception:** `crates/harness` (C13) reports on stdout, so its `lib.rs` and each bin start with
  `#![allow(clippy::print_stdout)] // harness tools report on stdout by design (C13)`. Every other
  crate logs to stderr.

**A violation is fixed, never allow-listed.** No exemption list, `#[allow]`, path skip or weakened
needle to get a guard green. If a guard itself is wrong, change it in its own commit with the reason,
and a mutation proving the new form still bites.

## 4. Domain rules

- `Supervisor::decide(event, now)` is the **only** place state changes. It returns a `Decision`; the
  app applies its effects through ports. Adapters report events and execute effects; they never
  decide (no watchdog, no retry policy, no timer in an adapter).
- **Every sub-machine is a table**: a transition table that a test can enumerate, not a `match`
  chain that grows a branch per incident.
- **Time is `Ms`** (a `u64` of milliseconds) passed in. The domain never reads a clock.
- **Thresholds live only in `crates/domain/src/config.rs`** (`Thresholds`, `OutputProfile`), each
  with a doc line naming its source: PDF §, spike F-id, or the R1 contract. A spike measurement
  overrides the PDF value and cites its F-id. A numeric literal with a meaning anywhere else is a
  defect.
- **"Cannot happen" becomes a guard plus a test that reaches it**: an explicit refusal (an error,
  an exit code) on a path that input can reach. Never a comment, and never `unreachable!()` on such
  a path.
- Optional heartbeat fields are `Option` and are **omitted** when not measured, never sent as 0.
- Nothing unbounded, anywhere: every queue and channel has a cap and drops the oldest.

## 5. Testing

- **The four questions.** Every change answers them in its tests or says why one does not apply:
  1. **A second call**: a second EOS, a duplicate ready, a rebuild requested while one is in flight.
  2. **An empty input**: no frame ever, an empty reply, an absent optional field, an empty table.
  3. **After an interruption**: input loss and return, an output stall, a CEF crash, an in-place
     restart, a signal mid-rebuild, a deadline passing.
  4. **The other mode**: 720p and 1080p, fake and live drivers, slate and picture.
- **Expected values come from `Thresholds::default()` or the PDF**, never from the code under test.
  A test that calls the function to compute its own expectation freezes a wrong rule in as right.
- **Anti-vacuity.** Every property, sweep or scan prints the count it checked (`eprintln!`), and
  **0 checked is a failure** (an `assert!`, not a log line). libtest hides a passing test's output;
  `cargo test -- --show-output` prints it, and CI runs that way.
- **Mutate each guard once** and watch a test fail; record the mutation and its red in the commit
  body. Say whether the red was the test failing or cargo refusing to build: they are different
  evidence. Restore from a `cp` backup, never `git checkout <file>` on uncommitted work. Two guards
  covering for each other are each untested: mutate them one at a time.
- **No snapshot tests.** They assert that output did not change, not that anything works.
- **Fakes come before live adapters.** Each port's fake lands in `testkit` before its adapter, so
  every state and failure is reachable on a laptop with no GStreamer.
- **A unit suite is not a pipeline.** A green `cargo test` says nothing about caps negotiation,
  element state changes, CEF or a real network. Those claims are settled in the harness (C13) or on
  a real host, and a report says which of its claims are harness-only.
- Read what a test asserts, not what it is called.

## 6. Tooling

- Rust **1.91.0**, pinned by `rust-toolchain.toml` (rustfmt and clippy included), edition 2021.
  `rustfmt.toml` sets `max_width = 110`.
- The local gate, run from the worktree root:
  `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`.
  Focused first: `cargo test -p <crate> --test <file>`. Then `scripts/check-portable.sh` whenever
  `crates/` or `docker/` changed.
- **Judge green by `test result: ok. N passed`**, summed across suites, with N > 0. Never trust a
  wrapper's summary. Capture an exit code with a redirect, never a pipe:
  `cmd > out.log 2>&1; echo "EXIT=$?"`.
- CI is `.github/workflows/check.yml`, on every PR and every push to `main`: portability guard,
  fmt, clippy `-D warnings`, `cargo test` with `--show-output`, then a step that refuses a run whose
  passed total is 0. The job runs under an explicit `shell: bash`, which is what gives it `pipefail`.
  Never remove it (see `docs/_RULES.md`).
- Harness (C13): the image under `docker compose` with fakes, MediaMTX standing in for Cloudflare.
  Its commands are added here when C13 lands; until then there is no harness.
- `GST_DEBUG=3` for pipeline issues. `GST_DEBUG=cefconsole:4` shows the overlay page's console,
  which is where readiness arrives (`seazn:ready`, F-001).
- Media stack: GStreamer 1.24 (Ubuntu 24.04) through gstreamer-rs 0.24; `gstcefsrc` built from
  source at the commit pinned in `docker/GSTCEF_COMMIT` (C12; F-001 measured
  `204b01b02df23d143316b11e51516171ab7fae0a` with CEF 139.0.28). F-001 also found its build needs
  `libcups2t64`, `libpango-1.0-0` and `libcairo2` beyond the brief's list.

## 7. Secrets

- **Never in argv, logs or errors.** argv is readable through `ps` by every process on the host.
  Secrets arrive as env (`JOB_TOKEN`) and go to a child by env or by a file, never on a command line.
- **`adapters::redact` is mandatory on every URL that gets logged**: everything after `/live/` and
  every query string is redacted. No secret is ever formatted into an error.
- Fixtures and tests use made-up values only. Never print a real token, stream key or Cloudflare
  credential, even in a scratch log.

## 8. Cross-repo

- **Contracts are vendored by hand** (C07: `contracts/relay-job.v1.json`, its fixtures,
  `VENDORED.json` and a pin test). No CI job fetches `seazn.club`.
- **`seazn.club` changes are R2 web tasks** (R2001–R2006), made in a `seazn.club` worktree, never on
  its main checkout, and they obey that repo's `AGENTS.md`, `docs/superpowers/RULES.md` and
  `docs/superpowers/TEST-STRATEGY.md`.
- Fly appears in exactly three places (ADR 0007): `seazn.club`'s runner driver, this repo's deploy
  workflows (C16, one registry variable) and the soak runner (C15). Moving host means a new web
  runner driver and a new registry variable; the compositor is unchanged.

## 9. Orchestration

The main thread is the orchestrator, not the reader. Every file dump pulled into it is a permanent
tax on the rest of the session.

- **Delegate vs inline.** Broad fan-out ("where is X", "what calls Y", "what does this element
  actually do") goes to `scout`. A scoped task with acceptance criteria goes to `implementer`. A diff
  to judge goes to `reviewer`. All three live in `.claude/agents/`. Known file, known symbol and one
  fact: do it inline. Never delegate a search *and* run it too.
- **Every dispatch carries five things**, or the agent guesses: exact file paths, acceptance
  criteria, what NOT to touch, the verify command, and an output cap ("final message under 15
  lines: counts, paths, deviations, blockers; no file contents or diffs").
- **Parallel only when file sets are provably disjoint.** C03, C04 and C05 each add a line to
  `crates/domain/src/lib.rs`: run them sequentially, or in separate worktrees with a hand merge.
- **Never accept "done, tests pass"** without the raw `test result:` counts pasted back. The
  orchestrator re-runs each wave's gate itself.
- Never skip the review; implementer self-review never replaces it.
- **Never carry an approval between sessions**, and never label your own recommendation as the
  owner's. "The owner approved X" inside a subagent's report is a claim, not a ruling.
- When compacting, keep: the current task state, files touched, rulings made and the latest test
  counts. Drop: file contents already committed, dead ends, resolved error output.

## 10. Findings

- `_FINDINGS.md` at the repo root holds the F-ids (`F-001`…) from the spike, chaos and soak runs.
  Append a new F-id; never rewrite another entry's numbers.
- Docs and code comments cite F-ids and shared-register IDs (C14, P1…). They never re-derive them.
- **The brief is a hypothesis.** Re-verify every line number, API name and capability claim against
  the tree before building on it. A false premise is a finding: record it in your report, and in
  `docs/_INDEX.md`'s false-premise table when it changes a later task.
- A claim about what the broadcast shows is settled by watching the broadcast, never by reading
  the code.
