# C05b: Domain Graphics hang detection

**Repo:** `seazn.club.compositor`. **Depends on:** C05 (merged). Run it **before** C06 routes
`CefAlive`, because C06's routing calls `Graphics::on_alive`.

**Authority:**
- The owner, 2026-10-18, in chat, relayed by the coordinator: "2 Ok". That approved hang detection
  as designed in `docs/_INDEX.md` Owed 18, which is now closed.
- **Hide at once.** The design screen the owner approved read, verbatim: "After 10 s of silence (5
  missed pulses), the compositor hides the overlay (so no stale score) and restarts it." This is
  an owner-approved design (2026-10-18). Reading "hides" as "at once, on every hang" is the
  coordinator's reading of it (PR #21 review, I1).

Where this brief and the Owed 18 row disagree, this brief wins. Raise any other gap before
building.

## Context

- **A hung page is invisible today.** Once the overlay page is Ready, nothing detects a JS hang.
  - C11 reports a bus error, a renderer exit, a CEF child gone and RSS over the cap. A
    `while(true){}` leaves the renderer alive.
  - Frame silence is not a signal either: cefsrc emits a buffer only when the page paints (F-001: a
    static page gave 4 in 40 s).
  - So a page that hangs after Ready keeps its last frame on air, possibly a stale score, until the
    2 h recycle (false-premise row "A CEF hang restarts the overlay").
- **The page beats.** R2005 (amendment 2026-10-18) makes the page log `console.info("seazn:alive")`
  every `cef_alive_every_ms` (2 000) on F-001's console channel, with `host=compositor` only and
  after ready. It never uses `gstSendMsg`. The harness page beats too (C11 amendment). C11 relays
  each line as `Event::CefAlive{gen}`, and C06 routes it to `Graphics::on_alive`.
- **This task is the domain half.**
  - Record the last beat while Ready. The ready signal counts as the first beat.
  - A Ready page silent for `cef_hang_after_ms` (10 000: five beats, equal to the page's
    `SCORE_STALE_AFTER_MS`) has hung. That is a counted failure through the existing `fail`, **and
    the overlay is hidden at once**, below the count as well as at it. A hung page's last frame may
    be a stale score, so it does not stay on air through the back-off and reload.
  - Below the count, the hang backs off like a crash and then restarts. At the count it enters
    Failed, as before.
  - No new action is needed. The hang tick returns ~~`Hide`~~ `Hide { reason: Hang, gen }`
    (amended 2026-10-18, PR #22 review I2: every hide names its reason and generation; see the
    amendment below).
  - C08 maps `HideOverlay` to `programme.hide_overlay()` plus `graphics.stop()` (Owed 17). C11
    writes the slot only from a generation that has signalled its own ready.
  - So the hide lifts by itself when the next generation is Ready. No `show_overlay` exists, and
    none is needed.
- ~~**A crash is unchanged.** Below the count, a crash still holds the last frame through the
  back-off. Only a hang hides at once, because only a hang can leave a frozen, stale picture behind.~~
  ~~**A crash's hold is bounded** (amended 2026-10-18; coordinator decision, PR #22 review G1). Below
  the count a crash still holds the last frame through the back-off and reload, but at most
  `cef_hang_after_ms` after the crash; then `Hide { reason: Crash, gen }` until the next generation
  is Ready.~~ **One bound on a frame's age** (amended again 2026-10-18; coordinator decision, the
  review's second round): the frame on air stays on air at most `cef_hang_after_ms` after the last
  ready or beat of the page that drew it, whether that page hung, crashed or was recycled, then
  `Hide { reason: Hang | Crash | Recycle, gen }` until the next generation is Ready. Only a hang
  hides at once, because only a hang leaves the frozen page itself on air; a crashed or recycled
  page's frame goes just as stale once its reloads fail to come ready (the review's probe held a
  crash's about 23 s, and a recycle's about 33 s).
- **Why 10 s and not shorter.** CEF children run at `nice 10`, so beats slip when the Machine is
  short of CPU. A shorter threshold would turn the ladder's CPU starvation into restarts, and their
  load spikes would feed the ladder. Ten seconds equals the page's own stale threshold.

## Step 0: the hide question (answered)

Owed 18 left one sub-question open: should a hang also hide the overlay at once? **Answered: yes**
(see "Authority" above). The trade the owner accepted:
- about 4 s with no bug on air (the 1 s back-off plus the ~2.8 s load, F-001)
- in exchange for never showing a frozen, possibly stale frame through that time

Build the hide. Paste this answer into the commit body.

## Files

- Modify: `crates/domain/src/graphics.rs`, and `crates/domain/src/config.rs` (two fields; C02
  amendment 2026-10-18)
- Modify: `crates/domain/tests/graphics.rs` (the pair matrix and the generated-schedule sweep)

## Interfaces

```rust
// config.rs (C02 amendment 2026-10-18)
pub cef_alive_every_ms: Ms,   // 2_000; R2005's beat; the domain does not read it
pub cef_hang_after_ms: Ms,    // 10_000; a Ready page silent this long is hung

// graphics.rs
pub enum GraphicsState {
    Starting { since: Ms },
    /// `since`: the ready signal (the recycle clock). `last_alive`: the last beat, the ready included.
    Ready { since: Ms, last_alive: Ms },
    Backoff { attempt: u32, until: Ms },
    Failed { retry_at: Ms },
}
impl Graphics {
    /// The page of generation `gen` beat. It counts only while that page is Ready.
    pub fn on_alive(&mut self, now: Ms, gen: u64);
}
```

- `Ready` gains `last_alive`, so the hang deadline lives in the state where it can be inspected,
  not in a private field beside it.
- C06's matches use `Ready { .. }`, so they are unaffected.
- `on_ready` sets `last_alive = now`.

## Rules (the tests' oracle; every number comes from `Thresholds::default()`)

| From | Event | Result |
|---|---|---|
| Ready (gen g) | `on_alive(now, g)` | `last_alive = ~~now~~ max(now, last_alive)` (amended 2026-10-18, review M3), `since` unchanged, no action |
| Ready | `on_alive(_, other gen)` | nothing |
| Starting / Backoff / Failed | `on_alive(_, _)` | nothing. The page beats only after ready, and a beat never makes a page Ready |
| Ready | tick at `now ≥ last_alive + cef_hang_after_ms`, below the count | `fail(now)` → Backoff `attempt n−1`, and **returns `Some(Hide)`** (amended 2026-10-18: `Hide { reason: Hang, gen: g }`) |
| Ready | the same tick, at the count | `fail(now)` → Failed + `Some(Hide)`, as in C05 (amended 2026-10-18: still `reason: Hang`; the reason names the cause, not the state left) |
| Ready | tick at `now = last_alive + cef_hang_after_ms − 1` | nothing |
| Ready | tick with **both** the hang and the recycle due | **the hang wins**: a counted failure, plus `Hide`. A hang near the 2 h mark is never laundered as a planned restart. Otherwise 3 hangs at recycles would never reach Failed |
| Ready | tick with only the recycle due | the planned restart, as in C05: no failure and no hide |
| Starting / Ready | crash below the count | as in C05: Backoff, **no** `Hide`, and the frame is held (amended 2026-10-18: for at most `cef_hang_after_ms`; see the amendment below) |

**The invariant, replacing "hidden iff Failed".** The overlay is hidden from a `Hide` until the next
generation's Ready. A `Hide` comes from Failed, or from a hang.
- So a hang's hide covers the Backoff and the Starting of the reload, and lifts at that
  generation's `on_ready`.
- A Failed hide lifts at the Ready of the generation after `retry_at`.
- The sweep tracks hidden as follows:
  - `Hide` sets it.
  - The `on_ready` of a generation newer than the one hidden clears it.
  - Nothing else touches it.

## Steps

- [ ] **Step 0:** paste the answer above into the commit body.
- [ ] **Step 1: config.** Add the two fields, with docs citing Owed 18 and F-001.
  - New inline test `the_hang_threshold_is_five_beats`: it asserts
    `cef_hang_after_ms == 5 * cef_alive_every_ms`, and that `cef_hang_after_ms` is below
    `cef_recycle_every_ms`.
  - Run it red (the fields are missing), then green.
- [ ] **Step 2: tests first** (inline in `graphics.rs`). Each must fail for the right reason before
  any body is written. Every Ready below sits at `r = cef_load_timeout_ms / 2`, never at 0, so a
  `last_alive` left at 0 is visible.
  - `a_beating_page_never_hangs`: Ready, then a beat every `cef_alive_every_ms` for 3× the recycle.
    No `fail` happens, and the only actions are the planned recycles. No `Hide` appears.
  - `a_silent_ready_page_hangs_and_hides_at_exactly_the_threshold`:
    - At `r + cef_hang_after_ms − 1`, a tick does nothing.
    - At `r + cef_hang_after_ms`, a tick returns `Some(Hide)` and enters Backoff `attempt 0`, with
      the generation unchanged.
    - The next tick at `+ cef_backoff_base_ms` returns `Some(Restart)`. One ms before that, it
      returns nothing.
  - `the_last_beat_moves_the_deadline`: beats at `r + 2 000` and `r + 4 000`, then silence. The hang
    fires at `r + 4 000 + cef_hang_after_ms`, and not before.
  - `three_hangs_in_the_window_fail_and_hide`: hang, back off, restart, ready, hang, and so on.
    **Every** hang returns `Some(Hide)`. The `cef_fail_count`-th enters Failed.
  - `a_hang_and_a_crash_share_the_window`: two crashes then a hang, and a hang then two crashes,
    reach Failed at the count. The crashes below the count return `None`, and the hang returns
    `Some(Hide)`.
  - `the_hang_wins_over_a_due_recycle`: the last beat sits at
    `since + cef_recycle_every_ms − cef_hang_after_ms`, so the hang and the recycle fall due on the
    same tick. That tick records a failure (counted) and returns `Some(Hide)`, not `Restart`.
  - `beats_from_other_generations_and_states_change_nothing`: `on_alive` with `gen ± 1` in Ready,
    and with any gen in Starting, Backoff and Failed. The state, the generation and the window are
    equal before and after. Count the cases, and fail on 0.
- [ ] **Step 3: implement.** Write `on_alive`, `on_ready`'s `last_alive`, and the Ready arm in
  `on_tick`. The hang is checked before the recycle, and it returns `Some(Hide)` whether `fail`
  answered `None` (Backoff) or `Some(Hide)` (Failed). Update the module docs:
  - The state diagram gains `Ready --silent cef_hang_after_ms: failure + Hide--> Backoff | Failed`.
  - The Failure bullet names the hang.
  - The Hide bullet states the new invariant.
- [ ] **Step 4: the integration sweeps** (`tests/graphics.rs`).
  - **The pair matrix.**
    - `GEvent` gains `Alive`, and `G_EVENTS` grows to 4.
    - `due()` for Ready is `min(since + recycle, last_alive + hang)`.
    - `brief()` gains the rows above: a Ready tick with the hang due is `after_failure(n, at, t)`,
      with its action forced to `Some(Hide)`.
    - `is_failure` gains `(Ready, Tick)` when the hang is due.
    - The setups gain Ready with the hang due, at both 0 and `count − 1` prior failures, so a hang
      lands both below the count and at it.
    - Count the hides that come from a hang, print the count, and fail on 0.
  - **The generated-schedule sweep.**
    - The generator emits beats while Ready, at `cef_alive_every_ms` plus jitter, and in some cases
      stops beating.
    - The invariant above replaces "hidden iff Failed". Every restart is still a new generation.
    - Add these counters, and print them all:

      | Counter | Counts | Fails on |
      |---|---|---|
      | `hangs` | hangs detected | 0 |
      | `hang_hides_lifted_by_ready` | hang hides lifted by the next generation's Ready | 0 |
      | `beaten_ready_ticks` | Ready ticks that had a beat within the threshold | 0 |
      | `hang_at_recycle` | ticks where the hang and the recycle were due together | see below |

    - If the generator cannot reach `hang_at_recycle` in 120 cases, add a directed case. Never
      accept 0 silently.
- [ ] **Step 5: run.**
  - `cargo test --workspace`: paste the counts. Total passed, 0 ignored, and all 5 layering guards.
  - `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and
    `scripts/check-portable.sh`.
- [ ] **Step 6: mutate each guard once.** Use a python in-place replace, plus a sha check to
  restore. Never `git checkout`.
  1. Delete the Ready hang arm. Expected red:
     `a_silent_ready_page_hangs_and_hides_at_exactly_the_threshold`, and the matrix.
  2. Change `>=` to `>` in the hang compare. Expected red: the exact-threshold test.
  3. Check the recycle before the hang. Expected red: `the_hang_wins_over_a_due_recycle`.
  4. Make `on_alive` ignore the generation. Expected red:
     `beats_from_other_generations_and_states_change_nothing`.
  5. Make `on_alive` also act in Starting, turning it Ready. Expected red: the same test.
  6. Make `on_ready` leave `last_alive` at 0. Expected red: the exact-threshold test, because the
     hang fires at once. That needs `r > 0`, which Step 2 requires.
  7. Make the hang arm return `fail`'s answer, which is `None` below the count. Expected red: the
     exact-threshold test and `three_hangs_in_the_window_fail_and_hide`.

  Record all seven in the commit body.
- [ ] **Step 7: commit.** `feat(domain): graphics hang detection from the page's alive beat (C05b)`

## Acceptance

- Step 0's answer is in the commit body.
- 8 new inline tests pass. The two sweeps report non-zero `hangs` and `hang_hides_lifted_by_ready`.
- 7 mutants went red, and the tree is clean after each restore.
- `cargo test --workspace` is green with 0 ignored. Clippy, fmt and check-portable are clean.
- `docs/_INDEX.md` gets a session-handoff line, and Owed 18's build is marked done.

## Amendment 2026-10-18 (PR #22 review; coordinator decisions, relayed)

The review returned NEEDS FIXES. The coordinator adopted I2, ruled on G1, and routed I1, G2 and G3
to the briefs named below. Built in PR #22 on top of the C05b commits.

**1. Every hide names its reason and generation (I2).** `GraphicsAction::Hide` is now
`Hide { reason: HideReason, gen }`, and `HideReason` is `Hang | Failed | Crash` (round 2 adds
`Recycle`, and changes what `Crash` and `gen` mean below; see its section):
- `Hang` is every hang, the count-th included. The reason names what took the frame off air, not
  the state the machine is left in.
- `Failed` is a crash or a load timeout that enters Failed.
- `Crash` is a crash's hold running out (point 2).
- `gen` is the generation whose failure hid the overlay. For `Crash`, that is the crashed generation,
  not the reload that is loading.

C06 emits `HideOverlay { reason, gen }`, and the driver logs the one line
`overlay hidden reason=<hang|crash|failed> gen=<g>` (C12). C14 case 11, C15 (b) and the C17 runbook
read that line, not the three-line chain. Tests pin the reason and the generation on every hide
path, and the reason-swap mutants went red.

**2. A crash holds the last frame at most `cef_hang_after_ms` (G1).** This is a coordinator decision
derived from the owner-approved 10 s rule (`docs/_INDEX.md`). **Superseded by round 2** (below): the
bound now runs from the crashed page's last beat, not the crash, it covers a recycle too, and the
private `OnAir` is gone.
- After a crash below the count, the first tick at or after the crash plus `cef_hang_after_ms` returns
  `Hide { reason: Crash, gen }` if no newer generation is Ready by then. The hide holds until the next
  generation is Ready.
- **A reload that is Ready inside the window** means no hide, then or later.
- **The clock.** A second crash keeps the first crash's clock and generation.
- **What can be held.** A crash holds only a frame that is on air:
  - none before the first Ready
  - none from any hide until the next Ready
  - a recycle's or a load timeout's last frame does stay on air, so a crash of that reload holds it
- **Tick order.** The hold's tick is checked first, so the frame comes off air on the tick its hold
  ends. A restart or load timeout due on the same tick follows on the next tick.
- **What follows a Crash hide.** It leaves the reload loading, so the next action is a Restart, or
  that reload's `Failed` hide. That is the one pair of hides with neither a restart nor a Ready
  between them.
- `Graphics` tracks this with a private `OnAir` (`Nothing | Frame | Held { since, gen }`). Its public
  API is unchanged except for the hide.

**3. `on_alive` keeps the later beat (M3).** `last_alive = max(now, last_alive)`, so an
earlier-stamped beat never pulls the deadline in. It is unreachable today: C08 stamps on receipt from
a monotonic clock.

**4. Sweep checks that could not fail (M1)** are replaced by air probes on a clone after every
step, which can fail:
- **Held:** the hold's tick hides as `Crash`.
- **Frame:** a crash then holds and hides.
- **Nothing:** a crash hides nothing.

The matrix gains frame-on-air and held setups.

**Routed to other briefs** (each amended 2026-10-18):
- **C11 (I1):** `stop()` takes the `restart()` reaper path. It never blocks, and it closes the
  stopped generation's slot gate at once; a reported crash closes that generation's gate too.
- **C08:** `stop()` gets semantics. A `Crash` hide maps to `hide_overlay()` alone, so the reload
  survives; `Hang` and `Failed` map to `hide_overlay()` and `stop()`.
- **C06 (G2):** the final hold runs the graphics tick and routes `CefCrashed`. It maps `Hide` and
  drops `Restart`.
- **C12, C14, C15 and C17 (I2):** the single reason line.
- **R2005 (G3):** E9 checks the beat across live → reconnecting → final.

## Amendment 2026-10-18, round 2: one bound on a frame's age (coordinator decision, relayed)

The coordinator confirmed round 1's derived points (a `Crash` hide maps to `hide_overlay()` alone;
C11 closes a generation's slot at its reported crash). It ruled on the open one, G1's ~33 s hold
after a recycle or a load timeout whose reloads never come ready: generalise the rule rather than
put it to the owner, since it is the principle the owner approved at 10 s. This supersedes round 1's
point 2.

**The invariant.** The frame on air may stay on air at most `cef_hang_after_ms` after the last proof
of life (its ready, or a beat) of the page that produced it. The first tick at or after that instant
hides it.
- **One check, first in `on_tick`.** It reads `on_air()`: the Ready page's own frame, whose proof is
  its `last_alive`, or a held frame. A hang is that check firing on the Ready page's own frame, so the
  separate hang arm is gone; the hang is still a counted failure, and still wins over a recycle due on
  the same tick. A restart or a load timeout due on the bound's tick follows on the next one.
- **What holds a frame.** A crash of a Ready page below the count, or a recycle, ends its life and
  records `Held { gen, proof: last_alive, ended }`. Nothing else does: a page that never got Ready
  showed no frame of its own, so its crash or load timeout leaves a held frame's page, bound and cause
  as they were. A Ready replaces the frame, and a hide clears it.
- **The clock moved.** Round 1 timed a crash's hold from the crash. It now runs from the crashed
  page's last beat, so the frame comes off up to one beat (2 s) sooner: the invariant read literally.
- **A crash reported past the bound** but before its tick is one failure, and hides as `Crash`, not as
  a hang too.

**The reasons: `Hang | Crash | Recycle | Failed`, the minimal set.** A bound hide names the path that
started it.
- `Recycle` is the one new reason. A recycle is planned and records no failure, so `Crash` would send
  an operator after a crash that never happened, and `Hang` would count a failure.
- No `LoadTimeout`. A load timeout never starts a hold: a page that timed out never signalled ready,
  so it showed no frame of its own (C11 writes the slot only after a page's own ready). Its reloads'
  load timeouts neither end nor move the bound of the frame on air.
- No `Stale`. One reason for every bound would throw away the cause I2 asked for.
- `gen` names the page whose frame came off (`Hang`, `Crash`, `Recycle`), or the generation whose
  failure entered Failed (`Failed`). A recycle whose reload crashes still hides as `Recycle, gen 1`.
- A `Recycle` hide leaves the reload loading, like a `Crash` hide, so C08 maps both to
  `hide_overlay()` alone. The only pairs of hides with neither a restart nor a Ready between are
  (`Crash` or `Recycle`, then that reload's `Failed`).

**G1's cases, under the defaults** (bound 10 s, load timeout 10 s, beat 2 s):
- a recycle whose reloads never come ready: off at its last beat + 10 s, so at most 10 s after the
  recycle (it was about 33 s)
- a crash whose reloads never come ready: off at its last beat + 10 s (it was the crash + 10 s)
- reloads that time out inside the bound (a load timeout under 10 s): no change to the bound

**Tests.**
- Inline:
  - the recycle-never-ready and the reloads-time-out-inside-the-bound cases hide at exactly the
    bound and stay shown at −1, for both paths
  - a reload Ready at its restart, at −1, or on the bound's own instant before the tick keeps the
    frame shown
  - the bound reads `cef_hang_after_ms` from the proof, under thresholds where the crash, the restart
    and the load timeout all differ
  - a crash reported past the bound, and the bound winning a tick a restart is also due on
- The matrix gains recycled-frame setups (hides by reason: Hang 4, Failed 5, Crash 2, Recycle 2).
- The sweep runs under the defaults and under a 3 s load timeout, so reloads time out beside a held
  frame. From the machine's outputs alone (a page's frames reach air at its Ready and leave at a
  `Hide` or the next Ready), it asserts after every tick that no frame is on air
  `cef_hang_after_ms` after its page's proof. It prints the count and fails on 0.
- The grid test covers both paths at every phase of both modes, and pins its case count.
- 54 of 54 mutants red, none equivalent: 10 on the reason and generation each hide names, 16 on the bound (its comparison, its field, its anchor, its precedence over a restart, a load timeout and a recycle), 7 on how a life ends, 7 on the beat (C05b's, re-pinned), and 14 anti-vacuity mutants on the harness. With the matrix assert and the air probes switched off, the sweep's bound invariant alone still goes red on a held frame that never hides and on a bound 1 ms late.

## Amendment 2026-10-20 (C11b: CEF's health is the process's, not this machine's)

C11b (PR #34) adds no state or action here. A dead zygote, the GPU-process deaths and leaked teardowns are facts
about the process's one CEF, so the supervisor reads them (`Event::CefHealth`) and restarts the process in place;
this machine keeps the page's life (the beat, the crash count, the bound). One consequence: a renderers' zygote's
death no longer walks graphics to `Failed` by its storm of `CefCrashed` (Owed 56's road), because the verdict reaches
the domain within a child poll (1 s) and a tick, and the process exits first.

## Amendment 2026-10-25 (Owed 99: a blank page, and the held frame past a ready)

Owner, 2026-10-25, in chat, relayed by the coordinator: "Apply rec" on Owed 99 (option B, a
blank-overlay watchdog). The rules below are implementer decisions under that dispatch
(`docs/_INDEX.md`, decision row "Owed 99 built").

- **A blank page.** A Ready page that has shown no frame of its own `cef_hang_after_ms` after its
  ready is blank: `on_tick` returns `GraphicsAction::Blank { gen }`, once a page. Its first frame is
  the event `on_painted(gen)` (the supervisor's `Event::CefPainted`), read only for the current
  generation while Ready. Frame silence is no rule: cefsrc paints only on change (F-001), so a
  static page is silent and healthy. The bound is this brief's one bound, from the Ready instant.
- **Order.** The hang (no beat) is checked before the blank, so a page that neither beats nor paints
  is a hang, counted once.
- **The reload.** `on_blank_reload(now)` ends the blank page's life as a reload (no failure) and
  builds the next generation; the supervisor decides whether to call it (C11b's amendment of this
  date).
- **The held frame (Owed 99's shape (b)).** A held frame from the page before is read while the new
  page is Ready and has shown no frame, so it keeps its own bound past that ready; the new page's
  first frame clears it. A page that showed no frame holds none of its own at its end.

## Amendment 2026-10-25, PR #92 fix round 1 (m-4, m-5: the order, said precisely)

The PR #92 review of `bdc4588` (Needs fixes: tests and docs only, the code approved), adopted
by the coordinator; the coordinator's fix-round dispatch, implementer decisions under it
(`docs/_INDEX.md`, decision row "PR #92 fix round 1").

- **Order** (amends the bullet of Owed 99's amendment above). The hang rule runs first, so a page
  silent since its ready, with no frame, is a hang, counted once. A page that beat after its ready
  and then went silent with no frame is blank at its ready's bound, which comes before its own hang
  bound (its last beat's), and is counted once too: its reload ends it. Test:
  `a_page_that_beat_then_went_silent_unpainted_is_blank_once_and_never_a_hang`
  (`crates/domain/tests/cef_health.rs`, both thresholds).
- **A blank and a recycle never share a tick** (`cef_hang_after_ms < cef_recycle_every_ms`, the
  config's own test), so their order after the bound is not load-bearing; `on_tick`'s comment says
  so and no longer claims an order it cannot reach.

## Do not touch

- The ladder, the links, the programme and the supervisor (C06 routes the event).
- Every adapter (C11 relays the event).
- Note that `crates/` and `docker/` must not contain the platform name: check-portable greps for it.

## Output cap

Under 15 lines.
