# C17: Stg end-to-end acceptance (owner-watched) and the runbook

**Repos:** both. **Depends on:** everything (C14 3× green, C15 green, C16 stg promoted, R2001–R2006
merged and deployed to stg). **Amended 2026-10-15:** also R2007 PR A + PR B and R2004's PR 0 (the
relay stop contract) deployed to stg. See "Amendment 2026-10-15 (overlay templates spec)" at the end.

## Context

This task covers spec §9.7 items 3 and 4. It is the **only** proof that the seams are live through
their real producer and consumer:

```
capture app → CF Input A → the compositor image → CF Input B → CF Output → YouTube
                         + stg web control plane + the real overlay page
```

Every earlier task proved a half with a fake on the other end. AGENTS.md failure class 1 (the inert
seam) is what this task exists to catch.

The orchestrator drives. **The owner watches and gives a verdict per screen.** "It worked" is not a
verdict (AGENTS.md class 11).

## Files

- Create: `docs/runbooks/relay-compositor.md` (compositor repo). Its contents are below.
- Create: `docs/acceptance/2026-xx-xx-r2-stg.md` (compositor repo), named with the actual run date.
  It records the observation table, verbatim.
- Modify: `docs/_INDEX.md` (status → R2 accepted, with a link to the acceptance doc)
- Modify: `seazn.club` `docs/superpowers/specs/2026-09-…-stream-overlay-prompts/_INDEX.md`. Re-pin
  the folder name. Update the pointer row only: status, plus a link.

## Runbook (`docs/runbooks/relay-compositor.md`)

The heartbeat can raise each of these signals. Each one gets four lines: **what it means**, **what
the viewer sees**, **what to check**, and **what to do**.

| Signal (heartbeat field) | Meaning |
|---|---|
| `inputKickAlert: true` | CF closed playback more than `kick_alert_count` times in `kick_window_ms`: a second reader, or CF maintenance |
| `input: backoff` for > 60 s | camera gone: phone backgrounded, or capture stopped |
| `graphics: failed` | CEF failed 3× in 5 min; the camera goes out without the overlay |
| `scoreStale: true` | the page can't reach the score feed |
| `rung != full` | encoder overrun; the ladder stepped down |
| `output: backoff` | can't reach CF Input B |
| session `lost` (web side, 3 missed beats) | Machine gone; `r<n+1>` cold-starts (~60 s) |
| Machine exit 78 | refused: token, 404, or wrong mode |
| Machine exit 70 | fatal: programme start failed |

Also include:
- the **YouTube settings organisers should use** (owner ruling 2026-10-15): stream latency **Normal**.
  The score is burned into the picture, so latency never desyncs it, and Normal rebuffers least on
  phones and club Wi-Fi. The cost: YouTube runs ~15–30 s behind the public fixture page's live
  score. Add auto-stop guidance from C00's YouTube gap-tolerance measurement.
- the **post-match health review**: run `seazn.club` `docs/runbooks/relay-health-review.sql` (R2002)
  against the session id
- the **rollback**: `promote.yml` with the previous ref (C16)
- **manual stop**: end the session in the organiser UI. **Never** `flyctl machine destroy` a live
  session's Machine, because the sweep would book an `r<n+1>`.

## Steps

- [ ] **Step 1: Preflight** (orchestrator, no owner needed):
  1. Stg web is on a commit containing R2001–R2006.
  2. `RELAY_IMAGE` on `seazn-club-stg` equals the C16 promoted ref.
  3. The stg fixture has a YouTube destination with a **private/unlisted** broadcast.
  4. The capture app build is the one in `seazn.club.capture` main.

  Record all four, with values (refs and ids only, no secrets).
- [ ] **Step 2: Book the run with the owner.** Book about 25 min. Share the runbook draft first.
- [ ] **Step 3: Run the script, with the owner watching** YouTube, the organiser panel and the
  public fixture page. The orchestrator records each row's observed time and the owner's verdict.

  | # | Action | Expected (from spec §6) | Seen | Verdict |
  |---|---|---|---|---|
  | 1 | Organiser picks *Composed*, delay default, Go live | panel shows starting → live; YouTube live ≤ cold start + 15 s | | |
  | 2 | Score 3 rallies on the pad | each appears on YouTube at about real time + delay; the bug matches the public page | | |
  | 3 | Phone: background the capture app for 20 s | YouTube freezes ≤ 0.5 s, then slate by 3 s; session stays live; panel shows R2006's *camera reconnecting* chip (from the beat's `programmePhase`) | | |
  | 4 | Foreground the app | live again ≤ 1.2 s after frames return; score bug intact | | |
  | 5 | Void the last rally | bug re-derives on YouTube | | |
  | 6 | Set the delay override to 0, start a second session | score lands ahead of video by about the measured delay (proves the override reaches the page) | | |
  | 7 | End the session | YouTube ends cleanly; Machine exits 0; no `r2` attempt row | | |
  | 8 | Post-match health query | every new heartbeat field populated, none 0-for-unknown | | |

- [ ] **Step 4: Screens.** Take screenshots of the organiser panel during rows 1, 3 and 7, at 320, 768
  and 1280. Crop them; full-page captures flood context. **Store them in the acceptance PR's comment
  thread or the scratchpad, never in the repo** (house rule: no evidence PNGs in the repo).
- [ ] **Step 5: Handle reds.** Any red is a finding against its owning task. Fix it there, through
  the review loop, then re-run **the whole script**, not just the red row.
- [ ] **Step 6: Finish the paperwork.** Finish the runbook with the timings actually seen. Commit the
  acceptance doc, plus the pointer-row update, as a PR in each repo.
- [ ] **Step 7: Prod.** Prod promotion is **not** part of this task. Ask the owner whether to tag
  `v0.1.0` and run `promote.yml env=prod`.

## Acceptance

- All ~~8~~ 11 rows (amended 2026-10-15) have the owner's **pass** verdict, recorded verbatim.
- The runbook covers every signal in the table.
- Both pointer docs are updated.
- (Amended 2026-10-15, coordinator decision G1) **The compatibility records exist for the release
  under test**: C11's `cefsrc` scene check (`_FINDINGS.md` F-0xx, with the scene leg run, not owed)
  and R2007 step 14b's OBS browser-source check. Link both from the acceptance doc. A missing record
  fails acceptance; if C11 recorded the scene leg as owed, this run's rows 1, 3 and 7 discharge it,
  and the acceptance doc says so.

## Amendment 2026-10-15 (overlay templates spec)

Authority: `seazn.club: docs/superpowers/specs/2026-10-15-overlay-templates-and-scenes-design.md`
§4.2, §4.3, §5 and §6. These rows were owed by the plan's "Amendments 2026-10-15" note and now live
here; the plan keeps a pointer only.

**Preflight additions (Step 1):**
5. Stg web contains R2007 PR A + PR B and R2004 PR 0 (record the refs).
6. The composed Machine's config shows `stop_config.signal = "SIGTERM"` (read it from the Machines
   API by id; record the value). This is coordinator decision I4, as PR 0 shipped it.

**Row changes (Step 3).** The rows keep their numbers; the expected column changes as follows.

| # | Expected, amended |
|---|---|
| 1 | Before the first camera frame, YouTube shows the **Starting soon** scene: the opaque backdrop and the Upcoming card (or the live/decided card if the match has started). Then live. |
| 3 | By 3 s, the **Reconnecting scene**: the backdrop and the Reconnecting card ("Camera reconnecting, back shortly"), with the score template still visible. It is not a bare slate. A rally scored during it raises its moment slab above the card. |
| 7 | The **Final scene** (the Result card if decided or void, else Stream ended with the score) for about `FINAL_HOLD_MS` = 10 s, then YouTube ends cleanly. The Machine exits 0. Its last beat's `exitReason` is `signalled` (web's SIGINT came first, the usual case) or `ended` (the `ending` reply came first); record which. An earlier beat carries `programmePhase: "final"`. |
| 8 | Also: the health query shows `phaseChannel: "loopback"`, the permanent proof of C11 Step 0 on a real Fly CEF. `web` there is a finding against C11/C12, not a pass. If C11 Step 0 recorded outcome (c), the expected value is `web`, and the acceptance doc cites that F-id. |

**New rows:**

| # | Action | Expected | Seen | Verdict |
|---|---|---|---|---|
| 9 | Before row 6's second session: the organiser picks the non-default template (R2006's picker), then Go live | YouTube shows the picked template, at its placement; a moment slab appears at that template's `slabPlacement` | | |
| 10 | On row 6's second session, the orchestrator runs `fly machine stop <machine id>` with **no** signal flag (a platform stop) | The picture stops **without** the Final scene; the last beat carries `exitReason: "signalled"`; no beat carries `programmePhase: "final"`. Then, per R2003's ruling (corrected by confirm pass N3; `domain/runner.ts:96-100` on `f9034f575`): `signalled` is record-only, exit 0 is never restarted, `auto_destroy` removes the Machine, and the composed stale window takes the runner `lost` → force_destroy → **the ONE retry**. So expect exactly one `r2` attempt row and the stream resuming on it after a cold start (F-004 addendum: the broadcast splits, 15–30 s). Record the gap seen and whether YouTube kept the broadcast. Then end the session from the organiser UI: r2 shows the Final scene (row 7's check) | | |
| 11 | The owner opens the OBS browser source from R2007 step 14b's record for this release | The record exists and its verdicts are all pass | | |

Row 10 is the live proof of the SIGINT/SIGTERM split, and its expected result follows from C06's signal
table and R2003's retry rule. If a platform stop shows the Final scene, PR 0's `stop_config` did not
take, so route it to PR 0. The retry is **expected** behaviour, not a finding. If the owner would
rather a platform stop not retry, that is an owner question for R2003, not a defect.

**Screens (Step 4):** add YouTube crops of the Starting soon, Reconnecting and Final scenes (rows 1, 3
and 7) and of the picked template (row 9). The same rule applies: never in the repo.

## Do not touch

Prod.

## Output cap

The filled table, plus PR URLs.
