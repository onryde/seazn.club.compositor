# C17: Stg end-to-end acceptance (owner-watched) and the runbook

**Repos:** both. **Depends on:** everything (C14 3× green, C15 green, C16 stg promoted, W01–W06
merged and deployed to stg).

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
- the **post-match health review**: run `seazn.club` `docs/runbooks/relay-health-review.sql` (W02)
  against the session id
- the **rollback**: `promote.yml` with the previous ref (C16)
- **manual stop**: end the session in the organiser UI. **Never** `flyctl machine destroy` a live
  session's Machine, because the sweep would book an `r<n+1>`.

## Steps

- [ ] **Step 1: Preflight** (orchestrator, no owner needed):
  1. Stg web is on a commit containing W01–W06.
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
  | 3 | Phone: background the capture app for 20 s | YouTube freezes ≤ 0.5 s, then slate by 3 s; panel shows camera reconnecting | | |
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

- All 8 rows have the owner's **pass** verdict, recorded verbatim.
- The runbook covers every signal in the table.
- Both pointer docs are updated.

## Do not touch

Prod.

## Output cap

The filled table, plus PR URLs.
