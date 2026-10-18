# C07: Vendor the contract; ControlPlane port and HTTP adapter

**Repo:** `seazn.club.compositor`. **Depends on:** C06, and **R2002 merged** in `seazn.club`. You need
its commit sha.

## Context

The compositor talks to R1 only over two outbound HTTPS calls (spec §5):
`GET {APP_URL}/api/internal/relay/sessions/{SESSION_ID}` and `POST …/heartbeat`. Both carry
`Authorization: Bearer {JOB_TOKEN}`.

The contract is `docs/contracts/relay-job.v1.json` plus fixtures in `seazn.club`. It gets vendored by
hand, with a sha256 pin and a pin test, following the capture convention. No CI job fetches the web
repo.

**Status mapping:**

| Response | Result |
|---|---|
| 401 | `Refusal::Unauthorized` |
| 404 | `Refusal::NotFound` |
| 410 | `Refusal::Gone` |
| 200 with `mode != "composed"`, or missing `input`/`output`/`overlayUrl` | `Refusal::WrongMode` (review focus 3) |
| 5xx or network error | transient: the domain retries on backoff |

## Files

- Create: `contracts/relay-job.v1.json`, `contracts/fixtures/relay-job.v1/*` (byte copies),
  `contracts/VENDORED.json`
- Create: `scripts/vendor-contracts.sh`
- Create: `crates/app/src/ports.rs`, with the ControlPlane part only (C08 adds the rest)
- Create: `crates/adapters/src/{control_http.rs,redact.rs}`
- Modify: `crates/app/src/lib.rs`, `crates/adapters/src/lib.rs`, and both `Cargo.toml` files
- Test: `crates/adapters/tests/contract.rs`, `crates/adapters/tests/control_http.rs`,
  `contracts/` pin test inside `contract.rs`

## Interfaces

- **Consumes:** `domain::{config::OutputProfile, supervisor::{Refusal, Desired}, health::*, ladder::Rung, programme::ProgrammePhase}`.
- **Produces** (C08 and C12 rely on these):
  ```rust
  // app/src/ports.rs
  #[derive(Debug, Clone, PartialEq, Eq)] pub struct InputSpec { pub url: String, pub stream_id: String, pub passphrase: String }
  #[derive(Debug, Clone, PartialEq, Eq)] pub struct TargetSpec { pub url: String, pub stream_key: String }
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct SessionConfig {
      pub session_id: String, pub fixture_id: String,
      pub input: InputSpec, pub target: TargetSpec, pub output: domain::config::OutputProfile,
      pub heartbeat_interval_ms: u64, pub overlay_url: String, pub overlay_delay_ms: u64,
  }
  // `maxDurationMinutes` is deliberately NOT decoded: RELAY_DEADLINE_AT (env) is the one deadline authority.
  // `overlay_delay_ms` has one reader: parse_session refuses a session whose overlayUrl `delay=` disagrees with it.
  #[derive(Debug, Clone, PartialEq, Eq)] pub enum FetchError { Refused(domain::supervisor::Refusal), Transient(String) }
  /// Values measured by adapters (absent ⇒ None ⇒ omitted on the wire).
  #[derive(Debug, Clone, Default, PartialEq)] pub struct Measured { pub fps: Option<f64>, pub bitrate_kbps: Option<u32>, pub egress_bytes: Option<u64>, pub measured_latency_ms: Option<u64>, pub encoder_speed: Option<f64>, pub cpu_pct: Option<f64>, pub mem_mb: Option<u64> }
  pub trait ControlPlane: Send {
      fn fetch_session(&mut self) -> Result<SessionConfig, FetchError>;
      fn heartbeat(&mut self, report: &domain::health::HealthReport, measured: &Measured) -> Result<domain::supervisor::Desired, String>;
      // (amended 2026-10-18, wave gate 1 I5) the Err tells a 410 apart from a transport error: see that amendment
  }
  // adapters
  pub fn redact(url: &str) -> String;                                 // redact.rs
  pub struct HttpControlPlane { /* ureq agent, base, sid, token */ }  // control_http.rs
  impl HttpControlPlane { pub fn new(app_url: &str, session_id: &str, job_token: String, timeout_ms: u64) -> Self; }
  impl app::ports::ControlPlane for HttpControlPlane { … }
  pub fn heartbeat_wire(report: &HealthReport, measured: &Measured) -> serde_json::Value; // pub for the contract test
  ```

## Steps

- [ ] **Step 1: Vendor the contract.** `scripts/vendor-contracts.sh`:
  ```bash
  #!/usr/bin/env bash
  # usage: scripts/vendor-contracts.sh /abs/path/to/seazn.club <web-commit-sha>
  # Byte-copies relay-job contracts from the web repo at an exact commit and records sha256 pins. Run by hand; never in CI.
  set -euo pipefail
  web="$1"; sha="$2"; here="$(cd "$(dirname "$0")/.." && pwd)"
  tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
  git -C "$web" archive "$sha" docs/contracts/relay-job.v1.json docs/contracts/fixtures/relay-job.v1 | tar -x -C "$tmp"
  rm -rf "$here/contracts"; mkdir -p "$here/contracts"
  cp -R "$tmp/docs/contracts/." "$here/contracts/"
  ( cd "$here/contracts" && find . -type f ! -name VENDORED.json | sort | while read -r f; do
      printf '    "%s": "%s",\n' "${f#./}" "$(shasum -a 256 "$f" | cut -d' ' -f1)"; done ) > "$tmp/pins"
  { echo '{'; echo "  \"source\": \"onryde/seazn.club\","; echo "  \"commit\": \"$sha\","; echo '  "files": {';
    sed '$ s/,$//' "$tmp/pins"; echo '  }'; echo '}'; } > "$here/contracts/VENDORED.json"
  echo "vendored $(grep -c '": "' "$tmp/pins") files at $sha"
  ```
  Run it with the R2002 merge sha. Confirm the printed count equals the number of files under
  `docs/contracts/fixtures/relay-job.v1/` plus 1.

  **(Amended 2026-10-18, `docs/_INDEX.md` Owed 24)** Run it with a `seazn.club` commit at or after
  #980's merge, never the R2002 merge sha. #980 removed `fast_preset` from the heartbeat's `rung`,
  and `docs/contracts/relay-job.v1.json` hashes to
  `fc99d44899ee543cfe36c48902573501f815d68b4ec7c2e4bbb50d096608dc50` there (it was
  `7091c0c4…6037` at R2002). The pin test's expected sha for that file is that value, or a later
  bump's. A contract test asserts that the vendored schema's `rung` enum is exactly
  `["full","overlay_half_rate"]`, and that every `Rung` variant serialises into it.

- [ ] **Step 2: Dependencies.**
  - `crates/app`: `serde = { version = "1", features = ["derive"] }` is **not** needed yet, because
    ports are plain structs.
  - `crates/adapters` `[dependencies]`: `ureq = { version = "2", default-features = false, features = ["tls", "json"] }`,
    `serde = { version = "1", features = ["derive"] }`, `serde_json = "1"`
  - `[dev-dependencies]`: `jsonschema = "0.26"`, `sha2 = "0.10"`, `tiny_http = "0.12"`, `hex = "0.4"`

- [ ] **Step 3: Write the failing contract tests.** `crates/adapters/tests/contract.rs`:
  ```rust
  //! The vendored contract is the arbiter: our wire output must validate against it, and its fixtures
  //! must decode into our types. Expected values come from the vendored files, never from our code.
  use adapters::control_http::{heartbeat_wire, parse_session};
  use app::ports::{FetchError, Measured};
  use domain::config::Thresholds;
  use domain::supervisor::{Event, Refusal, Supervisor};
  use sha2::{Digest, Sha256};
  use std::path::{Path, PathBuf};

  fn root() -> PathBuf { Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts") }
  fn fixture(name: &str) -> serde_json::Value {
      serde_json::from_str(&std::fs::read_to_string(root().join("fixtures/relay-job.v1").join(name)).expect(name)).expect("json")
  }
  fn schema_def(def: &str) -> jsonschema::Validator {
      let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(root().join("relay-job.v1.json")).unwrap()).unwrap();
      // A fresh wrapper, never a mutated copy of the doc: 2020-12 ANDs sibling keywords beside $ref,
      // so a root that carried its own schema would leak into every def's validation.
      let s = serde_json::json!({ "$schema": doc["$schema"], "$defs": doc["$defs"], "$ref": format!("#/$defs/{def}") });
      jsonschema::validator_for(&s).expect("schema compiles")
  }

  #[test]
  fn vendored_files_match_their_pins() {
      let pins: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(root().join("VENDORED.json")).unwrap()).unwrap();
      let files = pins["files"].as_object().expect("files map");
      eprintln!("pinned files checked: {}", files.len());
      assert!(!files.is_empty(), "anti-vacuity: no pinned files");
      for (rel, want) in files {
          let got = hex::encode(Sha256::digest(std::fs::read(root().join(rel)).expect(rel)));
          assert_eq!(&got, want.as_str().unwrap(), "{rel} drifted from its pin — re-vendor, never hand-edit");
      }
  }

  #[test]
  fn every_heartbeat_we_can_emit_validates_against_the_contract() {
      let v = schema_def("heartbeat");
      let t = Thresholds::default();
      // Drive a real supervisor through boot, live, output loss and drain; validate every beat it emits.
      let mut s = Supervisor::new(t.clone()).decide(Event::Tick, 0).next;
      s = s.decide(Event::BootstrapOk { heartbeat_interval_ms: 5_000, deadline_at: u64::MAX / 2 }, 0).next;
      let mut beats = 0;
      let script: Vec<Event> = (0..400).map(|i| match i % 4 { 0 => Event::InputFrame, 1 => Event::OutputConnected { gen: 1 }, _ => Event::Tick }).collect();
      let mut now = 0;
      for ev in script {
          now += 33;
          let d = s.decide(ev, now);
          for e in &d.effects {
              if let domain::supervisor::Effect::SendHeartbeat(h) = e {
                  for m in [Measured::default(), Measured { fps: Some(30.0), cpu_pct: Some(41.5), mem_mb: Some(812), ..Measured::default() }] {
                      let wire = heartbeat_wire(h, &m);
                      let errs: Vec<String> = v.iter_errors(&wire).map(|e| e.to_string()).collect();
                      assert!(errs.is_empty(), "beat {wire} violates contract: {errs:?}");
                      beats += 1;
                  }
              }
          }
          s = d.next;
      }
      eprintln!("heartbeats validated: {beats}");
      assert!(beats > 0, "anti-vacuity");
  }

  #[test]
  fn unmeasured_values_are_absent_not_zero() {
      let t = Thresholds::default();
      let s = Supervisor::new(t).decide(Event::Tick, 0).next;
      let wire = heartbeat_wire(&s.health(0), &Measured::default());
      for k in ["fps", "bitrateKbps", "egressBytes", "measuredLatencyMs", "encoderSpeed", "cpuPct", "memMb", "scoreStale"] {
          assert!(wire.get(k).is_none(), "{k} must be omitted when unmeasured, got {:?}", wire.get(k));
      }
  }

  #[test]
  fn composed_fixtures_decode_and_passthrough_is_refused() {
      for name in ["session-composed-720p.json", "session-composed-1080p.json"] {
          let cfg = parse_session(&fixture(name)).unwrap_or_else(|e| panic!("{name}: {e:?}"));
          let want = &fixture(name)["output"];
          assert_eq!(u64::from(cfg.output.width), want["width"].as_u64().unwrap());
          assert_eq!(cfg.overlay_url, fixture(name)["overlayUrl"].as_str().unwrap());
      }
      assert_eq!(parse_session(&fixture("session-passthrough.json")), Err(FetchError::Refused(Refusal::WrongMode)));
  }

  #[test]
  fn overlay_delay_must_agree_with_the_overlay_url() {
      let mut odd = fixture("session-composed-720p.json");
      let delay = odd["overlayDelayMs"].as_u64().unwrap();
      odd["overlayDelayMs"] = serde_json::json!(delay + 1);
      assert_eq!(parse_session(&odd), Err(FetchError::Refused(Refusal::WrongMode)), "two disagreeing delays: refuse, never pick one");
      let mut bare = fixture("session-composed-720p.json");
      let url = bare["overlayUrl"].as_str().unwrap().to_owned();
      let stripped = url.split('&').filter(|kv| !kv.starts_with("delay=") && !kv.contains("?delay=")).collect::<Vec<_>>().join("&");
      assert!(!stripped.contains("delay="), "test setup: delay= must be gone from {stripped}");
      bare["overlayUrl"] = serde_json::json!(stripped);
      assert_eq!(parse_session(&bare), Err(FetchError::Refused(Refusal::WrongMode)), "an absent delay= is refused, never read as 0");
  }

  #[test]
  fn output_must_be_a_declared_profile() {
      use domain::config::OutputProfile;
      assert_eq!(parse_session(&fixture("session-composed-720p.json")).unwrap().output, OutputProfile::P720);
      assert_eq!(parse_session(&fixture("session-composed-1080p.json")).unwrap().output, OutputProfile::P1080);
      let mut odd = fixture("session-composed-720p.json");
      odd["output"]["width"] = serde_json::json!(1000);
      assert_eq!(parse_session(&odd), Err(FetchError::Refused(Refusal::WrongMode)), "an undeclared profile is refused, never improvised");
  }
  ```

- [ ] **Step 4: Write the failing HTTP tests.** `crates/adapters/tests/control_http.rs` runs a
  `tiny_http` server on `127.0.0.1:0` in a thread and serves scripted responses. It covers:
  - `fetch_session_sends_bearer_token_and_parses_200`: assert the request path is
    `/api/internal/relay/sessions/<sid>` and the `Authorization` header is `Bearer <token>`. Serve
    `session-composed-720p.json`.
  - `status_401_404_410_map_to_refusals`: a table over the three codes.
  - `status_503_and_connection_refused_are_transient`: point at an unbound port for the second case.
  - `heartbeat_posts_to_heartbeat_path_and_reads_desired_state`: serve `reply-ending.json` and assert
    `Desired::Ending`.
  - `transport_errors_never_contain_the_token`: make the server close the connection, then assert the
    error string does not contain the token value.

- [ ] **Step 5: Run, expect a FAIL.** `cargo test -p adapters`

- [ ] **Step 6: Implement `redact.rs`.** Port the POC's `redact()`, keeping its two rules:
  - redact everything after `/live/` or `/live2/`
  - replace every query string with `?****`

  Add one more: in `srt://` URLs, `passphrase=` and `streamid=` values are always redacted.

  Unit tests:
  - `rtmps://live.cloudflare.com:443/live/abc123` becomes `rtmps://live.cloudflare.com:443/live/****`
  - `srt://h:778?streamid=x&passphrase=y` becomes `srt://h:778?****`
  - a URL with no secret is unchanged

- [ ] **Step 7: Implement `control_http.rs`.**
  - `HttpControlPlane` holds a `ureq::Agent` with a `timeout_ms` overall timeout.
  - `parse_session(&Value) -> Result<SessionConfig, FetchError>` is pub. It reads the camelCase keys.
    It returns `WrongMode` if `mode != "composed"`, if any of `input`, `output`, `overlayUrl` or
    `heartbeatIntervalMs` is absent, or if `output` is not exactly `OutputProfile::P720` or `P1080`
    (web's `relayOutputProfile`, R2004, is pinned to the same fixtures), or if the `delay=` query value
    in `overlayUrl` differs from `overlayDelayMs`. An **absent** `delay=` (or absent `overlayDelayMs`)
    is also `WrongMode`: both authorities must be present and agree, never one defaulted.
  - `heartbeat_wire(&HealthReport, &Measured) -> Value` builds a `serde_json::Map`:
    - `state`: `starting|playing|stalled|stopped`
    - `programmePhase`: lower-case
    - `input`/`output`: `connecting|connected|backoff`
    - `graphics`: `starting|ready|restarting|failed`
    - `inputKicks`, `inputKickAlert`, `pipelineRebuilds`, `sinkSwaps`
    - `droppedFrames`: from `dropped_frames`. The domain measures it, so it's always present.
    - `rung`: `full|overlay_half_rate`. (Amended 2026-10-18, Owed 20: `fast_preset` is gone from
      both C05's `Rung` and the contract; web answers a `fast_preset` beat with a 400)
    - `scoreStale`: only if `Some`
    - each `Measured` field: only if `Some`

    It never inserts a key whose value is unknown.
  - Errors are formatted as `format!("heartbeat transport: {}", redact(&e.to_string()))`, and the
    token is never interpolated. Request URLs contain only the session id. That's not a secret, but
    still pass every logged URL through `redact`.

- [ ] **Step 8: Run, expect a PASS.**
  `cargo test -p adapters -- --nocapture 2>&1 | grep -E "test result|checked|validated"`. Paste the
  output.

- [ ] **Step 9: Mutate once each.**
  1. Make `heartbeat_wire` emit `"fps": 0` when it's `None`. Expected red:
     `unmeasured_values_are_absent_not_zero`.
  2. Make `parse_session` accept passthrough. Expected red:
     `composed_fixtures_decode_and_passthrough_is_refused`.
  3. Change one byte in a vendored fixture. Expected red: `vendored_files_match_their_pins`. Restore
     the fixture with `git checkout`.

- [ ] **Step 10: Commit.** `feat(control): vendored relay-job.v1 and the HTTP control plane (C07)`.
  The body records the web sha and the three mutants.

## Acceptance

- Pins are green.
- Every heartbeat a real supervisor emits validates against the vendored schema, with a non-zero
  count.
- The passthrough session is refused.
- The token never appears in errors.
- 3 mutants went red.
- (Amended 2026-10-18, Owed 13) The contract test's script feeds a growing `OutputProgress`
  total, never `OutputConnected`/`OutputAck`. It asserts no `SwapSink` and prints its progress
  count, failing on 0. The frozen-total mutant went red, making 4 mutants red.
- (Amended 2026-10-18, Owed 24) The vendored `relay-job.v1.json` hashes to `fc99d448…dc50` or a
  later bump, and the `rung` enum is exactly `full|overlay_half_rate`.

## Final heartbeat with the exit reason (owner ruling 2026-10-15)

`heartbeat_wire` emits `exitReason` (snake_case of `ExitReason`) and `exitDetail` only when
`report.exit` is `Some`. Otherwise both keys are **absent**, never null and never "".
`exitDetail` = `redact(detail)`, truncated to **100 Unicode scalar values** (Rust `chars().take(100)`). R2002's published contract caps it at `maxLength: 100` code points, so it always fits web's zod limit of 200 UTF-16 units (F-006; #966 review M1). Also: the exit fields are valid only on a `state: "stopped"` beat (the contract's if/then).
The final beat is **best-effort**: one attempt, 2 s timeout, never retried. Its failure is logged
(redacted) and never delays `Exit` (amended 2026-10-18, wave gate 1 review I4: by more than this 2 s
timeout; C08 flushes the final beat before `run()` returns, bounded by it). A 404 reply to it is not
an error, and neither is a 410 (web refuses every beat on a terminal session with 410).

Tests:
- The contract test's sweep includes the 6 exit beats, and every one validates against the vendored
  `heartbeat` def (which gains both fields in R2002).
- `exit_detail_is_redacted_and_capped`: a detail containing an `rtmps://…/live2/<key>` URL and 500
  chars, half of them emoji. The wire carries no key, and its length is ≤ 100 chars.
- `non_final_beats_have_no_exit_keys`: assert absence, not null.
- **Mutant:** skip `redact` on `exitDetail`. Expected red: the redaction test.

## Amendment 2026-10-18 (owed checks become acceptance criteria)

Owner, 2026-10-18, in chat, relayed by the coordinator: "apply rec 4-9". Every owed check is an acceptance criterion in its owning brief.

**Owed 13: the contract test feeds byte progress, not acks.** C04 replaced `OutputConnected` and
`OutputAck` with one event, `OutputProgress { gen, total_bytes }` (C06 routes it to
`output.on_progress`). The acked bytes stay 0 on CF (F-005).
- Step 5's `script` (the line with `Event::OutputConnected { gen: 1 }`) sends `OutputProgress {
  gen: 1, total_bytes }` with a total that **grows** on every send, by a frame's worth of bytes
  every 1 s of script time.
- A frozen total stall-swaps the output to generation 2, and the script's generation-1 events go
  stale behind a green pin test.
- The test asserts that no `SwapSink` effect appears in the run, and it prints the number of
  progress events sent. Zero sent is a failure.
- **New mutant:** freeze the total at its first value. Expected red: the script sees a `SwapSink`.

## Amendment 2026-10-18 (wave gate 1 review of C06, PR #24; the coordinator's decisions)

**A heartbeat 410 is its own result, not a transport error** (review I5). Web answers every beat on
a terminal session with `410 SESSION_ENDED` (`seazn.club` `stream-sessions.ts` `heartbeat`). C06
now has `Event::HeartbeatRefused(HeartbeatRefusal::Gone)`, which exits 0 at once with no hold, no
drain and no beat. Without it the compositor kept streaming, and spending Input B and host minutes,
after web had ended the session.
- `ControlPlane::heartbeat` (Interfaces) returns a result that tells a 410 apart, e.g.
  `Result<Desired, HeartbeatError>` with `HeartbeatError::{Gone, Transport(String)}`; the variant
  names are yours. **Only 410 is `Gone`.** A 401, a 404, a 5xx and a network error stay transport
  errors (C08 maps them to `HeartbeatFailed`): a 401 or a 404 on a beat has no ruling, and the
  domain's `HeartbeatRefusal` has no variant for them.
- Test (`control_http.rs`): `heartbeat_410_is_gone_and_other_refusals_are_transport`, a table over
  410 (Gone) and 401, 404, 503 (transport), each asserting the error text carries no token.
- **Mutant:** classify a heartbeat 410 as a transport error. Expected red: that test.

**`exitDetail` now has a producer** (Owed 27, option a). The domain's `ExitInfo.detail` is a fatal
error's text, verbatim from `Event::Fatal { detail }` (C08 maps it from the error), and `None` on
every other exit. This section's rule stands: `exitDetail` = `redact(detail)` capped at 100 chars,
and it is **absent** when `detail` is `None` **or empty** ("absent, never ''").
- The contract test's sweep drives a real `Event::Fatal { detail: Some(<a URL with a key and 500
  chars>) }` through a real `Supervisor`, so `exit_detail_is_redacted_and_capped` is fed by the
  domain's own beat, not a hand-built report.
- New test `an_empty_detail_is_absent`: a fatal beat with `detail: Some("")` has no `exitDetail`
  key.

## Do not touch

`contracts/` by hand (only the script writes it), and the domain crate.

## Output cap

Under 15 lines.
