# R2 plan re-review (Opus), 2026-10-14

Scope: checks that each fix for `reviews/2026-10-14-plan-review-fable.md` F1–F20 is correct and complete,
and whether the fixes added new inconsistencies. This is not a fresh full review. Owner rulings are
fixed and are not re-argued here: F5 = option A, the web tasks are R2001–R2006, and the chaos suite
adds `input-impaired`.

**Judged against HEAD `103de69`.** That commit ("3 h soak, MediaMTX off the shelf, page rules tested
in R2005") landed while this review was running, after `9c6fc5e` and `86670b3`. It cut chaos to 10
cases (score-void and clock-skew moved to R2005 + C17), made C15 a single 3 h `soak.yml`, and took
R2001's docs step out to seazn.club PR #957. PR #957 is not on the local web `origin/main` yet (that
tip is `ca6e7a21a`, #955), so I could not check what it says.

Line numbers point at the brief files under `docs/tasks/`, unless a path is given.

## F1–F20

| # | Verdict | Evidence |
|---|---|---|
| F1 | FIXED | C12:85-89. I recomputed `2026-10-14T18:30:00.000Z` = `1_792_002_600_000` (day 20 740). The `.250Z` case catches a parser that truncates fractions. |
| F2 | FIXED | C02:157-160 (doc: "grace − 3 s") and C02:194 `drain_timeout_ms: 7_000`. The C12:95-96 guard holds (7 000 < 10 000), and C06:283-284 stays symbolic. |
| F3 | FIXED | C12:109-124 `DrainingFake { inner, tx }` sits in `wiring.rs`. The fields it wraps are public (`FakeProgramme { pub log, pub fail_start }`, C08:269), and `tx` exists before the adapters (C12:139). Draining exits on `OutputDrained` (C06:517). The bound is `< 3 s + drain_timeout_ms` (C12:121). |
| F4 | FIXED | C03:236-242: the tolerance is `< period()`. Simulated: recovery from outage 1 takes 1 042 ms and from outage 2 takes 1 041 ms. The comment's numbers 8 019 / 18 018 are correct. |
| F5 (A) | FIXED, with a consumer gap → N2 | C06:528-532 has the mapping. Traced C06:238-267: `Starting` → `Playing`/`Waiting` at `OutputConnected@10` → `Live` → `Frozen@1627` → `Reconnecting@4102` (bound 4 232) → `Stalled@4234`. The input stays `Connecting`, because the 5 000 ms connect timeout is never reached. Spec §5.3 (spec:226) and `_INDEX.md:34` are updated. R2003 test 7 uses the enum `"connecting"`, which matches R2002:82. No brief still assumes camera-live ⇒ playing. No C06 mutant (C06:708-716) targets the new rule; see N5. |
| F6 | FIXED, new gaps → N3, N4 | C09:71-87 `Stats`/`StatsSnapshot` uses `Option` types that match C07:55 `Measured`. `GstProgramme::stats() -> Arc<Stats>` matches C12:79 `ProcMetrics::new(stats)`. 4 stats tests (C09:133-138); counts are 14 tests / 5 mutants (C09:218, :223). |
| F7 | FIXED, minor gaps → N8 | C13:88 and C13:105-109 define `camera.resumes` from gaps > 1 s in `emit.jsonl` plus strip counters. That measures hysteresis independently of latency. Case 2 (C14:46) bounds every entry and requires at least 10 entries. |
| F8 | FIXED | R2002:136-155: two stores, allowlist keys, mutant 2 = `rung`. Web check: `sanitise.ts:10-36` lacks `input`/`output`/`programmePhase`/… today and skips them at `:53`. The credential regex (`sanitise.test.ts:46-47`) passes every new key. `outputs` was already allowlisted, so `output` adds no new leak shape. |
| F9 | **PARTIAL** → N1 | C13:142-152 moves netem onto MediaMTX egress (`sport 8890 … match ip dst <compositor ip>`), which is the right leg. But nothing defines `<compositor ip>`: no static `ipv4_address` in compose and no `getent hosts compositor` step. An implementer has to guess, and a wrong address leaves the filter matching nothing. The case also exists now (C14:54), but its anti-vacuity witness is unsound (N1). |
| F10 | FIXED | R2002:113-116 has a bare root. C07:112-117 `schema_def` builds a fresh wrapper with `$defs` + `$ref`, so no root keywords leak in. |
| F11 | FIXED | C04:139-142, :274-283, :293, :433-449: "failure f waits delay(f−1)". Simulated six failures: gaps [1000, 2000, 4000, 8000, 10000, 10000] = `want`. A connect timeout gives `Backoff{1, 6000}`. C06's no-frames rebuild lands at 6 139 (6 039 after connect), which fits its bound. Spec:160 says "1 s → 10 s". |
| F12 | FIXED | C08:52-57: `Debug` impl for `Ports` and derive on `Driver`. C01:57-61: the harness's `print_stdout` allow. |
| F13 | FIXED | C09:187-200: feeder EOS step, file-size acks, mutant 4. |
| F14 | FIXED | C10:34-38 and test row C10:74. |
| F15 | FIXED | R2004:71-74, plus the addition to test 4. |
| F16 | FIXED, new gap → N6 | R2006:14-19. Web check: `server/stream-panel-context.ts:37-45` returns `{entitled, relayEntitled, relayDisabled}`, and `drivers.ts:135` has `relayUnavailable`. |
| F17 | FIXED | R2001:20-32 refusal order matches web `server/relay/domain/session.ts:94-110` (9 refusals, including `phone_not_responding`). |
| F18 | FIXED | R2002 step 4 adds absent-key assertions. The `:380` anchor is right: `capture-contract.test.ts:51` (SHA table), `:379-380` (loop), `:45-47` (loader). |
| F19 | FIXED, new gap → N7 | C07:51-52 drops `maxDurationMinutes`, with the reason given. C07:229-232 refuses a `delay=` that disagrees with `overlayDelayMs`, and C07:182-188 tests it. C08:169's `0` placeholder is held to the fixture by C12:130-133. |
| F20 | FIXED | C14:100-103: mutant 3 comments out case 1's `/pause` and must fail on the anti-vacuity clause itself. |

Counts: 19 FIXED (5 of them with new gaps raised below), 1 PARTIAL (F9), 0 NOT FIXED, 0 FIX WRONG.

## New findings

### N1 [major] `input-impaired` (case 10) proves its impairment with a metric that SRT is built to hold flat

- **Where:** C14:54; C13:110; C13:142-152; C10:42-44 (`input_srt_latency_ms` = 1000).
- **Problem:** The anti-vacuity clause expects the `latency_windows_ms` p50 in 20–80 s to sit at least
  300 ms above the pre-20 s p50. SRT is TSBPD: the receiver releases each packet at sender timestamp
  + base offset + latency (1000 ms here). A one-way delay of 400 ± 100 ms stays inside that 1000 ms
  window, so packets still arrive in time and are released on the same schedule. Glass-to-glass latency
  barely moves, or moves by however much libsrt's slow, RTT-compensated drift tracer shifts the base
  time, and that amount comes from no declaration. Either the case reds on its own witness every run,
  which breaks C14's "three runs green" (C14 step 6), or it passes on drift luck. The 300 ms has no
  source in `thresholds.json` either, which breaks the brief's own rule (C14:13-17).
- **Evidence:** C14:54 asks for "p50 … ≥ pre-20 s p50 + 300 ms". C13:110 says the windows exist to
  "prove the impairment landed". C10:42-44 sets the fixed 1000 ms receive latency.
- **Fix:** Witness the fault where it is injected, not at the far end of a jitter buffer. Use
  (a) `tc -s qdisc show` on the netem child, whose `sent` and `dropped` both grow during 20–80 s, and
  (b) the compositor's SRT receive stats (`srtsrc` `stats` → `pkt-rcv-loss`/`pkt-retransmitted`, or the
  MediaMTX reader metrics), which must be > 0 in the window. Keep the latency windows as a reported
  number, not a pass gate. Close F9 in the same edit: give the compositor a static `ipv4_address` in
  compose, or have `netem.sh` resolve `getent hosts compositor`, and assert the filter matched
  (`tc -s filter` hit count > 0).

### N2 [minor] C17 row 3 expects "panel shows camera reconnecting", and no R2 task makes the panel show it

- **Where:** C17:72; R2006 (health chips read only fps, bitrate and beat); R2002 stores
  `programmePhase`/`input` in the sample `raw` and the event payload only.
- **Problem:** Under ruling A the session stays `playing`/`live` while the camera reconnects. Camera
  state now travels only in `programmePhase`/`input`. No R2 web task reads those fields into the panel.
  The existing panel has a phone-side read model (`lib/stream-session-view.ts` `reconnectReasonOf`,
  built from capture-app beats). It may cover the composed case, but that is unverified, and no brief
  says it does. If it doesn't, the C17 row has no producer and will be judged against something that
  isn't built: an inert seam (AGENTS.md class 1).
- **Fix:** One of these: (a) R2006 renders `programmePhase ∈ {frozen, reconnecting}` as the camera
  chip, with a test that folds a real R2002 composed beat into it; or (b) C17 row 3 names
  `reconnectReasonOf` as the source, and R2006 adds a test that it fires for a composed session.

### N3 [minor] `encoderSpeed ≥ 1.0` in C14 case 6 is a coin flip against C09's own band

- **Where:** C14:50 (case 6, "after stress ends, `encoderSpeed ≥ 1.0`"); C09:87, C09:137-138
  (speed is the PTS advance over a 1 s window; healthy is `0.9..=1.1`).
- **Problem:** At real time a 1 s window holds 29, 30 or 31 frames, which gives 0.967, 1.0 or 1.033.
  "≥ 1.0" fails about a third of healthy windows. C09 already accepts 0.9 as healthy, so the two
  briefs disagree on what "real time" means, and the C14 literal comes from no declaration.
- **Fix:** Put one `encoder_speed_realtime_min` (e.g. 0.95) into `Thresholds`, export it through
  `thresholds.json`, and have both C09's band floor and C14 case 6 read it.

### N4 [minor] `ProcMetrics` copying the `Stats` snapshot into `Measured` is not tested before C14

- **Where:** C12:46-52, C12:79, C12:98-99 (the metrics tests cover only CPU/mem); C09:133-138 (tests
  cover the `Stats` side only).
- **Problem:** Each side of the F6 seam has tests, but the join between them doesn't. A `ProcMetrics`
  that never copies `fps`/`bitrate_kbps`/`egress_bytes`/`encoder_speed` passes C09 and C12. It's caught
  only at C14, as a heartbeat with absent fields, which is the most expensive place to find it.
- **Also:** C09's Files list (C09:41) doesn't add `pub mod stats` to `adapters/src/lib.rs`, and the
  plan's adapters file list leaves out `stats.rs`.
- **Fix:** Add a C12 test: build `Stats`, `record` known values, call `ProcMetrics::sample()`, and
  assert all four fields equal the snapshot. Mutant: drop one field copy, which must go red. Add the
  `lib.rs` and plan lines.

### N5 [minor] R2003 test 7 can't fail in its Step 2, isn't in the command or Files, and has no mutant

- **Where:** R2003:107-115 (test 7); R2003:68-77 (Files); R2003:117-119 (Step 2 command); R2003:122-125
  (3 mutants); R2003:136 ("8 new tests").
- **Problem:**
  - Web already promotes on `body.state === "playing"`: `server/usecases/stream-sessions.ts:2138-2142`
    sends `callback_playing`, and `relay/domain/runner.ts:235` and `:247` signal `went_live`. So test 7
    is green before any R2003 change, and "Step 2: expect a FAIL" is wrong for it.
  - It runs "through the real heartbeat route", but neither the Files list nor the Step 2 command
    includes that route's test file. The command is `{runner,expiry,runner-fly,config}.test.ts`.
  - The setup leaves out two preconditions. `callback_playing` is handled only from runner `booting`
    or `playing` (`runner.ts:233-247`; other states are null). And a composed session can exist only
    if R2001's `composedUnavailable()` is false, so the test needs `RELAY_IMAGE` set.
  - No mutant kills it. The same gap exists on the compositor side: no C06 mutant (C06:708-716)
    targets the ruling-A mapping.
- **Fix:** Name the heartbeat route test file in Files, Step 2 and Step 7. Mark test 7 "green on base,
  a regression pin". State both preconditions: runner `booting`, and `RELAY_IMAGE` set. Add mutant 4:
  gate `callback_playing` on `programmePhase === "live"`, which test 7 must red. On the C06 side, add
  a mutant that maps `Playing` only when the phase is `Live`, which
  `beat_state_follows_output_and_programme` must red.

### N6 [minor] R2001's new `composedUnavailable()` lives in a file R2001 doesn't list, and only one disjunct is tested

- **Where:** R2001:59-61 (the predicate in `server/relay/drivers.ts`); R2001:43-52 (Files leave out
  `drivers.ts` and any `drivers` test); R2001:101 (one mutant: refusal order).
- **Problem:** The predicate is `relayUnavailable() || RELAY_IMAGE unset`. No test pins each side
  alone, so `return relayUnavailable()` or `return !RELAY_IMAGE` survives. R2006 reads the same
  predicate, so the gap carries into the UI.
- **Stale after 103de69:** The R2001 title (R2001:1, "and repoint the docs") and plan:176 ("+ docs
  pointer") still describe the docs step that moved to #957.
- **Fix:** Add `server/relay/drivers.ts` to Modify. Add a 2×2 table test (relay up/down × image
  set/unset), and mutate each disjunct once. Retitle R2001 and update plan:176.

### N7 [minor] The `delay=` agreement is defined only for the happy path

- **Where:** C07:229-232; R2002:117-128 (fixtures); C13:42, C13:58-59 (fake control rewrites
  `overlayUrl` to harness hosts, and the harness page takes `?delay=`).
- **Problem:** The rule doesn't say what happens when `overlayUrl` has no `delay=` at all: refuse,
  treat as 0, or trust `overlayDelayMs`? R2002 doesn't say the `session-composed-*.json` fixtures must
  carry `delay=` equal to `overlayDelayMs`, so a fixture that leaves it out fails the C07 decode or
  hits the open question. C13's rewrite doesn't say it keeps `delay=`. A rewrite that drops it turns
  every harness run into exit 78 (refused), not a media test.
- **Fix:** C07: an absent `delay=` is refused as `WrongMode` (two authorities must both be present),
  with a test. R2002: the fixtures carry `delay=<overlayDelayMs>`, and a fixture test asserts they
  agree. C13: the rewrite keeps the query string, and asserts the rewritten URL still parses.

### N8 [minor] `resumes` is undefined for 1–3 s gaps, and C14 mutant 1 can move its own oracle

- **Where:** C13:105-109; C14:46, C14:96-98; C14:83-84 (`thresholds.json` drift check).
- **Problem:**
  - (a) An entry exists for any gap > 1 s, but the slate only appears after 3 s (`slate_after_ms`). A
    1–3 s gap has no "slate span that covers the gap", so `first_shown_counter` is undefined. No
    current case makes such a gap, but the report generator still has to pick something.
  - (b) Mutant 1 sets `resume_after_ms` to 0 in a scratch build. Case 2's floor reads
    `$t.resume_after_ms`. If that scratch build regenerates `thresholds.json`, which the drift check
    pushes toward, the floor drops to 0 with the mutant and case 2 stays green.
- **Fix:** (a) Define gaps shorter than `slate_after_ms` as "no slate expected": `suppressed_ms`
  absent, and `counter_regressions` still enforced. Or limit `resumes` to gaps ≥ `slate_after_ms`.
  (b) Mutant 1 runs against the **committed** `thresholds.json`, never a regenerated one. Say so in
  C14:96-98.

### N9 [minor] `egress_bytes == final file size` needs a final poll at EOS

- **Where:** C09:135 (`stats_egress_and_bitrate_follow_acked_bytes`); C09:87 (1 s poll).
- **Problem:** The stats poll every 1 s, and `filesink` buffers. Read straight after the run, the last
  snapshot can lag the file by up to one second of bytes, plus whatever is still in the sink buffer.
  An exact-equality assert then flakes.
- **Fix:** Take a final stats sample after EOS reaches the sink (or after the C09:187-200 drain
  completes), and assert equality against that sample. If no final sample is taken, assert within one
  poll interval of bytes, derived from the bitrate.

### N10 [minor] Text left stale by the rename and 103de69

- `_INDEX.md:33`: "A 12th chaos case". There are 10 cases now.
- C14:56: "not in spec §9.4's original table". Spec §9.4 now lists "Camera uplink impaired", so the
  sentence is out of date.
- C13:92, C13:112-114: the report fields `overlay.late_frames_max` and `future_dated` no longer feed any
  case, since void and skew moved to R2005. C13's acceptance still requires "every field populated".
  Either drop them or mark them report-only.
- Plan:41-42: "Tasks `W*` land in … Every W task obeys" was missed by the rename.
- Worktree and scratch names still say w0x: R2001:5 `w01-composed-hazard`, R2001:81 `/tmp/w01-a.json`,
  R2002:4, R2003:4 and R2003:118 `$SCRATCH/w03.json`, R2004:4, R2005:4, R2006:4. These are cosmetic.

## Checked and fine

- Counts: C06 has 15 scenarios. C04 has 18 tests and 5 mutants. C09 has 14 tests and 5 mutants. C07
  has 3 mutants. C08 has 3 mutants. R2003 test 8 derives `2` from `RUNNER_INPLACE_RESTARTS`.
- C14 at HEAD: 10 cases, the cases dir list (C14:71) and `run-chaos` agree. Mutant 2 → case 8
  (`cef-killed`) is the right pairing.
- C15 3 h arithmetic: 36 dropouts and 3 disconnects. The job timeout (run minutes + 40) is under the
  360-minute Actions cap.
- Plan file structure reflects 10 cases and `soak.yml` (apart from N4's `stats.rs` line).
- The R2002 heartbeat enums match C07:233-242 wire values exactly. `relayOffer` is server-side, so it
  adds no client-bundle import.
- The rename doesn't break any path outside `docs/reviews/`. ADRs, the README and the spec are clean.
  The old review's W0x paths are historical, which is expected.
- C02's `drain_timeout` headroom is fine. C12's fake-run bound has no margin above `3 s +
  drain_timeout_ms`, but it is not a real risk (a fake drain is near-instant).
- C01's workspace `members` leave out `crates/harness`. C13 adds it, so that is fine.
