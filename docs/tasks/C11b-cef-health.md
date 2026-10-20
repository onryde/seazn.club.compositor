# C11b: the compositor acts on CEF's health (stub)

**Status:** ~~stub~~ **built 2026-10-20 (PR #34; "As built (2026-10-20)" below).** Written as a stub in the
PR #32 review fix loop (2026-10-19) from `docs/_INDEX.md` Owed 55, 56 and 58, and amended in the PR #32 confirm pass
(2026-10-19: N1, N3; Owed 61 and 62). The coordinator dispatched it with this stub as the spec, plus the final C11
review's G1–G4 (2026-10-20).

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
  - **Amended 2026-10-20 (the final C11 review's G3):** the fallback is **not safe with the GPU process's zygote
    dead**. With graphics stopped, the orphaned GPU process's death still aborts the process (probe FS3, 2 of 2), and
    a retry build reaches ready (FS2), so the abort stays armed for as long as the orphan lives (OOM adj 800, ahead of
    the compositor's 0). For that verdict the choice is a restart now or an abort later; nor is it safe at the GPU
    death count (each later death brings the 6th closer). It can apply only to the renderers' zygote and to the
    leaks. And the "2-hour recycle" is a page recycle, not a process restart (as built, false premise (2) below).

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

With its zygote alive, a killed GPU process was relaunched 5 times; the 6th death, ~~about 10 s in
(one kill every 2 s),~~ aborted the process, 2 of 2. **Amended 2026-10-20 (the final C11 review's G2, probe FD):** a
count over the process's life, not a rate: with kills 20 s apart the 6th death, 100 s in, aborted it too; no decay
over 100 s. G1's OOM order puts the GPU process ahead of the
zygotes. Whether Chromium's count decays with time was not measured. The full brief decides
whether `health()` counts GPU-process deaths and C11b restarts before the limit.

## Known residual even then

While a zygote is dead and a build exists, CEF's browser side burns about 1 core in the compositor
process (the review's P7) until that build stops. The restart ends it.

## As built (2026-10-20)

**The domain decides; the adapter counts** (the C10b / Owed 50 lesson: one authority).
- `domain::cef_health`: `CefHealth { unrecoverable: Option<u64>, leaked_teardowns: usize, gpu_process_deaths: usize }`
  (the domain's type; the adapter re-exports it) and `CefHealth::restart_cause(&Thresholds) -> Option<RestartCause>`,
  in urgency order:
  - any dead zygote, **whatever generation it names**: CEF is one per process, so the verdict is the process's;
  - `cef_gpu_deaths_to_restart` (4) GPU-process deaths: `CHROMIUM_GPU_DEATHS_ABORT` (6) less two, one for a death
    the 1 s poll has not read yet and one for a death during the exit;
  - `cef_leaks_to_restart` (3) teardowns leaked at once (a live count).
- `Event::CefHealth(CefHealth)`: while Running, a report that asks for a restart exits at once like a fatal error:
  the final beat (`Stopped`, reason `Restart`, `exitDetail` the cause, e.g. `restart in place: a CEF zygote is gone
  (gen 1)`), then `Exit(75)`. Nothing is drained: the broadcast is to carry on. The final hold, the drain and
  booting ignore it (the hold and the drain already end the process with exit 0; no CEF runs before the session).
  Exited ignores every later report, so the restart is asked for once.
- `ExitReason::Restart`, exit **75** (sysexits' EX_TEMPFAIL): non-zero, so the host's `on-failure` restarts it in
  place; distinct from a crash's 70 and a refusal's 78.
- **The wire, until web adds a word** (Owed 66): `Restart` goes out as `exitReason: "fatal"`, the one word whose
  path web already runs as an in-place restart (R2003b: below RUNNER_CRASH_CAP a crash is only recorded, the Machine
  restarts in place and beats again on the same attempt). A new word now would be refused by web's strict enum and
  the final beat lost. So until Owed 66, each restart is counted as a crash, and the `exitDetail` names it.
- **App:** `GraphicsRenderer::health(&self) -> CefHealth` is the port. The driver reads it once per `tick_due` that
  ran a tick, after the ticks, and hands every read to `decide` (not only a changed one: a report read while booting
  restarts on the first tick on air). A change writes `cef health zygote_gone=<gen|-> leaked_teardowns=<n>
  gpu_process_deaths=<n>`. Nothing is read after `Exit`.
- **Adapter:** the child poll counts GPU-process deaths (`gpu_check`, process-wide beside the zygote watch; one
  `WARN gen=<g>: <n> CEF GPU process(es) gone, <total> in this process` line per poll that saw one). A GPU process
  missing from a list counts only once it is gone, so the final review's G4 orphan (reparented to pid 1 when its
  zygote dies, out of `cef_children()`, alive) is no death; with its zygote dead the domain is already restarting.
  The orphan's memory is outside the PSS cap and its OOM adj is not re-graded, for the at most one poll and one tick
  until the restart; the Machine's stop ends it.
- **Residuals:** a GPU process that lives under one poll (1 s) between two deaths is not seen (the two deaths of
  headroom absorb one); a pid reused before the poll that would see it gone is no death (Owed 70).

**Tests:** domain `tests/cef_health.rs` (11; the sweep checks 1640 reports), the C06 matrix (+2 kinds, 182 pairs) and
exit paths (+1, 9); app `tests/cef_health.rs` (7: each trigger through the real driver to one final beat and one
`Exit(75)`, then no port call and no read), `run.rs` (+1: the final beat posts before `run()` returns 75); adapters:
`gpu_check`'s unit sweep (32 polls), the contract sweep (+1 session, `fatal` and 75), and on the image
`gpu_process_deaths_are_counted_from_real_kills` (4 kills in a child, each counted) and G4 in the GPU-zygote case (an orphan alive, out of the tree, no death; at least one orphan, or it fails).
Mutants: 12 domain, 7 app, 8 adapters (4 on the image), all red, each at its own assertion.

**False premises found** (recorded in `docs/_INDEX.md`):
1. "Rule out a stale gen" (What, Domain): the zygote verdict is the process's; a verdict naming an older page still
   means CEF is dead, and ignoring it keeps a dead CEF on air until the next GPU-process death aborts it.
2. "let the 2-hour scheduled restart clear them" (Why; the ruling's text) and "the existing 2-hour recycle" (What):
   `cef_recycle_every_ms` recycles the PAGE (a new build in the same process). No scheduled process restart exists,
   and a leaked teardown is a blocked thread only the process's exit frees. So fewer than 3 leaks stay until the
   session ends (or a restart for another cause). Reported to the coordinator (owner question).
3. "C11b must give this exit a code or `exitDetail` of its own, so neither charges nor alarms it" (exit accounting):
   no compositor-side value can. Web's crash tally counts any final `fatal` beat and any non-zero exit but 78 that Fly
   restarts (R2003b `crashExitSeen`), and Fly charges every non-zero exit to `max_retries`. Web needs a change
   (Owed 66), and Fly's charge comes with any exit (Owed 67).
4. "The outage budget: YouTube's auto-stop grace bounds it" (60–90 s): the binding limit is the broadcast SPLIT,
   15–30 s (F-004 addendum; `OUTPUT_GAP_BUDGET_MS` = 15 s): a 30 s gap starts a new broadcast long before the
   auto-stop. C14's plan measures against 15 s.
5. "C13's runbook" (What, Docs): the runbook is C17's (`docs/runbooks/relay-compositor.md`, C17 brief, Runbook).
6. The dispatch's "the GPU death count is the one allowed adapter edit": a new `ExitReason` forces the wire's
   exhaustive map (`exit_reason_wire`) and its two exhaustive tests, and the port's new method forces
   `CefGraphics`'s impl.

**C14:** the measurement plan and the cases are in C14's brief ("Amendment 2026-10-20 (C11b)").

## Output cap

Under 15 lines.
