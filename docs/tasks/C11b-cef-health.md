# C11b: the compositor acts on CEF's health (stub)

**Status:** stub, written in the PR #32 review fix loop (2026-10-19) from `docs/_INDEX.md` Owed 55,
56 and 58. The coordinator owns it: dispatch, scope and sequencing are theirs. Expand it into a full
brief before dispatch.

**Repo:** `seazn.club.compositor`. **Depends on:** C11 (PR #32). **Sequenced before** C14 (its
`cef-zygote-killed` and leak cases need the restart this task adds). It is its own task because
C11's do-not-touch list includes the domain and app crates.

## Why

**Owner ruling 2026-10-19 "B"** (in chat, relayed by the coordinator; Decision log):

- **Owed 56, a dead renderer zygote:** CEF can start no renderer again in this process (F-013). The
  compositor **restarts in place**.
- **Owed 55, a teardown that never returns:** each one leaks a thread, a pipeline and a live renderer
  (about 22–38 MB PSS, the PR #32 review's P6). The compositor **warns, counts the leaks, lets the
  2-hour scheduled restart clear them, and requests an early in-place restart once 3 have leaked**.

C11 reports both as signals of their own and acts on neither:

- `CefGraphics::health()` → `CefHealth { unrecoverable: Option<u64>, leaked_teardowns: usize }`.
- `unrecoverable` is `Some(gen)` from the first child poll (1 s) that finds the renderers' zygote
  gone, or every zygote gone. `gen` is the newest build's generation then. It covers the review's P7
  case, where the current build's renderer is killed after the zygotes and no `CefCrashed` follows in
  3 s: the poll decides on the zygote alone. A dead GPU-process zygote is no verdict (CEF recovered,
  F-013). An ERROR line is logged once.
- `leaked_teardowns` counts retired builds whose teardown had not returned `TEARDOWN_LEAK_AFTER_MS`
  (3 000) after the reaper's kill. It is counted down if one returns after all. Each is logged at WARN
  with the running count.

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
- **Docs:** amend the C05b, C06, C08 and C12 briefs; close Owed 55 and 56.

## Known residual even then

While a zygote is dead and a build exists, CEF's browser side burns about 1 core in the compositor
process (the review's P7) until that build stops. The restart ends it.

## Output cap

Under 15 lines.
