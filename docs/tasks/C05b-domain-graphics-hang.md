# C05b: Domain Graphics hang detection

**Repo:** `seazn.club.compositor`. **Depends on:** C05 (merged). Run it **before** C06 routes
`CefAlive`, because C06's routing calls `Graphics::on_alive`.

**Authority:** the owner, 2026-10-18, in chat, relayed by the coordinator: "2 Ok". That approves
hang detection as designed in `docs/_INDEX.md` Owed 18, which is now closed. The design is restated
below; where this brief and the Owed 18 row disagree, raise it before building.

## Context

- **A hung page is invisible today.** Once the overlay page is Ready, nothing detects a JS hang. C11
  reports a bus error, a renderer exit, a CEF child gone and RSS over the cap. A `while(true){}`
  leaves the renderer alive. Frame silence is not a signal either: cefsrc emits a buffer only when the
  page paints (F-001: a static page gave 4 in 40 s). So a page that hangs after Ready keeps its last
  frame on air, possibly a stale score, until the 2 h recycle (false-premise row "A CEF hang restarts
  the overlay").
- **The page beats.** R2005 (amendment 2026-10-18) makes the page, with `host=compositor` only and
  after ready, log `console.info("seazn:alive")` every `cef_alive_every_ms` (2 000) on F-001's
  console channel. It never uses `gstSendMsg`. The harness page beats too (C11 amendment). C11
  relays each line as `Event::CefAlive{gen}`, and C06 routes it to `Graphics::on_alive`.
- **This task is the domain half.**
  - Record the last beat while Ready. The ready signal counts as the first beat.
  - A Ready page silent for `cef_hang_after_ms` (10 000, five beats, equal to the page's
    `SCORE_STALE_AFTER_MS`) has hung. That is a counted failure through the existing `fail`.
  - So 3 hangs in 5 min → Failed + `Hide`. There is no new action, and "hidden iff Failed" still
    holds.
  - Before the count, a hang backs off like a crash. The adapter holds the last overlay frame
    meanwhile (C11).
- **Why 10 s and not shorter.** CEF children run at `nice 10`, so beats slip when the Machine is
  short of CPU. A shorter threshold would turn the ladder's CPU starvation into restarts, and their
  load spikes would feed the ladder. Ten seconds equals the page's own stale threshold.

## Step 0: one question before building (the owner's, not yours)

Owed 18 left one sub-question open: should a hang **also hide the overlay at once**? The trade is
about 4 s with no bug on air, against about 4 s more of a held, possibly stale frame while the page
reloads. "2 Ok" approved the design as written, and that design is the counted failure with no
immediate `Hide`.

**Build that** unless the controller relays a different ruling before you start. Ask the controller
in your first message, and never decide it yourself. If the answer is "hide at once":
- the Ready hang arm returns `Some(Hide)`, even below the count
- the "hidden iff Failed" invariant becomes "hidden iff Failed or hung-and-backing-off"
- this brief's tests change with it, so stop and get the amended brief

## Files

- Modify: `crates/domain/src/graphics.rs`, `crates/domain/src/config.rs` (two fields; C02
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

`Ready` gains `last_alive`, so the hang deadline lives in the state and can be inspected. It is not
a private field beside it. C06's matches use `Ready { .. }` and are unaffected. `on_ready` sets
`last_alive = now`.

## Rules (the tests' oracle; take every number from `Thresholds::default()`)

| From | Event | Result |
|---|---|---|
| Ready (gen g) | `on_alive(now, g)` | `last_alive = now`; `since` unchanged; no action |
| Ready | `on_alive(_, other gen)` | nothing |
| Starting / Backoff / Failed | `on_alive(_, _)` | nothing. The page beats only after ready, and a beat never makes a page Ready |
| Ready | tick at `now ≥ last_alive + cef_hang_after_ms` | `fail(now)`: Backoff below the count; Failed + `Hide` at it |
| Ready | tick at `now = last_alive + cef_hang_after_ms − 1` | nothing |
| Ready | tick with **both** the hang and the recycle due | **the hang wins**: a counted failure. A hang near the 2 h mark must never be laundered as a planned restart, or 3 hangs at recycles never reach Failed |
| Ready | tick with only the recycle due | the planned restart, as in C05 (no failure) |

## Steps

- [ ] **Step 0:** the question above. Paste the answer, or "no ruling relayed; built the design".
- [ ] **Step 1: config.** Add the two fields, with docs citing Owed 18 and F-001.
  - New inline test: `the_hang_threshold_is_five_beats`, which asserts
    `cef_hang_after_ms == 5 * cef_alive_every_ms` and that `cef_hang_after_ms` is below
    `cef_recycle_every_ms`.
  - Run it red (the fields are missing), then green.
- [ ] **Step 2: tests first** (inline in `graphics.rs`), each failing for the right reason before
  any body is written:
  - `a_beating_page_never_hangs`: Ready, then a beat every `cef_alive_every_ms` for 3× the recycle.
    No `fail` happens: the only actions are the planned recycles.
  - `a_silent_ready_page_hangs_at_exactly_the_threshold`: Ready at `r`. A tick at
    `r + cef_hang_after_ms − 1` does nothing. A tick at `r + cef_hang_after_ms` gives Backoff
    `attempt 0` and the generation unchanged.
  - `the_last_beat_moves_the_deadline`: beats at `r + 2 000` and `r + 4 000`, then silence. The
    hang fires at `r + 4 000 + cef_hang_after_ms`, not before.
  - `three_hangs_in_the_window_fail_and_hide`: hang, back off, restart, ready, hang, and so on. The
    `cef_fail_count`-th hang returns `Some(Hide)` and enters Failed.
  - `a_hang_and_a_crash_share_the_window`: two crashes then a hang (and a hang then two crashes)
    reach Failed at the count.
  - `the_hang_wins_over_a_due_recycle`: the last beat sits at `since + cef_recycle_every_ms −
    cef_hang_after_ms`, so the hang and the recycle fall due on the same tick. The tick records a
    failure, and that is counted.
  - `beats_from_other_generations_and_states_change_nothing`: `on_alive` with `gen ± 1` in Ready;
    any gen in Starting, Backoff and Failed. The state, the generation and the window are equal
    before and after. Count the cases, and fail on 0.
- [ ] **Step 3: implement** `on_alive`, `on_ready`'s `last_alive`, and the Ready arm in `on_tick`
  (hang checked before recycle). Update the module docs: the state diagram gains `Ready --silent
  cef_hang_after_ms--> failure`, and the Failure bullet names the hang.
- [ ] **Step 4: the integration sweeps** (`tests/graphics.rs`).
  - **The pair matrix.** `GEvent` gains `Alive`, and `G_EVENTS` grows to 4. `due()` for Ready is
    `min(since + recycle, last_alive + hang)`. `brief()` gains the rows above, and `is_failure`
    gains `(Ready, Tick)` when the hang is due.
  - The setups gain `Ready` with the hang due, at both 0 and `count − 1` prior failures, so a hang
    lands both below the count and at it.
  - Assert `hides > 0` from a hang specifically. Count those, print them, and fail on 0.
  - **The generated-schedule sweep.** The generator emits beats while Ready, at
    `cef_alive_every_ms` plus jitter, and stops beating in some cases. The existing invariants keep
    holding: "hidden iff Failed", and every restart is a new generation. Add these counters:
    - `hangs`: hangs detected. Fail on 0.
    - `beaten_ready_ticks`: Ready ticks where a beat was within the threshold. Fail on 0.
    - `hang_at_recycle`: ticks where the hang and the recycle were both due. Print the count. If the
      generator cannot reach it in 120 cases, add a directed case, and never accept 0 silently.
- [ ] **Step 5: run.** Run `cargo test --workspace` and paste the counts: total passed and 0
  ignored. All 5 layering guards must pass. Also run `cargo fmt --check`, `cargo clippy
  --workspace --all-targets -- -D warnings` and `scripts/check-portable.sh`.
- [ ] **Step 6: mutate each guard once** (python in-place replace plus a sha check to restore;
  never `git checkout`):
  1. Delete the Ready hang arm. Expected red: `a_silent_ready_page_hangs_at_exactly_the_threshold`,
     plus the matrix.
  2. Change `>=` to `>` in the hang compare. Expected red: the exact-threshold test.
  3. Check the recycle before the hang. Expected red: `the_hang_wins_over_a_due_recycle`.
  4. Make `on_alive` ignore the generation. Expected red:
     `beats_from_other_generations_and_states_change_nothing`.
  5. Make `on_alive` also act in Starting, turning it Ready. Expected red: the same test.
  6. Make `on_ready` leave `last_alive` at 0. Expected red: the exact-threshold test, because the
     hang would fire at once.
  Record all six in the commit body.
- [ ] **Step 7: commit.** `feat(domain): graphics hang detection from the page's alive beat (C05b)`

## Acceptance

- The Step 0 answer is recorded.
- 8 new inline tests pass, and the two sweeps report non-zero hang counts.
- 6 mutants went red, and the tree is clean after each restore.
- `cargo test --workspace` is green with 0 ignored, and clippy, fmt and check-portable are clean.
- `docs/_INDEX.md`: a session-handoff line, and Owed 18's build marked done.

## Do not touch

The ladder, the links, the programme, the supervisor (C06 routes the event), and every adapter
(C11 relays it). Note that `crates/` and `docker/` must not contain the platform name, because
check-portable greps for it.

## Output cap

Under 15 lines.
