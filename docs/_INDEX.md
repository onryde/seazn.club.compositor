# Relay compositor — programme index

The entry point for any session working on this repository. Read this file first, then
`docs/_RULES.md` (written alongside the R2 plan).

## Status

| Release | State | Docs |
|---|---|---|
| R1 control plane (`seazn.club`) | merged (#812, #902, #904) | `seazn.club/docs/superpowers/specs/2026-09-05-stream-overlay-prompts/_INDEX.md` |
| **R2 compositor** | design approved 2026-10-14. The spec is under review. The plan has not been written yet. | `specs/2026-10-14-r2-compositor-design.md` |
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

## False premises found

| Premise | Reality | Found |
|---|---|---|
| `R2-compositor.md` in `seazn.club` describes the R2 to build | It describes B3, which the bench showed fails (156 dup / 84 gap) and which was struck in Sept. B2 replaced it, and ADR 0001 now supersedes B2. | 2026-10-14 |
| The capture app's `deliveredLagS` gives the camera delay | It is relative lag growth on the HLS manifest, not absolute delay. | 2026-10-14 |
| `composed` is unreachable because the organiser UI only posts `passthrough` | The API accepts `mode: "composed"`, and `runner-fly.ts` then throws because `RELAY_IMAGE` is unset. This is a live hazard (spec §8 item 1). | 2026-10-14 |

## Session handoffs

- 2026-10-14 (session "r2"): brainstormed and approved the design, wrote the spec and ADRs 0001–0006.
  Next step is owner review of the spec, then the writing-plans step.
