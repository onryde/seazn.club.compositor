# C11b: the compositor acts on CEF's health (stub)

**Status:** stub, written in the PR #32 review fix loop (2026-10-19) from `docs/_INDEX.md` Owed 55,
56 and 58, and amended in the PR #32 confirm pass (2026-10-19: N1, N3; Owed 61 and 62). The
coordinator owns it: dispatch, scope and sequencing are theirs. Expand it into a full brief before
dispatch.

**Repo:** `seazn.club.compositor`. **Depends on:** C11 (PR #32). **Sequenced before** C14: its case
8 (`cef-killed`) must be rewritten (below), and the in-place restart this task adds needs C14 cases
of its own. C14's brief has no `cef-zygote-killed` or leak case yet. It is its own task because
C11's do-not-touch list includes the domain and app crates.

## Why

**Owner ruling 2026-10-19 "B"** (in chat, relayed by the coordinator; Decision log):

- **Owed 56, a dead zygote:** the renderers' zygote's death means CEF can start no renderer again in
  this process (F-013). The compositor **restarts in place**. The confirm pass (N1): the GPU
  process's zygote's death is the same signal, because the next GPU-process death after it is
  Chromium's FATAL "GPU process isn't usable. Goodbye." and the whole process dies of SIGTRAP: off
  air, at a moment the kernel chooses. The coordinator's decision: one signal, one restart.
- **Owed 55, a teardown that never returns:** each one leaks a thread, a pipeline and a live renderer
  (about 22–38 MB PSS, the PR #32 review's P6). The compositor **warns, counts the leaks, lets the
  2-hour scheduled restart clear them, and requests an early in-place restart once 3 have leaked**.

C11 reports both as signals of their own and acts on neither:

- `CefGraphics::health()` → `CefHealth { unrecoverable: Option<u64>, leaked_teardowns: usize }`.
- `unrecoverable` is `Some(gen)` from the first child poll (1 s) that finds any zygote gone (a zygote
  missing from one poll's list counts only once its process is gone). `gen` is the newest build's
  generation then. It covers the review's P7 case, where the current build's renderer is killed
  after the zygotes and no `CefCrashed` follows in 3 s: the poll decides on the zygote alone. An
  ERROR line is logged once. While a GPU zygote is dead and its GPU process lives, the page keeps
  painting, so nothing else shows it; the restart must come before that GPU process dies.
- `leaked_teardowns` counts retired builds whose teardown had not returned `TEARDOWN_LEAK_AFTER_MS`
  (3 000) after the reaper's kill. It is counted down if one returns after all. Each is logged at WARN
  with the running count.
- **The threshold of 3 is a live count** (the coordinator's recommendation, matching the gauge): the
  restart is asked for while 3 teardowns are leaked at once, not after 3 in the process's life. A
  leak that returns after all is no longer counted. The ruling's words allow either.

They are an accessor, not `Event`s, because C11 does not touch the domain crate.

## What (to be confirmed in the full brief)

- **Domain:** a way to say "graphics is unrecoverable for this process" and "N leaked". For example,
  `Event::CefUnrecoverable { gen }` and `Event::CefLeaked { count }`, and an `Effect` that asks for an
  in-place restart. The ruling's thresholds are 3 leaks and the existing 2-hour recycle. Rule out a
  stale gen, and a second report. Decide how the restart meets the final hold and the drain (C06,
  C08), and what the heartbeat says meanwhile.
- **App:** `GraphicsRenderer::health()`, or an adapter-pushed event on the existing channel; the
  apply arm for the restart effect. "In place" means the process re-execs or exits for Fly's
  `on-failure` (ADR 0004: max_retries 2). Which one is a full-brief question. Check that an exit for
  this reason does not spend the session's retries meant for crashes (R2003, R2003b).
- **Adapters:** none expected, beyond pushing the signals as events if that is the chosen shape.
- **Tests:** the second call (a second zygote verdict; a 4th leak); the empty case (a healthy
  process reports `CefHealth::default()`); a leak that returns after all (count down); a restart
  during the final hold.
- **Docs:** amend the C05b, C06, C08, C12 and C14 briefs (case 8, the new cases, the budget) and
  C13's runbook (the exit's code, what an operator sees); close Owed 55, 56, 61 and 62.

## C14's case 8, rewritten (the confirm pass's N3; Owed 62)

C14's case 8 (`docker exec compositor pkill -KILL -f cef` at 20 s) kills every CEF child: both
zygotes, the GPU process and the renderers. Its brief expects `CefCrashed{g}` within 2 s and then
`RestartGraphics{g+1}` and `CefReady{g+1}`. That cannot hold on CEF 139:
- **Measured (probe PK, 3 of 3, each in a process of its own):** every CEF child SIGKILLed at once
  aborted the process (Chromium's FATAL, SIGTRAP) before the next child poll reported anything. No
  event follows, and C11b's in-place restart cannot come first.
- **So case 8 asserts case 9's shape:** tini exits non-zero and compose's `on-failure` restarts the
  compositor; output resumes within `BUDGET_PROCESS_RESTART_OUTPUT_MS` (15 s) of the kill; heartbeats
  resume; fake-control sees the same `SESSION_ID`; `counter_regressions == 0`. It also records the
  exit as C11b decides to count it (below).
- **The in-place restart gets cases of its own:**
  - the GPU process's zygote killed alone (`kill -KILL` on its pid, from the CEF tree in `/proc`):
    `health().unrecoverable` within 2 s, then the restart in place within the outage budget,
    before any GPU-process death; the process must not abort;
  - the renderers' zygote killed alone: the same, with the orphaned renderer gone after the restart;
  - 3 leaked teardowns at once (a test hook: C11's `hold_teardowns`): the restart; 2 leaked, none.

## The outage budget (the confirm pass's N3)

An in-place restart takes the programme off air like case 9's process restart. YouTube's auto-stop
grace bounds it: C00's Step D survived 5 s and 15 s gaps and ended the broadcast about 60–90 s after
the final stop (F-004), and ADR 0004 asks that an outage stay well under about 60 s.
- The budget: `BUDGET_PROCESS_RESTART_OUTPUT_MS` (15 s, C14) unless the full brief sets another,
  always well under 60 s.
- C14 measures it, from the decision to the first programme packet out, in the cases above.
- **Over budget, the fallback is to stay on air without the overlay** (the coordinator's decision):
  graphics `Failed`, the programme runs on, and the 2-hour recycle (or the session's end) restarts
  the process.

## How an exit-based restart is counted (the confirm pass's N3)

If "in place" means an exit for Fly's `on-failure` (ADR 0004: `max_retries` 2), the exit reads as a
crash twice over:
- **Fly** counts the restart against `max_retries`, so a third such exit in a session ends it.
- **R2003b** counts crashes as `restart_count + 1`, so the web app alarms on it.

C11b must give this exit a code or `exitDetail` of its own, so neither charges nor alarms it as a
crash, or record why that is acceptable. A code or field the web reads is a `seazn.club` change (the
web repo), which the full brief must name with its owner. A **re-exec** keeps the pid instead:
- the profile sweep handles that since the confirm pass (N2);
- the CEF children that were not orphaned (the GPU zygote, the utilities) become the new image's
  descendants, where `cef_children()`, the claims, the PSS cap and the zygote watch will count them.
  The full brief must say how they are reaped first.

## Also measured in the confirm pass (Owed 61)

With its zygote alive, a killed GPU process was relaunched 5 times; the 6th death, about 10 s in
(one kill every 2 s), aborted the process, 2 of 2. G1's OOM order puts the GPU process ahead of the
zygotes. Whether Chromium's count decays with time was not measured. The full brief decides
whether `health()` counts GPU-process deaths and C11b restarts before the limit.

## Known residual even then

While a zygote is dead and a build exists, CEF's browser side burns about 1 core in the compositor
process (the review's P7) until that build stops. The restart ends it.

## Output cap

Under 15 lines.
