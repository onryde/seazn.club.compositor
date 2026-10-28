# 0005. 1080p30 by default (amended 2026-10-27), 720p30 by the session's flag

- **Status:** accepted (owner, 2026-10-14); the default amended to 1080p30 (owner, 2026-10-27, F-033)
- **Originally:** "720p30 by default, 1080p30 behind a flag"

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

## Amendment 2026-10-27 (owner ruling on F-033)

- The programme output is **1080p30 by default**: `seazn.club` serves `output` = 1080p when
  `RELAY_OUTPUT_PROFILE` is unset or blank (seazn.club PR #1052). 720p30 stays the same session
  flag. The compositor is unchanged: it renders whatever the bootstrap's `output` block says.
- The encoder is x264 `medium` (`gst_programme.rs`, PR #103) and the production guest is
  performance-6x, 6 CPUs and 12288 MB (`RUNNER_DEFAULT_GUEST`, PR #1052).
- The size rule is p95 Machine CPU at most **75 %** of cores (`cost_cpu_p95_max_share`, was 50 %).
- The "Why" above is superseded: F-033 measured 1080p `medium` on 6 CPUs at p95 2.85 to 3.68 cores
  on the worst-case cost legs (peak 4.26), within 75 %, and a 3 h soak (run 40585028513) passed
  15 of 15 checks.
- The C15 soak runs at 1080p: `harness/soak/soak.json`'s `soak_output`, which `driver.sh` serves
  as the session (the 1080p soak above ran from a spike branch). The chaos legs stay 720p (the
  PR #52 review's m11).
