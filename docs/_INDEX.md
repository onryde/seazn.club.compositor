# Relay compositor — programme index

The entry point for any session working on this repository. Read this file first, then
`docs/_RULES.md` (written alongside the R2 plan).

## Status

| Release | State | Docs |
|---|---|---|
| R1 control plane (`seazn.club`) | merged (#812, #902, #904) | `seazn.club/docs/superpowers/specs/2026-09-05-stream-overlay-prompts/_INDEX.md` |
| **R2 compositor** | design approved 2026-10-14; spec approved; plan written 2026-10-14, under review. `plans/2026-10-14-r2-compositor-plan.md`, briefs in `tasks/`. | `specs/2026-10-14-r2-compositor-design.md` |
| R3 native capture apps | separate programme | `seazn.club.capture` |
| R4 compositor remainder | not started (scope listed in spec §12) | — |

## Decision log

| Date | Decision | ADR |
|---|---|---|
| 2026-10-14 | Rust + GStreamer with two pipelines; `cefsrc` renders the overlay page. Supersedes Sept B2/B3. | 0001 |
| 2026-10-14 | Separate repo, which is also the docs home for R2. | 0002 |
| 2026-10-14 | R2 covers PDF phases 0–1 plus R1 wiring, opening with a spike gate. The rest is R4. | 0003 |
| 2026-10-14 | In-process Rust supervisor; Fly `on-failure` with max_retries 2. | 0004 |
| 2026-10-14 | 720p30 by default; 1080p30 behind a flag. | 0005 |
| 2026-10-14 | Measured default delay plus per-fixture override; SEI probe. | 0006 |
| 2026-10-14 | Plan: exit 78 (refused) never spends web's one retry. `on-failure` restarts it in place twice (harmless, each refuses in about 1 s), but web marks the session `failed(refused)` instead of creating `r<n+1>`. This was a spec gap, found while writing R2003. | — (plan, R2003) |
| 2026-10-14 | **Owner ruling:** void and clock skew are page rules, tested in R2005 (unit + e2e) and live in C17, not in chaos. Chaos is 10 cases, including `input-impaired` for spec §9.3. | — (C14, R2005) |
| 2026-10-14 | Plan: composed `target` is CF Input B (the destination key never reaches the Machine); `fixture_stream_inputs` gains `role`; Input B records, and Input A doesn't (pending R2004's recording-consumer re-pin). | — (plan, R2004) |
| 2026-10-14 | Plan: the overlay URL carries `sid` beside `st`, because `verifyRelayToken` needs an expected sid. The relay read of a non-public fixture never shares a cache entry with the public read. | — (plan, R2004/R2005) |
| 2026-10-14 | Plan: a Running input that delivers no frame for `input_stall_after_ms` (5 s, after the 3 s slate) is rebuilt, because CF can hold a silent playback socket open. The domain owns the rule, and adapters have no watchdog. | — (plan, C04/C06) |
| 2026-10-14 | Plan: the soak runs on two Fly Machines (the real image on the production guest, plus a harness Machine), so the fakes don't steal the compositor's CPU. | — (plan, C15) |
| 2026-10-14 | Plan: an image push is not a release. `RELAY_IMAGE` changes only through `promote.yml`, behind a required-reviewer environment. | — (plan, C16) |
| 2026-10-14 | Plan: the web tasks are numbered R2001–R2006 (owner request; previously W01–W06), so they don't collide with other programmes' W-waves. Compositor tasks keep C00–C17. | — |
| 2026-10-14 | Plan review (Fable, once): 3 blockers, 6 majors, 11 minors in `reviews/2026-10-14-plan-review-fable.md`. All fixed in the briefs; F5 was decided by the owner (option A, next row). Chaos case 10, `input-impaired`, covers spec §9.3's input impairment. | — |
| 2026-10-14 | **Owner ruling (F5, option A):** a composed session is `playing` once its output reaches CF, whether it's showing the slate or the picture. That's when it goes `live` and uses its credit. The camera's state travels separately, in `programmePhase`/`input`. | — (spec §5.3, C06, R2003) |
| 2026-10-14 | **Owner ruling:** the soak is 3 h (was 8 h). One GitHub Actions job drives it, and the media runs on Fly Machines on the production guest. | — (spec §9.5, ADR 0003, C15) |
| 2026-10-14 | **Owner ruling:** the fake Cloudflare is the off-the-shelf MediaMTX image (pinned), not custom code. It runs only on our CI and Machines, never a public instance. Chaos and soak stay on it because real stg would need a fake paired phone (admission requires one) and caps a session at 300 min. Real CF behaviour comes from C00/F-002 and is proven live in C17. | — (C13–C15) |
| 2026-10-14 | Plan re-review (Opus): 19 fixed, F9 partial, 1 major + 9 minor new in `reviews/2026-10-14-plan-rereview-opus.md`. All applied: case 10 witnessed at `tc` + SRT retrans (TSBPD holds latency flat), compositor fixed at 172.28.0.10, `ENCODER_SPEED_REALTIME_MIN` = 0.95 shared by C09/C14, ProcMetrics join test (C12), R2003 test 7 a regression pin + mutant 4, C06 mutant 4, R2001 2×2 predicate table, absent `delay=` refused, R2006 composed camera chip. | — |
| 2026-10-15 | **Owner ruling:** the compositor must not depend on Fly. ADR 0007 defines a 6-item host contract; Fly is allowed only in web's runner driver, the deploy workflows (one `RELAY_REGISTRY` variable) and the soak runner. `check-portable.sh` (C01) fails on any Fly reference in `crates/`/`docker/`. | ADR 0007 |
| 2026-10-15 | **Owner ruling:** C00 CPU gate moves from CEF-alone (≤1 core) to the full chain: item 5 at 720p30 ≤ 3.0 of 4 cores with encoder speed ≥ 0.95 is a gate; CEF > 2.0 cores is the item-1 runaway fail; CEF ≤ 1 is recorded only. | — (C00) |
| 2026-10-15 | **Owner ruling:** organisers are told to use YouTube stream latency **Normal** (burned-in score can't desync; least rebuffering; accepted cost: ~15–30 s behind the public page's live score). | — (C17 runbook) |
| 2026-10-15 | **C00 gate PASS** (items 1, 2-partial, 3, 5; `_FINDINGS.md` F-001..F-005, spike `spike/t00` @738eb88, never merged). 720p30 full chain 0.83 of 4 cores; SEI survives (ADR 0006 takes the "survives" branch). **`_FINDINGS.md` overrides any brief it contradicts.** Owed to the owner session: delay median, phone reconnect, YouTube go-live + gap tolerance, Poor-health cause with real footage. | F-001..F-005 |
| 2026-10-15 | **C00 owner session (real capture app, stg fixture 5):** delay in-chain ~3.1 s (n=2; separate reader reads ~300 ms high), provisional `overlayDelayMs` default 3100, n≥10 in-chain sample owed before C11 fixes it. App backgrounding: no gap seen (owner timing unconfirmed). YouTube "Poor" is NOT filler (0 % filler, flat 4.16 Mbps, recovered by itself); suspect CF Output→YouTube leg, direct-to-YouTube probe owed. YouTube kept the SAME broadcast across 5 s and 15 s gaps, but a **30 s gap started a NEW broadcast** (owner-confirmed); after the last frame it auto-ended ~60–90 s later. So the split threshold is **between 15 s and 30 s**: every R2 recovery path that the organiser's broadcast must survive has to finish **under 15 s**, measured as output-gap. That covers output reconnect, in-place restart, and CEF restart. A new-attempt cold start (~25 s boot + warm) will likely SPLIT the broadcast; the runbook must say so, and ADR 0004/C14 budgets are to be re-checked against 15 s. One single-shot RTMPS restart failed ("Socket I/O timed out"), so output reconnect must retry (C04/C09). | F-002, F-004 |
| 2026-10-15 | **Owner ruling: final heartbeat carries the exit reason.** Every exit path sends one best-effort `state: stopped` beat with `exitReason` (6-value enum) + redacted `exitDetail` (≤200), because Machine logs vanish with the Machine. Logging otherwise stays plain redacted stderr, with no crate and no log shipping (ADR 0007). Briefs: C06, C07, C12, R2002. | owner |
| 2026-10-15 | **YouTube "Poor" root cause = digital-silence AAC** (audio A/B, one session, garden, both direct: real audio Excellent 10 min; silent Poor at 1:50 and never recovered). Filler, CF relay, drift and transport all refuted earlier. C09: pass real audio through (AAC-LC 128k 48 kHz stereo); never digital zeros; noise filler is provisional, with a YouTube test owed. | F-004 |

## False premises found

| Premise | Reality | Found |
|---|---|---|
| CF SRT playback is single-reader; a second reader kicks the first (spec §3.1–3.2; C14 case 3 `reader-kicked`; `inputKickAlert`) | No kick from the same or a different IP (F-002). A closed playback is still possible (CF maintenance), so the reconnect path and case 3 stay; the alert's wording and the runbook must not claim "a second reader" as the cause | C00 F-002 |
| The C00/C12 Dockerfile deps are enough to link gstcefsrc | Also needs `libcups2t64`, `libpango-1.0-0`, `libcairo2` (C12's image) | C00 F-001 |
| `cefsrc ! video/x-raw,framerate=30/1` sets the rate | Caps don't negotiate; use cefsrc's `max-video-framerate` (C10/C11) | C00 F-001 |
| CEF alpha is straight | Premultiplied: C11 must unpremultiply (or composite as premultiplied) | C00 F-001 |
| Readiness via js-signals | `listen-for-js-signals=false` + page `console.info("seazn:ready")` read from the `cefconsole` category. The js-signal route hangs `start()` and `set_state(NULL)` when the page never signals (C11, R2005's page) | C00 F-001 |
| YouTube health reflects our send rate | Static, filler-padded content went Poor ~1 min in at 2.5/4/6 Mbps with a steady send; 0 %-filler snow stayed Excellent (confounded by keyframe density). Slate must carry real picture (C09); decided with real footage | C00 F-004 |
| `R2-compositor.md` in `seazn.club` describes the R2 to build | It describes B3, which the bench showed fails (156 dup / 84 gap) and which was struck in Sept. B2 replaced it, and ADR 0001 now supersedes B2. | 2026-10-14 |
| The capture app's `deliveredLagS` gives the camera delay | It is relative lag growth on the HLS manifest, not absolute delay. | 2026-10-14 |
| `composed` is unreachable because the organiser UI only posts `passthrough` | The API accepts `mode: "composed"`, and `runner-fly.ts` then throws because `RELAY_IMAGE` is unset. This is a live hazard (spec §8 item 1). | 2026-10-14 |

| An existing per-fixture `overlay_delay_ms` can be overridden | The column is on `fixture_stream_sessions` with default 0, and no code path or API ever writes it. Every session reads 0. | 2026-10-14 |
| The hazard throw is harmless because nothing gets provisioned | The throw happens after the session row, the CF input and provisioning already exist. R2001 must refuse in `admit()` before any write. | 2026-10-14 |
| R1 has a "degraded" heartbeat state to reuse | There is no degraded concept. Staleness is one `STALE_HEARTBEAT_SECONDS = 90` check in `expiry.ts`. | 2026-10-14 |
| A domain unit test can print its count with `eprintln!` (C03 `unlisted_pairs_hold_their_phase`) | The crate root's `#![forbid(clippy::print_stderr)]` (C01) covers inline `#[cfg(test)]` modules too: clippy `--all-targets -D warnings` refuses it (run in C02). A sweep that must print goes in `crates/domain/tests/` (C02's `foundations.rs`, C06's `properties.rs`); an inline one carries its count in the assertion. | C02, 2026-10-15 |
| Acked RTMP bytes show the output is alive (C04: "5 s with no acked bytes" is a stall; C09: "the first acked bytes → `OutputConnected{gen}`", acked bytes feed `bitrate_kbps` / `egress_bytes`) | F-005: CF Input B sent no RTMP acknowledgements; rtmp2sink `out-bytes-acked` stayed 0 in its 290 s pushes while `out-bytes-total` grew. On the live sink the ack stall would fire on every healthy push and `OutputConnected` would never arrive. C04 and C09 need a signal the live sink actually produces (for example `out-bytes-total` progress), proven on a real Input B, not on a `file://` target. | C02, 2026-10-15 |
| One output recovery fits the 15 s broadcast split (F-004 addendum), however many attempts it takes (C04 retry loop, C14 budgets) | It fits ONE attempt. Stall detected at 5 s (`output_ack_stall_ms`), back-off 0.5 s, a connect that times out at 5 s (`output_connect_timeout_ms`; F-004 rerun 1 saw exactly that, "Socket I/O timed out"), back-off 1 s: attempt 2 starts at 11.5 s with 3.5 s left. C02 pins only the single-attempt path (5 + 2 + 5 = 12 s). C04/C14 must budget the second attempt explicitly, or shorten a timeout. | C02, 2026-10-15 |
| The C03 brief's inline tests can collect phases in a `HashSet` (`table_has_no_duplicate_keys`, `every_phase_is_reachable_from_waiting`) | `purity.rs` tokenises every file under `domain/src`, inline `#[cfg(test)]` modules included, and refuses the bare name `HashSet` (probe: one `use std::collections::HashSet;` in `programme.rs`'s test module turned `domain_source_has_no_clock_env_fs_net_thread_or_process` red). C03 compares the rows pairwise and collects phases in a `Vec`; any later domain task's inline test uses `BTreeSet` or a `Vec`. | C03, 2026-10-15 |
| "1 s of continuous frames" is the time since the run's first frame (C03 brief Step 3: `Continuous` when `elapsed(now, run_since) >= resume_after_ms`) | That counts silence as frames: a camera that returns for 0.6 s and stops again reads Continuous 1 s after its return (the newest frame is only 0.4 s old), so Reconnecting goes live on a stale frame and freezes 0.1 s later. C03 measures the run as the span from its first frame to its latest, which needs frames that actually cover 1 s (tests `a_short_return_then_silence_is_not_a_second_of_frames`, `a_tick_one_second_after_the_first_frame_is_not_a_second_of_frames`; the brief's form survives the brief's own 13 tests). C06's P2 oracle (`now - run_start >= resume_after_ms`) still holds, being weaker, and C06's mutant 3 (`resume_after_ms` → `frozen_after_ms` in `feed`) still applies. | C03, 2026-10-15 |
| Audio follows the frame choice: "live audio, or silence generated on the programme clock" (spec §3.3) | F-004: digital-silence AAC is what makes YouTube report Poor. `FrameChoice` picks the video layer only and carries no audio decision; the audio track runs on in every programme phase with no PTS gap, real input audio or low-level noise (C09's Audio section). Nothing downstream (C06, C08, C09) may map `Slate`/`Repeat` to silence. | C03, 2026-10-15 |

## Session handoffs

- 2026-10-14 (session "r2"): brainstormed and approved the design, wrote the spec and ADRs 0001–0006.
  Next step is owner review of the spec, then the writing-plans step.
