# 0003. R2 scope = PDF phases 0–1 + R1 wiring; the rest is R4

- **Status:** accepted (owner, 2026-10-14)

## Decision

R2 delivers:

- the compositor image
- two-pipeline media core
- `cefsrc` on the existing overlay page
- bootstrap, heartbeat and lifecycle wired to R1, so that `mode: "composed"` works end to end
- the chaos harness

It opens with a 2–3 day spike gate (T00). The plan halts if `cefsrc` on Fly or CF SRT playback
fails.

Acceptance:

- chaos suite green on 3 consecutive runs
- 8 h soak passed
- one owner-watched real end-to-end on stg

R3 is already taken: the native capture apps in `seazn.club.capture`. The remaining PDF phases 2–4
are therefore **R4**:

- page moments and slots
- sponsor rules and proof-of-play
- console additions
- warm pool and standby
- the pilot

## Consequences

Phases 2–4 depend on contracts that R2 will shake out, so they wait. R2 has no warm pool, and
machine-loss recovery is about 60 s rather than the PDF's 30 s.
