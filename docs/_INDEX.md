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
| 2026-10-14 | Plan: exit 78 (refused) never spends web's one retry. `on-failure` restarts it in place twice (harmless, each refuses in about 1 s), but web marks the session `failed(refused)` instead of creating `r<n+1>`. This was a spec gap, found while writing W03. | — (plan, W03) |
| 2026-10-14 | Plan: chaos cases 8 (void) and 11 (clock skew) run against the harness test page and prove the CEF render path. The real page's rules are owned and tested by W05, and proven live in C17. | — (plan, C14/W05) |
| 2026-10-14 | Plan: composed `target` is CF Input B (the destination key never reaches the Machine); `fixture_stream_inputs` gains `role`; Input B records, and Input A doesn't (pending W04's recording-consumer re-pin). | — (plan, W04) |
| 2026-10-14 | Plan: the overlay URL carries `sid` beside `st`, because `verifyRelayToken` needs an expected sid. The relay read of a non-public fixture never shares a cache entry with the public read. | — (plan, W04/W05) |
| 2026-10-14 | Plan: a Running input that delivers no frame for `input_stall_after_ms` (5 s, after the 3 s slate) is rebuilt, because CF can hold a silent playback socket open. The domain owns the rule, and adapters have no watchdog. | — (plan, C04/C06) |
| 2026-10-14 | Plan: the soak runs on two Fly Machines (the real image on the production guest, plus a harness Machine), so the fakes don't steal the compositor's CPU. | — (plan, C15) |
| 2026-10-14 | Plan: an image push is not a release. `RELAY_IMAGE` changes only through `promote.yml`, behind a required-reviewer environment. | — (plan, C16) |

## False premises found

| Premise | Reality | Found |
|---|---|---|
| `R2-compositor.md` in `seazn.club` describes the R2 to build | It describes B3, which the bench showed fails (156 dup / 84 gap) and which was struck in Sept. B2 replaced it, and ADR 0001 now supersedes B2. | 2026-10-14 |
| The capture app's `deliveredLagS` gives the camera delay | It is relative lag growth on the HLS manifest, not absolute delay. | 2026-10-14 |
| `composed` is unreachable because the organiser UI only posts `passthrough` | The API accepts `mode: "composed"`, and `runner-fly.ts` then throws because `RELAY_IMAGE` is unset. This is a live hazard (spec §8 item 1). | 2026-10-14 |

| An existing per-fixture `overlay_delay_ms` can be overridden | The column is on `fixture_stream_sessions` with default 0, and no code path or API ever writes it. Every session reads 0. | 2026-10-14 |
| The hazard throw is harmless because nothing gets provisioned | The throw happens after the session row, the CF input and provisioning already exist. W01 must refuse in `admit()` before any write. | 2026-10-14 |
| R1 has a "degraded" heartbeat state to reuse | There is no degraded concept. Staleness is one `STALE_HEARTBEAT_SECONDS = 90` check in `expiry.ts`. | 2026-10-14 |

## Session handoffs

- 2026-10-14 (session "r2"): brainstormed and approved the design, wrote the spec and ADRs 0001–0006.
  Next step is owner review of the spec, then the writing-plans step.
