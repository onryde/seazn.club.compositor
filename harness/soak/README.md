# The soak (C15)

The 3 h soak from `docs/tasks/C15-soak.md` and spec §9.5. The real relay image runs on the
production guest (performance, 4 CPU, 8192 MB) as one Fly Machine, beside a harness Machine, in the
dedicated app `seazn-relay-soak` (org seazn-club, region `lhr`). One workflow job,
`.github/workflows/soak.yml`, drives it end to end.

```
gh workflow run soak.yml --ref <branch> -f minutes=30 -f sha=<40-hex head>   # the rehearsal
gh workflow run soak.yml --ref <branch> -f sha=<40-hex head>                 # the 3 h soak (185)
```

## The pieces

| File | What it is |
|---|---|
| `Dockerfile` | The harness Machine's image. The C13 harness image (fake-camera, fake-control, checker) plus MediaMTX (compose.yml's digest), iptables and the driver, under `tini -s`. |
| `mediamtx.yml` | `harness/mediamtx.yml` on one Machine. SRT `cam` in, RTMP `live/dest` out, and only `live/dest` recorded. |
| `driver.sh` | The harness Machine's main process. See its header and "The run" below. |
| `soak.json` | The soak's own numbers: schedule, waits, ratios and the workflow's margins. The domain's numbers stay in `harness/thresholds.json`. |
| `schedule.jq` | `soak_plan($soak; $drive_s)`: the dropout and disconnect times for a drive. It refuses a plan that cannot hold every action kind. |
| `verdict.jq`, `verdict.sh` | The verdict over one run's files. `soak.yml` and `test-verdict.sh` call the same command. |
| `flylog.py` | Turns the platform's log stream into the `docker logs --timestamps` shape that logfacts reads, plus `logscan.json` for check (b). |
| `test-verdict.sh` | Runs the verdict on `fixtures/green` (a 30-min miniature) and on one jq patch per red. Each case asserts its status, its exact failing ids, its reason and the check list. |
| `test-driver.sh` | Runs the driver under stubs on a 1-minute compressed schedule. It also covers the error path, which holds `/out` until the hard stop. |
| `test-workflow.sh` | Checks `soak.yml`'s shape: the gate and the plan step, run for real; `--restart no`; the job token; the `always()` destroy. It also runs `flylog.py` on a recorded-shape stream. |

`check.yml` runs the three tests.

## The run

1. **Plan job.** It skips with `::warning::FLY_API_TOKEN_SOAK unset — soak skipped` when the
   secret is empty. Otherwise it validates the inputs and makes the numbers:
   - drive = minutes − `deadline_margin_min`;
   - job timeout = minutes + `timeout_after_min` (40).

   GitHub's expressions have no arithmetic.
2. **Images job.** It builds the relay and harness images with chaos.yml's cache recipe, then the
   soak image. It pushes `registry.fly.io/seazn-relay-soak:relay-<sha>` and `:harness-<sha>`.
3. **Soak job.**
   1. It launches the harness Machine and waits until the driver's phase is `waiting_live` and the
      Machine's private name resolves.
   2. It launches the compositor Machine:
      - `APP_URL=http://<harness>.vm.seazn-relay-soak.internal:7000`;
      - `RELAY_DEADLINE_AT` = launch + minutes;
      - `--restart no` and `--rm` (a Machine that exits is destroyed).

      The job token (32 random bytes) is masked. It reaches the Machines only in their config
      files, which are mode 600 and deleted after `flyctl machine run`.
   3. It streams `flyctl logs --machine <compositor> --json` in the background.
   4. **The driver** runs MediaMTX, fake-camera and fake-control (on `[::]:7000`), then:
      - waits for a live beat and starts the score timeline;
      - drives the `schedule.jq` plan, appending each action to `actions.jsonl` with its wall time;
      - ends the session and waits for the stopped beat;
      - writes `/out/ended`.
   5. The job watches `/out/status.json` every minute (`flyctl machine exec`). At `awaiting_log`, it
      waits for the compositor Machine to exit and for the stream's lag. It converts the stream
      (`flylog.py`), uploads it to `/out/compositor.log` and touches `/out/log-ready`.
   6. The driver runs the checker and a secrets scan, then writes `/out/done`. It stays up until its
      hard stop (launch + minutes + `hard_stop_after_min`), so the Machine is bounded even if the
      job's cleanup never runs. An error writes `done {ok: false}` and phase `error`, and holds the
      same way.
   7. The job fetches the outputs (`flyctl ssh sftp get`) and runs a leak gate (the job token,
      plus the harness's own scan). It then runs `verdict.sh` and uploads `soak-<run>`.
   8. **`if: always()`:** it destroys every Machine in the app, then lists it, and fails unless the
      list is empty. The concurrency group `soak` allows one run at a time, so "every Machine" is
      this run's.

### The schedule

The brief's 3 h schedule, scaled by `schedule.jq` so that a short run still holds every action
kind:

- **Camera dropouts.** A dropout every `dropout_every_s` (300), at phase `dropout_phase_s` (150), for
  `dropout_pause_s` (10). Each one pauses fake-camera, then resumes it.
- **Dest disconnects.** One in the middle of each of the drive's `parts` (3), each lasting
  `disconnect_hold_s` (30). A disconnect drops inbound TCP to 1935, for IPv4 and for IPv6 (see the
  findings), and reads the DROP packet count before the rule is removed.

| Drive | Dropouts | Disconnects (s) |
|---|---|---|
| 180 min | 36 | 1800, 5400, 9000 |
| 25 min (the 30-min rehearsal) | 5 | 300, 900, 1200 |

## The verdict

`verdict.sh --dir <out> --page real|test --drive-min N` produces the checks below. Every number
comes from `thresholds.json`, `soak.json` or the drive.

| Check | What it requires |
|---|---|
| `inputs` | — |
| 1 memory | Part 3's median `memMb` within ±10 % of part 1's. |
| 2 | Every output outage overlaps a disconnect and ends within `budget_output_reconnect_ms` of it. `pts_gaps` is empty. |
| 3 | Exactly 1 session GET (reason "programme rebuilt"). |
| 4 | The final `droppedFrames` is under 0.1 % of drive × fps. |
| `5.beats`, `5.dropouts`, `5.disconnects` | The anti-vacuity counts, scaled to the drive. A disconnect needs dropped packets and an outage it caused. |
| 5a | `cef alive` lines. |
| 5b | Graphics never `failed`. Every `overlay hidden` names hang, crash or recycle, and none is bare. |
| 5c | Score deliveries per minute. |
| 5d | Overlay samples per minute. |
| 6 | Every beat's rung is `full`. A stepped beat fails with "sizing alarm: the guest is short of CPU (C17 runbook)". |

On `page=test`, checks 5a–5d read `pending: page source`, and the best status is `plumbing`, not
`pass`. The workflow then prints `::warning::PLUMBING ONLY`.

## The page source (open: owner ruling owed)

The driver takes the page as one input: `SOAK_PAGE=test|real` plus `SOAK_OVERLAY_URL`. On a real
page, fake-control hands the URL to the compositor unchanged. `soak.yml` reads the URL from the repo
secret `SOAK_OVERLAY_URL`, never from an input, because a relay-door URL carries a signed token.

**stg setup: none created.** No org, fixture or account exists yet, so there are no ids to record.
The stg route as approved does not work; see finding 1.

Not built yet, owed once the page source is ruled:

- the score driver that writes `deliveries.jsonl` (check 5c);
- the overlay sampler that writes `overlay-samples.jsonl` (check 5d).

On `page=real`, both checks fail while their files are missing, so a real-page run never passes on
nothing.

## Findings (the brief's premises that did not hold)

1. **A public stg page never goes ready under the compositor.** The overlay page passes
   `host=compositor` only behind relay access (`apps/web/src/app/overlay/fixtures/[fixtureId]/page.tsx:177`,
   `access && isCompositorHost(query.host)`). On the public door, the page emits neither
   `seazn:ready` nor `seazn:alive` (`useCompositorSignals`, `overlay-stage.tsx`). The compositor's
   readiness is the `seazn:ready` console line (`crates/adapters/src/cef_graphics.rs`).

   Relay access needs two things: a non-terminal composed `fixture_stream_sessions` row and a
   signed relay-page token (`server/overlay/relay-access.ts`). On stg, that means a real stream
   session, with infrastructure it would launch.
2. **fake-control cannot replay `score.json` every 20 min.** `/control/timeline-start` works once
   only and returns 409 after. The 2026-10-18 amendment replaced the index-drop criterion, so the
   soak needs no replay. The driver gives the test page one score timeline for the whole drive.
3. **`iptables -I INPUT …` alone drops nothing on Fly's private network**, because 6PN is IPv6. The
   driver adds the rule with `ip6tables` too, and records the dropped packets per family.
4. **The 5-minute dropouts and the minute-30 disconnect coincide.** The dropout phase is therefore
   150 s. `schedule.jq` refuses any plan in which they overlap.
5. **GitHub expressions cannot compute `minutes + 40`.** The plan job computes it instead.
6. **The brief's workflow never moves the compositor's log to the checker.** On a Machine, the log
   is the platform's stream. The job streams it, converts it (`flylog.py`) and uploads it.
7. **`image.yml` never pushes.** `soak.yml` builds and pushes both images itself.
8. **A `workflow_dispatch` workflow can be dispatched only once a file of that name exists on the
   default branch**, so the first rehearsal waits for `soak.yml` to reach `main`.

## Risks the rehearsal measures

- The checker's decode time on the recordings, which must fit inside `poll_after_min`. Read it from
  `driver.log`: `checking` to `done`.
- Whether the log stream is complete. Check 5a's ratio catches lost lines on a real page.
- Whether `flyctl ssh sftp` works with the soak token.
- Whether `--rootfs-size 30` is accepted and the disk holds 3 h of recordings.
