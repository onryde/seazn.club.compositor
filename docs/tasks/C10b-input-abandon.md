# C10b: the domain abandons a generation it gave up (stub)

**Status:** stub, written in PR #30 round 2 (2026-10-19) from `docs/_INDEX.md` Owed 50. The
coordinator owns it: dispatch, scope and sequencing are theirs. Expand it into a full brief before
dispatch.

**Repo:** `seazn.club.compositor`. **Depends on:** C10 (PR #30). **Sequenced before** C14's flap
cases. It is its own task because C11's do-not-touch list includes the input adapters and C12's
includes the domain and app crates.

## Why

The domain gives an input generation up by its stall rule (`InputLink::check_stall`) or its connect
timeout (the Connecting arm of `InputLink::on_tick`), and today tells the adapter nothing until the
next `Effect::RebuildInput`, a back-off later. C10 (G1) closes the gap with a second copy of the
rules inside the adapter (`gst_input.rs`: `SampleClock`, `ruled`, `DOMAIN_LAG_ALLOWANCE_MS` = 250),
which leaves three residual windows (Owed 50):

1. A loop that lags by more than 250 ms lets a given-up generation's frames back on air for the
   back-off.
2. The 250 ms early window: a first frame or a stall gap in [rule − 250 ms, rule) is given up by the
   adapter where the domain alone would keep it.
3. A `Thresholds` mismatch between `Driver::new(t)` and `GstInput::new(…, &t, …)` (Owed 51).

## What (to be confirmed in the full brief)

- **Domain:** `InputLink` reports its own give-up (stall and connect timeout), and `Supervisor`
  pushes a new `Effect::AbandonInput { gen }` on the Tick arm that decided it. Tests for both rules,
  the second call (a gen already abandoned), and an Eos/Error loss (which the adapter decided itself,
  so no abandon is owed).
- **App:** `InputSource::abandon(&mut self, gen: u64)` in `ports.rs`; one apply arm in `driver.rs`.
- **Testkit:** `FakeInput` logs `abandon`.
- **Adapters:** `GstInput::abandon(gen)` closes the gate for that generation's build (video and
  audio), about 10 lines. Then delete `SampleClock`, `ruled` and `DOMAIN_LAG_ALLOWANCE_MS` (keeping
  both would keep window 2), take `Thresholds` back out of `GstInput::new` if nothing else reads it,
  and re-point C10's G1 tests (`frames_after_the_stall_rule_never_reach_the_programme`,
  `a_late_connect_is_dropped_and_a_fresh_generation_connects`,
  `audio_stops_with_the_video_past_the_stall_rule`) at the port call.
- **Docs:** amend the C04, C06 and C08 briefs; close Owed 50 and 51.
- **Size:** about 100–150 lines across the four crates.

## Known residual even then

`InputFrame` carries no generation, so frames already queued when the domain abandons a
generation still extend the phase by at most `frozen_after_ms`.

## Output cap

Under 15 lines.
