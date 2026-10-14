# 0001. Rust + GStreamer, two pipelines, CEF rendering the overlay page

- **Status:** accepted, owner, 2026-10-14
- **Supersedes:** the Sept 2026 B2 ruling in
  `seazn.club/docs/superpowers/specs/2026-09-05-stream-overlay-prompts/` (ffmpeg x11grab, headed
  Chromium, `#ff00ff` colorkey, TS supervisor on Node 26), and the B3 design still described in
  that folder's `R2-compositor.md`.

## Context

Three designs were on the table:

- the repo's Sept B2 design
- the owner's production spec (the PDF, 2026-10-03, §4 amended 2026-10-05), with Options A, B and C
- the owner's POC: Rust, a single pipeline, native cairo templates

The PDF names "Node + ffmpeg + Chromium" as a rejected alternative.

## Decision

Adopt PDF Option A, with graphics rendered by `cefsrc` from the existing `/overlay/fixtures/[id]` page:

- **Two pipelines.** The input pipeline is restartable. The programme pipeline runs on its own clock
  and is never rebuilt.
- **One HTML ticker for OBS and the cloud.** The same page serves both.
- **No native template renderer.**

## Consequences

- A theme ships once, and OBS and cloud stay pixel-identical.
- Costs about one extra core and 300–600 MB per match for the browser. Score timing is wall-clock
  (±2 frames).
- The POC's `cairooverlay`, JSON templates and scene engine are dropped. Moments and slots move into
  the page in R4.
- Feasibility of `cefsrc` on Fly is gated by spike T00.1.
