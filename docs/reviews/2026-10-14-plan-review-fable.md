# R2 compositor plan review (2026-10-14)

> Note (orchestrator): the web briefs were renumbered after this review. W01–W06 here = R2001–R2006 now. Resolution per finding is in the commit that follows this file.

Reviewer: `reviewer` subagent (Fable). Scope: the plan `docs/plans/2026-10-14-r2-compositor-plan.md`,
the spec, ADRs 0001–0006 and all 24 briefs (C00–C17, W01–W06) at `docs/r2-spec` HEAD `4120a0e`,
pinned against `onryde/seazn.club` main `09290de3c`. Docs only; nothing was run or edited in either
repo. Every web-repo line cited below was opened, not grepped.

Counts: **3 blockers, 6 major, 11 minor.**

---

## Blockers

### F1 [blocker] C12 freezes a wrong epoch into `parses_r1_iso_deadline_with_millis`

Where: `docs/tasks/C12-binary-and-image.md:86-88`.

Problem: the literal the brief tells the implementer to hard-code is 26 h off. `2026-10-14T18:30:00.000Z`
is **1_792_002_600_000** ms (checked with `date -u -j` and Python `datetime`); the brief says
1_791_909_000_000 (= 2026-10-13T16:30:00Z). The brief also forbids deriving the value from the parser,
so an implementer who follows it to the letter ships either a parser that is wrong by 93 600 s
(the Machine's hard stop fires a day early, before the session's wall clock) or a test rewritten to
match the parser — both are the "wrong rule frozen in as correct" shape TEST-STRATEGY forbids.

Evidence: `Compute the expected value from the literal date arithmetic in the test (1_791_909_000_000); don't call the parser to get it.`

Fix: replace the literal with `1_792_002_600_000` and show the arithmetic in the brief
(`20_740 days × 86_400_000 + 18.5 h × 3_600_000`), or have the test compute it from `time::Date`
component arithmetic (not from the parser under test). Add a second case whose millis are non-zero
(`…:00.250Z`) so a parser that drops the fraction can be caught.

### F2 [blocker] C02's `drain_timeout_ms` default equals the R1 stop grace; C12's own guard test fails on it

Where: `docs/tasks/C02-domain-foundations.md:192` vs `docs/tasks/C12-binary-and-image.md:36-38, 94-95`; web
`apps/web/src/server/relay/config.ts:141` (`RUNNER_STOP_GRACE_SECONDS = 10`).

Problem: C02 defaults `drain_timeout_ms: 10_000`. C12 says the drain must sit *below* the 10 s grace and
pins it with `Thresholds::default().drain_timeout_ms < R1_STOP_GRACE_MS` where `R1_STOP_GRACE_MS` is
10 000. `10_000 < 10_000` is false: the C12 test is red against C02's default, and the real-world hazard
the test exists for is live — Fly sends SIGINT, waits `RUNNER_STOP_GRACE_SECONDS`, then SIGKILLs. A
drain that times out at exactly the grace is a drain that the kill races every time the EOS is slow.

Evidence: C02: `drain_timeout_ms: 10_000,`; C12: `(C06 drain_timeout_ms, which must be below the 10 s grace; assert that in a test here` and `drain_timeout_fits_inside_r1_stop_grace: Thresholds::default().drain_timeout_ms < R1_STOP_GRACE_MS`.

Fix: set C02's default to leave real headroom (`7_000` or `8_000`), state the derivation in C02 ("≤ grace
− 2 s for the EOS to reach the sink and the process to exit"), and keep C12's guard. C06's
`drain_times_out` test (`C06:270-271`) uses `th.drain_timeout_ms` symbolically, so it moves with the
constant.

### F3 [blocker] C12's fake-run test demands exit "within 6 s" but the fake programme can never drain

Where: `docs/tasks/C12-binary-and-image.md:106-116`; `docs/tasks/C08-app-event-loop.md:126-134`
(`FakeProgramme`); `docs/tasks/C06-domain-supervisor.md:15, 505`.

Problem: the run is driven with a deadline 3 s ahead. At the deadline the supervisor enters `Draining`
and exits only on `OutputDrained` or after `drain_timeout_ms`. C08's `FakeProgramme::drain` just logs
`Call::Drain` — it has no channel to send `OutputDrained` — so the process exits at
3 s + `drain_timeout_ms` (10 s today, F2; 7–8 s after the fix). "Exit within 6 s" cannot hold under either
value. The 60 s smoke variant with its 75 s bound happens to fit only because 60 + 10 ≤ 75. An
implementer will either loosen the bound without understanding why or make the fake drain
instantaneous and hide the real drain path.

Evidence: C12: `exit code 0, no earlier than 2.5 s and within 6 s (the deadline, not an early refusal)`; C08: `fn drain(&mut self) { self.log.push(Call::Drain) }`.

Fix: either give the binary's fake programme an event sender so `drain()` posts `OutputDrained` on the
next tick (then the 6 s bound is honest and the test also witnesses the drain path), or express the
bound as `deadline + Thresholds::default().drain_timeout_ms + slack` in the test, derived, never a
flat 6 s (AGENTS.md class 20: a flat timeout beside a derived cost is a latent red). Say which in the
brief.

---

## Major

### F4 [major] C03 `second_outage_behaves_like_the_first` cannot pass with the brief's own `drive` helper

Where: `docs/tasks/C03-domain-programme.md:126-141, 236-241, 288`.

Problem: `drive` walks a single global 33 ms grid from 0. The first frame ≥ 8 000 lands at 8 019
(243 × 33); the first ≥ 18 000 at 18 018 (546 × 33). Live is reached one `resume_after_ms` run later on
the same grid, so `a = 1 042` and `b = 1 041`. `assert_eq!(a, b)` fails by one millisecond of grid
aliasing, not by any defect in the table. The sibling tests already anticipate this (`live <
resume_after_ms + 2 * period()`), this one does not. "Expected: 13 passed" at Step 4 is therefore 12.

Evidence: `let a = first(&out, 8_000, Emit) - 8_000; let b = first(&out, 18_000, Emit) - 18_000; assert_eq!(a, b);`

Fix: either place the outage boundaries on grid multiples (e.g. `7_986`/`17_985` — ugly) or assert
`(a as i64 - b as i64).abs() <= period() as i64` with a comment naming the aliasing. Keep the test: it
is the only one proving the second outage is not slower than the first.

### F5 [major] `playing` requires the camera to be Live, so a composed session on air with a slate never goes live in R1

Where: `docs/tasks/C06-domain-supervisor.md:512-517` (`health()`), spec `§5.3` (`state … mapped from the
programme phase`); web `apps/web/src/server/usecases/stream-sessions.ts:2123-2160` (`heartbeat`:
`callback_playing` only on `body.state === "playing"`), `apps/web/src/server/relay/domain/session.ts:231`
(`went_live` only from `warming`), `apps/web/src/server/relay/config.ts:47`
(`WARMING_TIMEOUT_MINUTES = 10`), `session.ts:394-404` (`warming_timeout → machine_boot_timeout`).

Problem: C06 reports `Playing` only when the output is connected **and** `programme.phase() == Live`.
With the output connected and no camera yet the first beats say `stalled` (`output_ever_connected`
is true from the first ack), never `playing`. On the web side only `playing` promotes `booting →
playing` and `warming → live` (one credit consume). So: organiser goes live before the phone
publishes; the Machine is pushing the WAITING slate to the destination (YouTube shows the club
"live"); the panel shows *warming*; no credit is consumed; and if the camera takes longer than
10 min the session is **failed `machine_boot_timeout`** and the Machine destroyed while it was
correctly on air. Not a brief-vs-brief contradiction (the spec says the same), so flagged as a
cross-repo semantic the plan has not decided.

Evidence: C06: `Lifecycle::Running if self.output.is_connected() && self.programme.phase() == ProgrammePhase::Live => BeatState::Playing, Lifecycle::Running if self.output.is_connected() || self.output_ever_connected => BeatState::Stalled,`.
Web: `if (s.state !== "warming") return { next, events, effects }; return { next: { ...next, state: "live", startedAt: now }, … consume_credit }`.

Fix (owner question, recommendation attached): define `playing` = *output connected* (the programme is
on air, slate or picture) and carry the camera's state in the new `programmePhase`/`input` fields W02
adds — that is what R1's `live` means for the destination and what the credit pays for. If the owner
instead wants `live` = camera up, W03 must (a) stop `warming_timeout` from failing a session whose
beats are arriving, and (b) say in the panel that the destination is receiving a slate. Either way
C06's `beat_state_follows_output_and_programme` test and W03's rule need the same sentence.

### F6 [major] C12's heartbeat metrics are an inert seam: nothing in C09 produces the counters C12 reads

Where: `docs/tasks/C12-binary-and-image.md:52, 80` vs `docs/tasks/C09-gst-programme-output.md:59-66`
(Produces) and `:108-118` (only `builds()`, `overlay_pushes()`); consumers
`docs/tasks/C14-chaos-suite.md:50` (case 6 `encoderSpeed ≥ 1.0`), `docs/tasks/C15-soak.md:40`
(`droppedFrames`), web `apps/web/src/server/usecases/stream-sessions.ts:1981, 2030` and
`apps/web/src/lib/stream-session-view.ts` `healthChips` (panel chips read `fps`, `bitrateKbps`).

Problem: C12 says `fps`, `bitrate_kbps`, `egress_bytes`, `encoder_speed` are "read from `GstProgramme`'s
counters through a shared `Arc<Stats>` (C09 exposes the counters it already keeps; add a `stats()`
accessor there only)". C09 keeps no such counters: its Produces block declares `FrameStore`,
`GstProgramme`, `GstOutput`, and the only test-visible counters are `builds()` and `overlay_pushes()`.
Acked egress bytes live in the **output** pipeline's `rtmp2sink stats` poll (C09 step 7.5), encoder
speed in x264's output rate vs wall clock, bitrate in bytes/s — none is specified, none is tested, and
no brief owns the arithmetic. Result: every heartbeat carries `None` for the four fields, C14 case 6's
`encoderSpeed ≥ 1.0` and C15's dropped-frame criterion are judged on nulls, and the organiser's R1
health chips stay empty in composed mode (they already render `fps`/`bitrateKbps` for passthrough).

Evidence: C12: `read from GstProgramme's counters through a shared Arc<Stats> (C09 exposes the counters it already keeps; add a stats() accessor there only)`. C09 Produces: `pub fn set_camera… set_overlay… push_audio…` and tests `builds() == 1`, `overlay_pushes() == 30` — no `fps`, `bitrate`, `egress`, `encoder_speed` anywhere in the brief.

Fix: give C09 a `Stats` type and a **measured** definition per field (fps = frames pushed to the
programme per 1 s window; bitrate = bytes acked per window; egress = cumulative acked from
`rtmp2sink stats`; encoderSpeed = encoded-frame PTS advance ÷ wall clock, with `1.0` meaning real
time), each with a test that drives N frames and asserts the counter, and a test that `encoder_speed`
drops below 1 when the encoder is starved (the C14 case-6 witness). C12 then only wires. If the owner
prefers to defer, C12 must send the fields **absent**, W02's schema must not require them for
composed, and C14 case 6 / C15 criterion 4 must be rewritten not to rely on them.

### F7 [major] C14 case 2's hysteresis assertion is vacuous over 5 s heartbeats; mutant 1 cannot go red

Where: `docs/tasks/C14-chaos-suite.md:46, 92-93`; `docs/tasks/C02-domain-foundations.md` (`resume_after_ms`
1 000); W02 `heartbeatIntervalMs` 5 000.

Problem: the case asserts "across heartbeats, no `programmePhase` change < `resume_after_ms` after the
previous one". Heartbeats are sampled every 5 000 ms, and the only phase observations the checker has
are those samples, so two consecutive observations are always ≥ 5 000 ms apart > 1 000 ms. The clause
is true for any implementation, including `resume_after_ms = 0` — the mutant the brief expects to red
it. The case can still fail on `output_outages` or `counter_regressions`, but not on hysteresis.

Evidence: `across heartbeats, no programmePhase change < resume_after_ms after the previous one` and `Set resume_after_ms to 0 in a scratch build. Expected red: case 2`.

Fix: judge hysteresis on per-frame evidence the harness already records — C13's `strip.rs` output or
the recorded fmp4: derive the slate/picture spans from the frame strip and assert every
picture span after a slate is ≥ `resume_after_ms` long, with the count of spans checked ≥ N (anti-vacuity).
Alternatively have the compositor count phase transitions (`phaseChanges`, `minPhaseGapMs`) in the
heartbeat; the second is cheaper but is "the code reporting on itself" — the strip is the independent
oracle.

### F8 [major] W02's "samples keep a raw jsonb, so the new fields ride in it" is a false premise — `raw` passes the sanitise allowlist

Where: `docs/tasks/W02-relay-job-contract.md:135-140, 155-158`; web
`apps/web/src/server/usecases/stream-sessions.ts:2131` (`last_heartbeat = sql.json(body)`),
`apps/web/src/server/relay/telemetry.ts:121-129` (`recordSample(... raw: body)` → `sanitise()`),
`apps/web/src/server/relay/sanitise.ts:10-36` (`ALLOWED_KEYS`), `:56-57` (`if (!ALLOWED_KEYS.has(k)) continue;`).

Problem: there are two stores. `fixture_stream_sessions.last_heartbeat` keeps the whole body;
`fixture_stream_samples.raw` keeps `sanitise(body)`, which **drops every key not in `ALLOWED_KEYS`**.
Of W02's additions only `droppedFrames`, `fps`, `bitrateKbps`, `cpuPct`, `memMb`, `encoderSpeed`,
`state` are allowlisted; `programmePhase`, `input`, `output`, `graphics`, `inputKicks`,
`inputKickAlert`, `pipelineRebuilds`, `sinkSwaps`, `rung`, `scoreStale` are silently discarded from
the per-beat history. W02's Step 4 "read the stored row and assert each new field round-trips" passes
against `last_heartbeat` and never sees the samples gap, so the telemetry C14/C15/C17 are meant to
read back over time (`inputKicks` trend, `rung` history) exists only as the latest beat.

Evidence: W02: `If samples keep a raw jsonb of the body, the new fields ride in it, and you only need to assert…`; sanitise.ts: `// ALLOWLIST: add a key here when a new fact is worth keeping; nothing else survives.`

Fix: W02 extends `ALLOWED_KEYS` with the new names (none is a credential; `input`/`output`/`graphics`
are nested objects whose leaves are `protocol`/`connected`/`status`-style words — add them too) and
extends `sanitise.test.ts`'s "no allowed key names a credential" pin. The round-trip test reads
**both** `last_heartbeat` and the latest `fixture_stream_samples.raw` and asserts each new field in
each; a mutant that removes one key from the allowlist must red it.

### F9 [major] C13's `netem.sh in` cannot impair inbound SRT media, and no chaos case impairs the input at all

Where: `docs/tasks/C13-harness.md:14, 19-24, 135-137`; `docs/tasks/C14-chaos-suite.md:40-54` (case table);
spec `§9.3:354-355`.

Problem: `tc` qdiscs on the compositor's netns act on **egress**. A `u32` filter on `dport 8890` in the
compositor's namespace matches packets the compositor *sends* to MediaMTX's SRT port — SRT control
and ACK traffic — not the media MediaMTX sends back (those arrive with `sport 8890`, on ingress, which
a plain `prio`+`netem` qdisc never sees without an `ifb` redirect). So "in" delays acks and may
provoke retransmits, but the 100–800 ms latency / 1–5 % loss the spec specifies on the camera leg is
never applied to the frames. And none of C14's 11 cases invokes `netem.sh in` — cases 3/4 are
"output" and "kick" shapes — so the input-impairment harness feature is inert and spec §9.3's input
impairment has no coverage.

Evidence: C13: `adds a prio qdisc with a u32 filter on dport 8890 (in) or 1935 (out), with a netem child.`; spec: `tc netem applies 100–800 ms latency, 1–5 % loss and jitter.`

Fix: for the input leg, either run the netem on the **MediaMTX** side's egress (filter `sport 8890`
on the mediamtx container, which needs the sidecar to share *its* netns) or use `ifb0` + `tc filter …
ingress … action mirred egress redirect dev ifb0` on the compositor netns (needs the `ifb` module in
the runner kernel — record as an F-id if the CI kernel lacks it). Then add a C14 case
`input-impaired` (latency 400 ms ± 100, loss 2 %, 60 s) with an anti-vacuity clause that the SRT
reader reported retransmits > 0 (MediaMTX exposes them in its metrics API), and judge `pts_gaps`
and `fps_mean` on the recording.

---

## Minor

### F10 [minor] C07 `schema_def` adds a root `$ref` to a document whose root is itself a schema

Where: `docs/tasks/C07-control-plane.md:110-115`; `docs/tasks/W02-relay-job-contract.md:110-113`; web
`scripts/gen-capture-contracts.ts:252, 277-283` (the generator W02 copies renders the twin at the
**root** and extras under `$defs`).

Problem: if W02's generator follows `gen-capture-contracts.ts`, the root of `relay-job.v1.json`
carries one twin's `type`/`properties`/`additionalProperties: false`. Setting `s["$ref"]` on that root
ANDs the referenced def with the root constraints (draft 2020-12 keeps sibling keywords beside
`$ref`), so validating `heartbeat` would also demand the root twin's shape and every C07 schema test
goes red on first run.

Evidence: C07: `let mut s = doc.clone(); s["$ref"] = serde_json::json!(format!("#/$defs/{def}"));`; W02: `docs/contracts/relay-job.v1.json (both schemas, under $defs: session, heartbeat, heartbeatReply`.

Fix: W02 states the root is **bare** (`$schema`, `$id`, `title`, `description`, `$defs` only — no twin at
root) and its generator differs from the capture one in exactly that; C07 builds
`json!({"$schema": doc["$schema"], "$defs": doc["$defs"], "$ref": "#/$defs/<def>"})` rather than
mutating the whole doc, so it is correct under either root shape.

### F11 [minor] C04 input backoff: the first retry waits 2 s, the brief and the PDF say 1 s, and the test name says 1-2-4-8

Where: `docs/tasks/C04-domain-links.md:11, 121-141`.

Problem: the link starts at `Connecting{attempt: 0}`; a loss there goes to attempt 1, whose delay is
`base × 2^1 = 2 s`. The comment in the test admits it ("first loss is from Connecting{attempt:0} ⇒
next attempt 1 ⇒ 2 s"), so the sequence actually asserted is 2, 4, 8, 10, 10, 10 while the test is
named `…_1_2_4_8_10_10` and the brief's summary says "from 1 s". Tests must not lie in their names
(AGENTS.md class 4); and a 1 s first retry is what the PDF budgets the 1.2 s reconnect on.

Evidence: `// first loss is from Connecting{attempt:0} ⇒ next attempt 1 ⇒ 2 s; derive expectation from backoff::delay`.

Fix: delay for attempt `n` = `min(base × 2^(n−1), cap)` with attempt counting from 1 (so attempt 1 waits
`base`), or start the first `Connecting` at attempt 0 and index the delay by *failures so far*. Rename
the test to whatever it actually asserts.

### F12 [minor] C01's lint set vs C08's hand-written `Debug`

Where: `docs/tasks/C01-workspace-scaffold.md:52-56, 270`; `docs/tasks/C08-app-event-loop.md:50-53`.

Problem: `missing_debug_implementations = "warn"` with `clippy -D warnings` makes a missing `Debug`
fatal. `Ports` holds five `Box<dyn …>` and `Driver` holds `Ports`, and C08 writes neither a derive nor
an impl; the trait objects cannot derive. `print_stdout = "deny"` also bites the harness bins
(`strip.rs`, fake camera) that legitimately print reports to stdout.

Evidence: `missing_debug_implementations = "warn"` … `cargo clippy --workspace --all-targets -- -D warnings`; `pub struct Ports { pub input: Box<dyn InputSource>, … }`.

Fix: C08 adds `impl fmt::Debug for Ports` (print the port names) and derives on `Driver`; C01 says
`print_stdout` is allowed per-crate for `harness/*` bins (`#![allow(clippy::print_stdout)]` with a
one-line reason).

### F13 [minor] C09 `drain()`: an EOS on the programme appsrcs does not by itself reach the output sink

Where: `docs/tasks/C09-gst-programme-output.md:27-34, 160-172`.

Problem: the programme and output are two pipelines joined by a ring and a feeder thread (step 7.4).
EOS on the programme appsrcs ends the *programme* pipeline; the output pipeline's `rtmp2sink` sees EOS
only if the feeder thread, after the ring runs dry, pushes EOS into the output appsrcs. The brief
states the outcome ("the output's EOS reaches the sink") without the step that makes it true, and
`drain()` is the path F2/F3 depend on. Also `file_target_reports_connected_then_acks` assumes an
`OutputConnected`/`OutputAck` source for `filesink`, but step 7.5 derives both from `rtmp2sink stats`.

Evidence: `drain(): EOS on the programme appsrcs. When the encoders flush through the ring, the output's EOS reaches the sink, and the bus EOS sends OutputDrained.`

Fix: step 7 adds "the feeder thread, on programme EOS and an empty ring, calls `end_of_stream()` on
both output appsrcs"; for `filesink` targets derive acks from the file's size (or a `fdsink`
byte count), and say so beside the test.

### F14 [minor] C10 always emits `passphrase=` even when the passphrase is empty

Where: `docs/tasks/C10-gst-input.md:34-35, 55, 64`.

Problem: the test listener uses an empty passphrase; an SRT URI with a present-but-empty
`passphrase=` is rejected by libsrt builds that validate passphrase length (10–79) when the key is
set, so the local harness test may fail for a reason unrelated to the code under test.

Evidence: `srt://{host:port}?mode=caller&streamid={stream_id}&passphrase={passphrase}&latency={ms}`.

Fix: omit the `passphrase` query key when the value is empty; add that as a one-line case to
`srt_uri_encodes_and_never_appears_in_logs` (empty → key absent).

### F15 [minor] W04's "if the session column is nullable" branch resolves to NOT NULL DEFAULT 0 — decide, don't branch

Where: `docs/tasks/W04-input-b-provisioning.md:65-72`; web `db/migration/deltas/V410__stream_sessions.sql:120`
(`overlay_delay_ms integer not null default 0`).

Problem: the column is not nullable, so "keep it nullable for passthrough" cannot happen without an
ALTER the brief does not budget. The premise that the column is never written is true (only read at
`stream-sessions.ts:2185`), so today every session carries 0 and passthrough would keep 0.

Evidence: W04: `Re-pin the session column's nullability. If it's nullable, keep it nullable for passthrough (null), and write it for composed.`; V410: `overlay_delay_ms integer not null default 0,`.

Fix: state the rule: passthrough writes 0 (the column's default; the field is unused in that mode),
composed writes the effective value; add a route test that a passthrough create leaves 0 and a
composed create writes the setting-or-default.

### F16 [minor] W06 pins `relayOffer` to a file that does not exist

Where: `docs/tasks/W06-organiser-composed-ui.md:14, 62`; real location `apps/web/src/server/stream-panel-context.ts:37`
(W01 pins it correctly).

Problem: `components/v2/stream-panel-context.ts` is not in the tree; `relayOffer` lives in
`server/stream-panel-context.ts` and returns `{ entitled, relayEntitled, relayDisabled }` where
`relayDisabled = relayEntitled && relayUnavailable()` (`drivers.ts:135`). The UI's "same authority"
for `composed_unavailable` must therefore extend this server function, not a client file.

Evidence: `relayOffer lives in components/v2/stream-panel-context.ts:37.`

Fix: correct the path in Context and Files; add `composedAvailable` (RELAY_IMAGE set and driver not
disabled — the same two facts W01's usecase reads) to `relayOffer`'s return.

### F17 [minor] W01 lists `admit()`'s refusal order wrongly

Where: `docs/tasks/W01-composed-hazard-fix.md:20-31`; web `apps/web/src/server/relay/domain/session.ts:94-110`.

Problem: the first check is `overlay_required` (relay target without overlay), then
`plan_lacks_overlay`, `plan_lacks_relay`, … The brief swaps the first two. Harmless for W01's own
rule (its new refusal goes first) but the brief's "composed + every other refusal also true → still
composed_unavailable" table will be written against the wrong neighbour order.

Evidence: `1. plan_lacks_overlay 2. overlay_required 3. plan_lacks_relay …`.

Fix: re-pin the order when writing the table test; assert the position of `composed_unavailable` by
constructing the admit input with *all* other refusals true, which is independent of the order.

### F18 [minor] W02 "stored as null, not 0" — absent heartbeat fields are absent keys, not nulls

Where: `docs/tasks/W02-relay-job-contract.md:158, 175`; web `stream-sessions.ts:2131`.

Problem: `last_heartbeat = sql.json(body)` stores the parsed body; a field the Machine did not send is
not a key in the JSON at all, and `row.last_heartbeat.fps` reads `undefined`, not `null`. A test that
asserts `toBeNull()` fails (or is written as `?? null`, which can no longer tell "absent" from "sent
null"). Also the drift-gate anchor `capture-contract.test.ts:49` is the fixture loader; the sha table
is at `:380`.

Evidence: `POST heartbeat-minimal.json and assert the new fields are stored as null, not 0.`

Fix: assert `not.toHaveProperty("programmePhase")` on the stored body and `0` is never substituted;
re-pin the sha anchor.

### F19 [minor] C07 `SessionConfig.overlay_delay_ms` and `max_duration_minutes` are parsed and consumed by nobody

Where: `docs/tasks/C07-control-plane.md:46-49, 217`; consumers: C11 reads the delay off the
`overlayUrl` (W04 puts it in the URL), C12 takes the deadline from `RELAY_DEADLINE_AT`.

Problem: two fields cross the wire, are decoded and typed, and nothing reads them — the inert-seam
shape AGENTS.md class 1 names. They also create a second authority for the deadline and the delay.

Evidence: `pub heartbeat_interval_ms: u64, pub overlay_url: String, pub overlay_delay_ms: u64, pub max_duration_minutes: u64,`.

Fix: either drop them from `SessionConfig` (W02 may keep them in the contract for the panel's sake)
or give each one reader with a test: `overlay_delay_ms` must equal the `delay=` in `overlay_url`
(refuse the session otherwise), `max_duration_minutes` is logged and must not exceed the env
deadline.

### F20 [minor] C14 mutant 3 is ambiguous about what "red" means for a no-op script

Where: `docs/tasks/C14-chaos-suite.md:95-96`.

Problem: a no-op `.sh` produces no `report.json`, so the `.jq` errors before any clause runs. That is
"red" at the shell level but proves nothing about the anti-vacuity clause; conversely a script that
runs the baseline with no fault yields a report whose counters are all zero, which is the case the
clause exists for.

Evidence: `Remove a case's anti-vacuity clause and make its script a no-op. It must still go red; if it stays green, the clause is missing.`

Fix: define mutant 3 as "run the case with the fault injection line commented out" (the report
exists, counters are zero) and expect the `.jq` to fail on the anti-vacuity clause specifically,
with its message echoed in the table.

---

## Not verified (no finding; the briefs already say re-pin, or the fact lives outside the tree)

- Which timestamps the overlay clock schedules against (server `at` vs device). W05 §3 instructs the
  re-pin; `use-live-fixture.ts:246-262` keys the delay buffer on browser `receivedAt`, which is neither.
- Fly Machines API `restart.max_retries` field name (W03 says re-pin against the docs).
- `rtmp2sink` `stats` field name and x264 `speed-preset` runtime changeability (C09 step 1 pins them).
- MediaMTX SRT `streamid` grammar (`read:cam` vs `publish:cam`) for the compositor's reader side in C13.

## Checked and fine

- C02 `Thresholds` field names used by C03–C06, C10, C11, C14 all match; window/backoff tests and
  their mutants trace correctly.
- C04 `InputLink`/`OutputLink`: 12 + 6 tests and 5 mutants trace green/red as stated (apart from F11).
- C05 Graphics (6) + Ladder (3) tests and 4 mutants trace correctly.
- C06: the 15 supervisor scenarios trace (beats at 0 / 5 016 / 10 032 / 15 048 / 20 064; stall
  rebuild at +6 006 ms); the property oracle is independent of the code under test; the collapse of
  spec §4.2 `Ready`/`Streaming` into `Running` is a documented deviation.
- C07 `Refusal`/`Desired`/`HealthReport` names match C06; `parse_session` keys match W02's schema
  (`sessionId`, `fixtureId`, `mode`, `input{url,streamId,passphrase}`, `target{url,streamKey}`,
  `output`, `heartbeatIntervalMs`, `overlayUrl`, `overlayDelayMs`, `maxDurationMinutes`).
- C08 driver scenarios trace (ticks 0..5, cap 31 pushes); port signatures match C09–C11 usage.
- W02's heartbeat additions match C07's wire set exactly, so the strict `RelayHeartbeat` schema
  (`schemas.ts:1666`) will not 400 a compositor beat.
- W03 invariant-1 reasoning holds against `observedFrom` and `createStarted`
  (`domain/runner.ts:103-110, 260-300`); exit codes 0/78/70 agree across C06, C12, W03; `restart:
  {policy: "on-failure", max_retries}` is the real API shape; W03:62-64 already owes the
  `StreamFailReason` enum + 4 locale strings for `refused`.
- W04 `overlayUrl` params (`sid`, `st`, `host=compositor`, `delay`) match what W05's page reads;
  `fixture_stream_inputs` has no role column and is unique on `(session_id, slot)` (V410:237) as
  W04 says; the session `overlay_delay_ms` "never written" premise is true; `srtPlayback` is not read
  by `createLiveInput` (ingest-cf.ts:232) as W04 says; the destination key stays on B's `addOutput`.
- W05: `layout.tsx` already has `html,body{background:transparent}` (its "false premise" claim is
  right); `relay-page` is minted at `stream-sessions.ts:2184` and consumed nowhere; the private
  fixture + cache-isolation case (W05 test 7) covers the "private fixture via public cache" hazard;
  `verifyRelayToken` refuses a malformed expectation first (`tokens.ts:80`).
- W01: `fixture-stream-panel.tsx:1140` hardcodes `mode: "passthrough"`; `drivers.ts:84` comment and
  `lazyRunner` throw-wrapping are as described; `RELAY_IMAGE` is absent from `config.ts` and both
  `fly*.toml` `[env]`, so C16's "re-pin whether it is a secret" instruction is correct.
- C00 secrets-in-argv guidance is consistent with C10/C12 (`target=<redacted>`, no logging crate).
- C12 `oom_score_adj -900` as non-root is acknowledged; C16 `flyctl secrets set --stage` + `secrets
  deploy` is a valid sequence.
- C11 / W05 readiness and score-stale channel are deferred to F-001 explicitly; C09 defers the
  `rtmp2sink` stats field and x264 preset changeability to its step 1 — both acceptable deferrals.
- Epoch arithmetic for every other date literal in the briefs was not needed; only C12's is wrong.
