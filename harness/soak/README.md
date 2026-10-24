# The soak (C15)

The 3 h soak from `docs/tasks/C15-soak.md` and spec §9.5. The real relay image runs on the
production guest (performance, 4 CPU, 8192 MB) as one Fly Machine, beside a harness Machine, in the
dedicated app `seazn-relay-soak` (org seazn-club, region `lhr`). One workflow job,
`.github/workflows/soak.yml`, drives it end to end.

```
gh workflow run soak.yml --ref <branch> -f minutes=30 -f sha=<40-hex head>               # plumbing (test page)
gh workflow run soak.yml --ref <branch> -f minutes=30 -f sha=<40-hex head> -f page=real  # the rehearsal
gh workflow run soak.yml --ref <branch> -f sha=<40-hex head> -f page=real                # the 3 h soak (185)
```

## The pieces

| File | What it is |
|---|---|
| `Dockerfile` | The harness Machine's image. The C13 harness image (fake-camera, fake-control, checker) plus MediaMTX (compose.yml's digest), iptables and the driver, under `tini -s`. |
| `mediamtx.yml` | `harness/mediamtx.yml` on one Machine. SRT `cam` in, RTMP `live/dest` out. Both are recorded: `live/dest` under `/out/rec` (which the checker reads) and `cam` under `/out/camrec` (the sampler's reference). |
| `driver.sh` | The harness Machine's main process. See its header and "The run" below. |
| `soak.json` | The soak's own numbers: schedule, waits, ratios and the workflow's margins. The domain's numbers stay in `harness/thresholds.json`. |
| `schedule.jq` | `soak_plan($soak; $drive_s)`: the dropout and disconnect times for a drive. It refuses a plan that cannot hold every action kind. |
| `verdict.jq`, `verdict.sh` | The verdict over one run's files. `soak.yml` and `test-verdict.sh` call the same command. |
| `sampler.py` | Check (d): once a minute, compares the programme with the camera-only recording over the camera's still bands (see "The overlay sampler"). |
| `flylog.py` | Turns the platform's log stream into the `docker logs --timestamps` shape that logfacts reads, plus `logscan.json` for check (b). |
| `test-verdict.sh` | Runs the verdict on `fixtures/green` (a 30-min miniature) and on one jq patch per red. Each case asserts its status, its exact failing ids, its reason and the check list. |
| `test-driver.sh` | Runs the driver under stubs on a 1-minute compressed schedule. It also covers the error path, which holds `/out` until the hard stop. |
| `test-sampler.sh` | Runs `sampler.py` on synthetic recordings made with ffmpeg: a moving square and a noise strip, with an overlay bar that goes off air for one minute, a disconnect, and a missing segment. It also covers an all-noise camera and a run with no start. |
| `test-workflow.sh` | Checks `soak.yml`'s shape: the gate and the plan step, run for real; `--restart no`; the job token; the `always()` destroy; the page source's job. It also runs `flylog.py` on a recorded-shape stream. |
| `web.Dockerfile` | page=real's harness image: the soak image plus a standalone seazn.club build, node, and Postgres 16 with the migrated database restored at build time. See "The page source". |
| `web/up.sh` | page=real's page source on the Machine: Postgres, the web server, the seed, the stream session, the signed URL, a pre-flight. |
| `web/serve.sh` | Starts the web server with an allowlisted environment and a per-run signing key. |
| `web/seed.mjs`, `web/score.mjs` | The seed and the score driver (check (c)), through the app's own API. `web/lib.mjs` is their HTTP door and the relay token. |
| `web/session.sql`, `web/overlay-url.mjs` | The composed stream session the relay door checks, and the door's signed URL. |
| `web/bundle-check.sh` | Fails when the web build carries a PostHog key, a Sentry DSN or a Stripe key. |
| `web/smoke.sh` | `up.sh` and three deliveries inside the built image, on the runner, before the push. |
| `web/test-bundle-check.sh`, `web/test-overlay-url.sh` | The bundle check on synthetic builds; the signed URL against an independent HS256 verifier. |

`check.yml` runs the six tests.

## The overlay sampler (check (d))

After the checker, the driver runs `sampler.py` on the run's recordings. For each minute it picks
one instant clear of every dropout and disconnect, and decodes three frames: the programme at that
instant, the camera, and the camera `control_gap_s` later. Each frame is cut into bands of 40 rows.

- **A band is stable** when the two camera frames agree in it. On `ball`, that is every band except
  the ball's and the strip's. On a stable band, the camera's frame is the reference whatever the
  programme's latency.
- **`region_diff`** is the largest programme-versus-camera difference over the stable bands. The
  minute is on air when it is at least `overlay_diff_min`.
- **No stable band, no reference.** On `noise` no band is stable. Every sample then carries an
  error, and check (d) fails rather than passes.

`overlay_diff_min` (4) and `stable_band_max` (2) are first guesses. The rehearsals' measured
`region_diff` values calibrate them.

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

## The page source (page=real)

The production overlay page comes from a **standalone seazn.club build on the harness Machine**,
with its own Postgres. Nothing runs against stg or prod, no stg or prod secret is used anywhere, and
nothing is created on stg. The web commit is `soak.yml`'s `web_sha` input. Its default is
`7dcb3c42e23eb44ef69d7c564d4e9e0c5970d569` (onryde/seazn.club main, 2026-10-24), which carries
R2005 (#982, merge `e0608604c5`) and R2007 PR A (#974, `TemplateId = "bar" | "bug"`).

**Built.** The `web` job (page=real only) builds it on the runner:

1. `pnpm install`, then `db:apply` and `sync:sports` against a scratch `postgres:16` service.
2. A dump of the roles and the database, before anything is seeded.
3. `SKIP_TYPECHECK=1 npm run build`. The step fails if any PostHog, Sentry or Stripe setting is in
   its environment, and none is set, so a local server has nothing to send to live PostHog.
4. `web/bundle-check.sh` over the staged `.next/static` and `.next/server`. It fails on any PostHog
   project key, Sentry DSN or Stripe key.

The images job layers the result onto the soak image (`web.Dockerfile`). Postgres is restored at
build time, so a dump that does not restore fails the build. It then runs `web/smoke.sh` inside the
image before it pushes it as `harness-web-<sha>`. The smoke is `up.sh` exactly as the Machine runs
it, plus three deliveries.

**On the Machine.** `driver.sh` runs `web/up.sh` before fake-control:

1. **Postgres.** It listens on 127.0.0.1 only, with trust auth.
2. **The web server.** `serve.sh` starts it with an allowlisted environment: `DATABASE_URL`,
   `DATABASE_SSL=disable`, `AUTH_SECRET`, `AUTH_DEV_LINKS=1`, `NODE_ENV=production`, `PORT`,
   `HOSTNAME=::`, `NEXT_TELEMETRY_DISABLED=1`, `LOG_LEVEL`, `PATH`, `HOME`.
   - `AUTH_SECRET` is made per run. It sits in a mode-600 file under `/run/soak-web`, never in argv
     or `/out`.
   - The server's `/proc/<pid>/environ` is checked against the allowlist, by name.
   - The job token, the SRT passphrase and every telemetry setting stay out.
3. **The seed.** `seed.mjs` signs in with a dev magic link as `delivered+soak-<sid8>@resend.dev`, an
   e2e address that is never mailed because Resend is unset. It creates a public competition and a
   cricket ODI fixture (two teams of six, both lineups set, started). It also creates a badminton
   fixture for the cost legs.
4. **The stream session.** `session.sql` inserts a composed `fixture_stream_sessions` row, id
   `SESSION_ID`, state live, on the cricket fixture, with a placeholder stream target that is never
   used. This row and the signed token are what the relay door checks.
5. **The signed URL.** `overlay-url.mjs` builds
   `http://<harness>.vm.seazn-relay-soak.internal:3000/overlay/fixtures/<fx>?sid&st&host=compositor&delay=3100&template=bar`.
   `st` is web's relay-page token, signed with the run's key and valid until 30 min past the hard
   stop. The URL, the session file fake-control serves (`pageToken`, `fixtureId`) and the cookies
   all stay under `/run/soak-web`.
6. **The pre-flight.** The URL must answer 200 from the Machine.

During the drive, `score.mjs` records a delivery every `score_every_s` (30 s): two a minute against
check (c)'s one. Runs are even, with boundaries among them. A refused delivery re-reads the position
from the ledger and tries again at once.

ODI rather than T20: T20's 240 balls would end the match at minute 120 of a 180-minute drive, while
ODI's first innings alone is 300 balls. The secrets scan adds the signing key, the page token and
the session cookies to `/out`'s list. The fetch takes `deliveries.jsonl`, `score.log`, `seed.json`,
`web-up.log`, `web.log` and `postgres.log`.

**Harness Machine size, page=real: performance-8x, 16384 MB** (soak.json `harness_guest.real`).
page=test stays at performance-4x, 8192 MB. The compositor Machine is alone on the production guest
(performance-4x, 8192 MB) either way.

**Verified locally (2026-10-24)** against a standalone build of `7dcb3c42e2`, with no `.env.local`:

- **Bundle.** `bundle-check`: 6987 files, clean. The same check on a build that carries the dev
  `.env.local` reds on its PostHog key, so the check is not vacuous on the real key's shape.
- **Server.** It started under `serve.sh` with exactly the allowlist's names.
- **Seed and page.**
  - The seed went through.
  - The page under the signed URL logged `seazn:phase-channel=web`, `seazn:ready` and
    `seazn:alive`, every 2 s.
  - It polled `/api/v1/public/fixtures/<id>/overlay` with 200s.
  - Its only off-origin request was the compositor's own phase channel, `127.0.0.1:7419`.
- **Negatives.** A token signed with another key gave no `seazn:ready`, and so did a valid token for
  a sid with no session row.
- **Dump and restore.** The roles plus a plain-SQL dump, restored into a fresh cluster, served the
  same page to `seazn:ready`.
- **Scoring.**
  - `score.mjs`: 320 of 320 deliveries.
  - The engine closed innings 1 itself at 300 legal balls, and innings 2 carried on.
  - A delivery made by a second writer mid-run gave one `409 SEQ_CONFLICT`. The driver re-read the
    ledger and its next delivery landed on the right ball.

## Findings (the brief's premises that did not hold)

1. **A public page never goes ready under the compositor, on stg or anywhere.** The overlay page passes
   `host=compositor` only behind relay access (`apps/web/src/app/overlay/fixtures/[fixtureId]/page.tsx:177`,
   `access && isCompositorHost(query.host)`). On the public door, the page emits neither
   `seazn:ready` nor `seazn:alive` (`useCompositorSignals`, `overlay-stage.tsx`). The compositor's
   readiness is the `seazn:ready` console line (`crates/adapters/src/cef_graphics.rs`).

   Relay access needs two things: a non-terminal composed `fixture_stream_sessions` row and a
   signed relay-page token (`server/overlay/relay-access.ts`). On stg, that means a real stream
   session, with infrastructure it would launch. Hence the standalone page source (above), where
   the session row is seeded in the build's own database. Verified there: no `seazn:ready` without
   the row, or with a token signed by another key.
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
- page=real: whether the compositor's CEF loads the page over plain HTTP from the harness's private
  name, and the page goes `seazn:ready` there. Locally it did, in Chromium, from `localhost`.
- page=real: the web job's time (install, migrate, build: about 15 min, beside the CEF build), and
  whether the harness at performance-8x carries fake-camera's encode with the web server and
  Postgres beside it. Read `web.log`, `postgres.log` and the harness's CPU from the run.
