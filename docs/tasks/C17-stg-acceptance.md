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
| `rung != full` | **sizing alarm** (amended 2026-10-18, Owed 20/25; see below): encoder overrun, the ladder stepped to `overlay_half_rate` |
| `output: backoff` | can't reach CF Input B |
| session `lost` (web side, 3 missed beats) | Machine gone; `r<n+1>` cold-starts (~60 s) |
| Machine exit 78 | refused: token, 404, or wrong mode |
| Machine exit 70 | fatal: programme start failed |
| Machine exit 75 (amended 2026-10-20, C11b) | an in-place restart asked for by CEF's health: a dead zygote, 3 GPU-process deaths (`cef_gpu_deaths_to_restart`), or 3 leaked teardowns. Its final beat reads `exitReason: "fatal"` (until web adds a word, Owed 66) with `exitDetail` starting `restart in place: `; the log has `cef health …` and the adapter's ERROR/WARN line. The viewer sees an outage of about a process restart (C14's measurement), then the score back |

Also include:
- the **YouTube settings organisers should use** (owner ruling 2026-10-15): stream latency **Normal**.
  The score is burned into the picture, so latency never desyncs it, and Normal rebuffers least on
  phones and club Wi-Fi. The cost: YouTube runs ~15–30 s behind the public fixture page's live
  score. Add auto-stop guidance from C00's YouTube gap-tolerance measurement.
- the **post-match health review**: run `seazn.club` `docs/runbooks/relay-health-review.sql` (R2002)
  against the session id
- the **rollback**: `scripts/promote.sh stg <previous ref>@sha256:<digest>`, run by the owner on their
  own flyctl login; promote prints that command as its `Rollback:` line (C16, amendment 2026-10-25:
  promote.yml is removed). (Amended 2026-10-26, C16's moving tags.) While RELAY_IMAGE tracks `:beta`,
  roll back either way: pin a previous build with that command (each deploy-stg summary prints its
  build's pin; while pinned, `:beta` is not followed), or let the next merge move `:beta` on. A re-run
  of an older deploy-stg run never moves `:beta` back: it warns and prints that build's pin (PR #94
  fix round 1)
- **manual stop**: end the session in the organiser UI. **Never** `flyctl machine destroy` a live
  session's Machine, because the sweep would book an `r<n+1>`.

**Amendment 2026-10-18 (owner rulings on the ladder and on hang detection).** These two runbook
entries are written in full, in the runbook's four-line form:

- **`rung != full`: the sizing alarm.** Owner, 2026-10-18, in chat, relayed by the coordinator: "no
  lite mode; if CPU impacts, measure and raise the CPU size".
  - **Meaning:** the Machine is short of CPU. The encoder overran, frames were dropped before it, and
    the ladder stepped to `overlay_half_rate`. The bottom holds for the rest of that process.
  - **Viewer sees:** the score stays on air, with the overlay refreshed at half rate (15 fps). The
    match is not affected.
  - **Check:** web logs `relay sizing alarm` at WARN (`alarm: "sizing"`, with `sid`, `orgId`, `rung`,
    `previousRung`, `attempt` and `machineId`) and reports it to Sentry under route
    `relay.sizing_alarm`, once per step and per attempt (`seazn.club` #980). In the health review,
    the step shows in `rung_stepped_beats`.
  - **Alert rule (the simplest that cannot go quiet):** a Sentry issue alert **on every event**
    whose tag `route` is `relay.sizing_alarm`, sent to the ops channel. The message is fixed, so
    every alarm groups into ONE issue. An alert on "a new issue" would fire once, ever, and then
    stay silent while the issue stays open. So the rule is per event, not per issue. Resolving the
    issue after a resize is housekeeping, and the alert does not depend on it.
  - **Do:** nothing during the match. Afterwards, raise the Machine size: a `seazn.club` PR moves R1's
    `RUNNER_DEFAULT_GUEST` to the next performance size, after C15's cost legs pass on it (Owed 25).
- **A hung overlay page** (Owed 18, "2 Ok", 2026-10-18).
  - **Meaning:** the page stopped its 2 s `seazn:alive` beat for `cef_hang_after_ms` (10 s) after
    ready. C05b counts that as a failure, the same as a crash, and **hides the overlay at once**
    (owner-approved design (2026-10-18); "at once" is the coordinator's reading of it).
  - **Viewer sees:** the frozen last frame for up to 10 s, while the hang is being detected. Then
    **no overlay** for about 4 s (the 1 s back-off plus the reload), so no stale score is shown.
    Then the overlay is back. After 3 failures in 5 min, it stays hidden for the cooldown
    (`graphics: failed`).
  - **Check** (amended again 2026-10-18, C05b PR #22 review I2; the hide names its reason):
    - The compositor log shows `overlay hidden reason=hang gen=<n>`, then `graphics restart
      gen=<n+1>`, then `cef ready gen=<n+1>` (C12 amendment).
    - The beats show `restarting`/`starting` → `ready`.
    - `reason=failed` is the count-th failure (`graphics: failed`).
    - The superseded reading: ~~`overlay hidden` with no `cef crashed` before it, then
      `graphics restart gen=<n+1>`, then `cef ready gen=<n+1>`. Precisely: generation `n` logged
      `cef ready gen=<n>` and no `cef crashed gen=<n>` before the hide, which lands about
      `cef_hang_after_ms` after its last `cef alive gen=<n>`. A hide whose generation never logged
      `cef ready` is a page that never loaded (load timeouts), not a hang.~~
  - **Do:** a single hang needs nothing. Repeated hangs mean the page itself is broken: look at the
    overlay page's recent deploy (R2005/R2007), not at the Machine.
- **A crashed overlay page that does not come back** (amended 2026-10-18, C05b PR #22 review G1;
  coordinator decision, relayed).
  - **Meaning:** the CEF renderer crashed (or went over `cef_rss_cap_mb`), and its reloads have not
    signalled ready.
  - **Viewer sees:** the last frame held through the reload, until 10 s (`cef_hang_after_ms`) after
    the crashed page's last beat (amended 2026-10-18, the review's second round: the one bound runs
    from the page's last proof of life, not the crash), so 8–10 s after the crash. Then **no
    overlay** until a reload is ready. A reload that is ready inside the window shows no gap.
  - **Check:** `cef crashed gen=<n>`, then `overlay hidden reason=crash gen=<n>` within 10 s (it
    names the crashed generation, not the reload). Then `cef ready gen=<m>` for the first reload that
    loads. If none loads, `overlay hidden reason=failed` follows, with `graphics: failed`.
  - **Do:** as for a hang. Repeated crashes point at the page or at the Machine's memory
    (`cef_rss_cap_mb`).
- **A recycled overlay page that does not come back** (amended 2026-10-18, C05b PR #22 review, second
  round; coordinator decision, relayed).
  - **Meaning:** the planned 2 h recycle restarted the browser, and the reload has not signalled
    ready. A recycle is not a failure; its reload's load timeouts are.
  - **Viewer sees:** the last frame held through the reload, until 10 s after the recycled page's
    last beat (at most 10 s after the recycle). Then **no overlay** until a reload is ready. It was
    about 33 s before this amendment: the hold ran through every load timeout to Failed.
  - **Check:** `graphics restart gen=<n+1>` at the recycle, then `overlay hidden reason=recycle
    gen=<n>` within 10 s (it names the recycled page). Then `cef ready gen=<m>` for the first reload
    that loads, or the load timeouts and `overlay hidden reason=failed` with `graphics: failed`.
  - **Do:** a single one needs nothing. Repeated ones mean the page loads slowly: look at the
    overlay page's load (R2005/R2007) and the Machine's network.
- **A CEF bump** (C16 amendment 2026-10-18, Owed 22). The steps, in order:
  1. Bump `docker/GSTCEF_COMMIT`.
  2. Build the image.
  3. Re-run C11 Step 0 on it, read by V2, any request reached, and `probe-ms`.
  4. Record F-0xx.
  5. Update `docker/CEF_STEP0_PASS`.
  6. Only then merge, deploy and promote. `check-cef-bump.sh` refuses otherwise.

**Amendment 2026-10-18, owed checks become acceptance criteria** (Owner, 2026-10-18, in chat, relayed by the coordinator: "apply rec 4-9").

**Row 12, live: the crash cap reads Fly's exit after the restart (R2003b; Owed 11).**
- *Why.* No live run yet shows that Fly's Machine GET still carries the crash's `exit` event once
  Fly reports `started` again. R2003b's `CrashTally` counts a slow loop "one exit at a time,
  including on a `running` read after Fly already restarted it" (web `server/relay/config.ts`,
  `RUNNER_CRASH_CAP`). That holds only if `events[]` keeps the exit.
- *The run.* On a composed stg session, crash the compositor three times with
  `fly machine exec <id> 'pkill -KILL -x compositor'` (tini exits non-zero, and `on-failure`
  restarts it in place).
  - Space the crashes past the ~330 s refill (F-007), so each one is a restarting exit with
    `restart_count` back to 1.
  - Observe each crash **only after its restart has completed**: wait until the Machines API GET
    shows `state: started`, with a `start` event newer than the exit.
- *Record.* For each crash:
  - the GET body's `events[]` entries for that exit (type, timestamp, `request.exit_event`,
    `request.restart_count`; ids only, no secrets)
  - the session's crash tally as web stored it
- *Expected.*
  - After crashes 1 and 2, the session stays live and the tally reads 1, then 2.
  - At crash 3, the session **fails `machine_crash`**, and no `r2` attempt row is created (the cap
    spends no retry).
  - The exit event is present in `events[]` on every `started` read. Its absence is a finding
    against R2003b, never a pass.
- The owner books about 20 minutes more for this row (Step 2).

**G1, both compatibility records on stg (Owed 21).** This is an acceptance criterion, gated on R2005
and R2007 PR A (#974) both being on stg. #974 merged at `99c477755` and is deployed. R2005 is not
yet merged.
- Preflight records the stg web commit and confirms both.
- Then C11's compatibility step runs against stg's real page:
  - **The template leg:** the real overlay page, with each `TemplateId` (`bar`, `bug`), through
    `cefsrc`. `seazn:ready` arrives, and the template renders.
  - **The scene leg:** the probe server's `--script waiting:5000,reconnecting:5000,final:5000`
    through CEF, with the probe server logging at least 3 `GET /phase` lines.
- The page is loaded from the host the composed `overlayUrl` names (C12's `allowed_origin`), with
  its CSP in force (Owed 23). That is the first CEF run of the CSP, whose `connect-src` criterion
  R2005's acceptance carries (A5, E8).
- If R2005 is not on stg when C17 runs, C17 is **not accepted**. The gate is recorded, never
  skipped.

## Steps

- [ ] **Step 1: Preflight** (orchestrator, no owner needed):
  1. Stg web is on a commit containing R2001–R2006.
  2. ~~`RELAY_IMAGE` on `seazn-club-stg` equals the C16 promoted ref.~~ (Amended 2026-10-26, C16's
     moving tags.) `RELAY_IMAGE` tracks `:beta`, whose digest is `<x>`. Record `<x>` as the
     `:beta` line of the latest deploy-stg run's summary, or as the `following:` line of
     `scripts/promote.sh stg --track beta`, which is also how to check the owner's hand-set value.
     If RELAY_IMAGE is pinned instead, record the pin, and that `:beta` is not followed.
  3. The stg fixture has a YouTube destination with a **private/unlisted** broadcast.
  4. The capture app build is the one in `seazn.club.capture` main.
  5. (Amended 2026-10-26, the PR #94 review's m-5; this item needs the owner.) **Pin stg for the
     run's duration**, so no merge during the run changes what new sessions take: on their go-ahead,
     the owner runs `scripts/promote.sh stg <sha>@sha256:<x>` (the pin that `<x>`'s deploy-stg
     summary prints) on their own flyctl login. Record its `following:` line (pinned). Step 6
     re-tracks.

  Record all five, with values (refs and ids only, no secrets).
- [ ] **Step 2: Book the run with the owner.** Book about 25 min. Share the runbook draft first.
- [ ] **Step 3: Run the script, with the owner watching** YouTube, the organiser panel and the
  public fixture page. The orchestrator records each row's observed time and the owner's verdict.

  | # | Action | Expected (from spec §6) | Seen | Verdict |
  |---|---|---|---|---|
  | 1 | Organiser picks *Composed*, delay default, Go live | panel shows starting → live; YouTube live ≤ cold start + 15 s | | |
  | 2 | Score 3 rallies on the pad | each appears on YouTube at about real time + delay; the bug matches the public page | | |
  | 3 | Phone: background the capture app for 20 s | YouTube freezes ≤ 0.5 s, then slate by 3 s; session stays live. ~~panel shows R2006's *camera reconnecting* chip (from the beat's `programmePhase`)~~ **Amended 2026-10-19:** R2006 deferred the chip (PR #989; `_INDEX.md` Owed 65). Until the follow-up ships it, record what the panel shows (a live session, no camera chip) and don't judge the row on it. Once it ships, the chip is this row's pass criterion again. | | |
  | 4 | Foreground the app | live again ≤ 1.2 s after frames return; score bug intact | | |
  | 5 | Void the last rally | bug re-derives on YouTube | | |
  | 6 | Set the delay override to 0, start a second session | score lands ahead of video by about the measured delay (proves the override reaches the page) | | |
  | 7 | End the session | YouTube ends cleanly; Machine exits 0; no `r2` attempt row | | |
  | 8 | Post-match health query | every new heartbeat field populated, none 0-for-unknown | | |

- [ ] **Step 4: Screens.** Take screenshots of the organiser panel during rows 1, 3 and 7, at 320, 768
  and 1280. Crop them; full-page captures flood context. **Store them in the acceptance PR's comment
  thread or the scratchpad, never in the repo** (house rule: no evidence PNGs in the repo).
- [ ] **Step 5: Handle reds.** Any red is a finding against its owning task. Fix it there, through
  the review loop, then re-run **the whole script**, not just the red row. A fix that merges moves
  `:beta`, not the pin: re-pin stg to the fix's build (Step 1 item 5) before the re-run.
- [ ] **Step 6: Finish the paperwork.** Finish the runbook with the timings actually seen. Commit the
  acceptance doc, plus the pointer-row update, as a PR in each repo. (Amended 2026-10-26, the PR #94
  review's m-5.) Then re-track: the owner runs `scripts/promote.sh stg --track beta`, and the
  orchestrator records its `following:` line, so stg follows `:beta` again.
- [ ] **Step 7: Prod.** Prod promotion is **not** part of this task. Ask the owner whether to tag
  `v0.1.0` and run `scripts/promote.sh prod v0.1.0@sha256:<digest> --confirm-prod` (C16, amendment
  2026-10-25), and only after FLY_API_TOKEN_RELAY_PROD exists: the owner mints it after this task passes, if the
  owner decides a repository token is acceptable for prod (compositor `_INDEX.md`, C16 open item (f):
  its reach includes every live prod session's stream key). (Amended 2026-10-26, C16's moving tags.)
  `:stable` exists only after the first `v*` tag's deploy-prod run creates it, so prod's RELAY_IMAGE is
  set to `registry.fly.io/seazn-relay-prod:stable` only after this task passes and that run has
  completed. The owner sets it once (`scripts/promote.sh prod --track stable --confirm-prod` is the
  verified way). seazn-club-prod showed all 49 secrets Staged on 2026-10-26, so any `secrets set` or
  deploy there ships them all, and promote refuses until the owner resolves them.

## Acceptance

- All ~~8~~ 11 rows (amended 2026-10-15) have the owner's **pass** verdict, recorded verbatim.
- The runbook covers every signal in the table.
- Both pointer docs are updated.
- (Amended 2026-10-15, coordinator decision G1) **The compatibility records exist for the release
  under test**: C11's `cefsrc` scene check (`_FINDINGS.md` F-0xx, with the scene leg run, not owed)
  and R2007 step 14b's OBS browser-source check. Link both from the acceptance doc. A missing record
  fails acceptance; if C11 recorded the scene leg as owed, this run's rows 1, 3 and 7 discharge it,
  and the acceptance doc says so.
- (Amended 2026-10-18, Owed 20) The runbook has the sizing-alarm entry. If any row shows
  `rung != full`, it is a finding routed to C15's size rule (Owed 25), never a pass.
- (Amended 2026-10-18, Owed 18) The runbook has the hung-page entry. Row 8's health query shows no
  `graphics` restart without a crash, unless a row caused one.
- (Amended 2026-10-18, Owed 22) The runbook has the "CEF bump" entry.
- (Amended 2026-10-18, Owed 11) Row 12 passes: three crashes, each read after its restart from a
  real GET whose `events[]` still carries the exit, and the session fails `machine_crash` at the
  third. The GET bodies are recorded (redacted).
- (Amended 2026-10-18, Owed 21) Both G1 legs pass on stg, with R2005 and #974 confirmed on the stg
  web commit and the CSP in force. Without R2005 on stg, C17 is not accepted.
- (Amended 2026-10-20, C11b; Owed 66, 67, 69) The runbook has the exit-75 entry. One in-place restart on a stg
  Machine (the GPU process's zygote killed by pid through `fly machine exec`, as C14 case 13 does in the harness): the
  final beat's `exitDetail` names the restart, Fly restarts the Machine in place, the same session beats again on
  the same attempt, and the outage (the last programme packet to the first, from YouTube's ingest or the C13 reader)
  is recorded against C14's 15 s. Until Owed 66 lands, the restart is counted by row 12's crash cap: say so in the
  row, and never run row 12 and this row in one session (Owed 67).

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
