# 0005. 720p30 by default, 1080p30 behind a flag

- **Status:** accepted (owner, 2026-10-14)

## Decision

- The programme output is 720p30 by default. 1080p30 is a config flag carried in the session
  bootstrap `output` block.
- CEF always renders the page at 1920×1080, and the frame is scaled down for 720p, so the page
  contract never changes.
- The soak runs at 720p.
- 1080p CPU on perf-4x is measured once in spike T00.5, to give a cost number.

## Why

At 720p a perf-4x Machine has headroom: one media core, one browser core, and spare capacity. That
headroom keeps the encoder-overrun degrade ladder a rare event rather than the normal path. 1080p
would use about 3 of the 4 cores at steady state.
