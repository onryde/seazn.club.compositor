# The soak (C15)

The 3 h soak from `docs/tasks/C15-soak.md` and spec §9.5. The real relay image runs on the
production guest (performance, 6 CPU, 12288 MB) as one Fly Machine, beside a harness Machine, in the
dedicated app `seazn-relay-soak` (org seazn-club, region `lhr`). One workflow job,
`.github/workflows/soak.yml`, drives it end to end.

```
gh workflow run soak.yml --ref <branch> -f minutes=30 -f sha=<40-hex head>               # plumbing (test page)
gh workflow run soak.yml --ref <branch> -f minutes=30 -f sha=<40-hex head> -f page=real  # the rehearsal
gh workflow run soak.yml --ref <branch> -f sha=<40-hex head> -f page=real                # the 3 h soak (185)
gh workflow run soak.yml --ref <branch> -f sha=<40-hex head> -f page=real -f minutes=15 \
  -f legs=A:snow,A:noise,C:snow,C:noise                                                  # the cost legs' pictures
gh workflow run soak.yml --ref <branch> -f sha=<40-hex head> -f page=real -f minutes=15 \
  -f legs=B:<720p's heavier>,D:<1080p's heavier>                                         # the rest (see "The cost legs")
```

`-f guest=performance-8x` puts the compositor Machine on the next size (a soak or a leg).

## The pieces

| File | What it is |
|---|---|
| `Dockerfile` | The harness Machine's image. The C13 harness image (fake-camera, fake-control, checker) plus MediaMTX (compose.yml's digest), iptables and the driver, under `tini -s`. |
| `mediamtx.yml` | `harness/mediamtx.yml` on one Machine. SRT `cam` in, RTMP `live/dest` out. Both are recorded: `live/dest` under `/out/rec` (which the checker reads) and `cam` under `/out/camrec` (the sampler's reference). |
| `driver.sh` | The harness Machine's main process. See its header and "The run" below. |
| `soak.json` | The soak's own numbers: schedule, waits, ratios and the workflow's margins. The domain's numbers stay in `harness/thresholds.json`. |
| `schedule.jq` | `soak_plan($soak; $drive_s)`: the dropout and disconnect times for a drive. It refuses a plan that cannot hold every action kind. `schedule_plan` picks it, the memory experiment's `dropout_plan`, or `none` (its control) by name. |
| `verdict.jq`, `verdict.sh` | The verdict over one run's files. `soak.yml` and `test-verdict.sh` call the same command. |
| `sampler.py` | Check (d): once a minute, compares the programme with the camera-only recording over the camera's still bands (see "The overlay sampler"). |
| `flylog.py` | Turns the platform's log stream into the `docker logs --timestamps` shape that logfacts reads, plus `logscan.json` for check (b). |
| `test-verdict.sh` | Runs the verdict on `fixtures/green` (a 30-min miniature; `green-dropouts` and `green-none` are the same run on the experiment's schedules) and on one jq patch per red. Each case asserts its status, its exact failing ids, its reason and the check list. |
| `test-driver.sh` | Runs the driver under stubs on a 1-minute compressed schedule. It also covers the error path, which holds `/out` until the hard stop, the secrets scan's jars, and cost legs B and C. |
| `test-sampler.sh` | Runs `sampler.py` on synthetic recordings made with ffmpeg: a moving square and a noise strip, with an overlay bar that goes off air for one minute, a disconnect, and a missing segment. It also covers an all-noise camera and a run with no start. |
| `test-workflow.sh` | Checks `soak.yml`'s shape: the gate and the plan step, run for real; `--restart no`; the job token; the `always()` destroy; the page source's job. It also runs `flylog.py` on a recorded-shape stream. |
| `web.Dockerfile` | page=real's harness image: the soak image plus a standalone seazn.club build, node, and Postgres 16 with the migrated database restored at build time. See "The page source". |
| `web/up.sh` | page=real's page source on the Machine: Postgres, the web server, the seed, the stream session, the signed URL, a pre-flight. |
| `web/serve.sh` | Starts the web server with an allowlisted environment and a per-run signing key. |
| `web/seed.mjs`, `web/score.mjs` | The seed and the score driver (check (c)), through the app's own API. `web/lib.mjs` is their HTTP door, the relay token and the page feed's moments. `score.mjs --boundaries` is the cricket legs' driver. |
| `web/rally.mjs` | The badminton legs' driver: a rally every 3 s, each game kept level (see "The cost legs"). |
| `web/session.sql`, `web/overlay-url.mjs` | The composed stream session the relay door checks, and the door's signed URL. |
| `web/bundle-check.sh` | Fails when the web build carries a PostHog key, a Sentry DSN or a Stripe key. |
| `web/smoke.sh` | `up.sh` and three deliveries inside the built image, on the runner, before the push. |
| `web/test-bundle-check.sh`, `web/test-overlay-url.sh` | The bundle check on synthetic builds; the signed URL against an independent HS256 verifier. |
| `cost/in-compositor.sh` | On the compositor Machine (bash builtins only: the relay image has no procps or python). `sample <max_s>`: a JSON line a second from `/proc` (the Machine's cores, the process tree's by kind, the busiest compositor thread, memory). `kill-renderer`: SIGKILL to the CEF renderer with the highest `--renderer-client-id` (the newest page's, as C14's), once a Machine: a second call repeats the first's lines. `kill-renderer <not_before_ms>` never kills before that instant by the Machine's own clock (it waits, at most 10 s; a call more than 10 s early is refused). |
| `cost/leg.sh` | On the runner, in the background: puts `in-compositor.sh` on the compositor Machine, keeps its sampler running over `flyctl ssh console`, and kills the renderer at the drive's midpoint over `flyctl machine exec`. |
| `cost/verdict.sh`, `cost/cost.jq` | A leg's verdict (the size rule, the moments, the reload, the samples) and its F-005 column. |
| `cost/table.sh` | The legs' columns as one F-005-shaped table (the `cost-table` job). |
| `cost/test-cost.sh`, `cost/fixture.py` | The sampler on fake `/proc` trees worked by hand, the kill on the test's own processes, `leg.sh` on a stub flyctl, the verdict on a green leg and one mutation per red, the table. |

`check.yml` runs the seven tests.

## The overlay sampler (check (d))

After the checker, the driver runs `sampler.py` on the run's recordings. For each minute it picks
one instant clear of every dropout and disconnect, and decodes three frames: the programme at that
instant, the camera, and the camera `control_gap_s` later. Each frame is cut into bands of 40 rows.

- **A band is stable** when the two camera frames agree in it. On `ball`, that is every band except
  the ball's and the strip's. On a stable band, the camera's frame is the reference whatever the
  programme's latency.
- **`region_diff`** is the largest programme-versus-camera difference over the stable bands. The
  minute is on air when it is at least `overlay_diff_min`, and the floor is not.
- **`median_diff`** is the floor: the median of the same differences. An overlay over part of the
  picture leaves it near zero. A floor at or over `overlay_diff_min` is a level shift or a
  misalignment between the recordings, not an overlay: the sample carries an error and is off air
  (`test-sampler.sh`: the programme 8 luma brighter, no bar).
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
      - `RELAY_DEADLINE_AT` = the compositor's own launch + minutes, after the harness's boot and
        `up.sh` (the harness's `SOAK_DEADLINE_MS` is the launch step's start + minutes, a lower
        bound, so its WARN is conservative);
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
   6. The driver runs the checker, the sampler and a secrets scan, then writes `/out/done`. It stays
      up until its hard stop (launch + minutes + `hard_stop_after_min`), so the Machine is bounded
      even if the job's cleanup never runs. The checker and the sampler each run only until the hard
      stop less `post_check_reserve_s` (180 s, the job's fetch): a hung one is stopped and reads exit
      124, and one with no time left is skipped with 124 (`test-driver.sh` run 5). An error writes
      `done {ok: false}` and phase `error`, runs the same secrets scan over what was made, and holds
      the same way.
   7. The job watches until `/out/done` or minutes + `poll_after_min` (20). It fetches the outputs
      (`flyctl ssh sftp get`) and runs a leak gate: the job token, plus the harness's own scan. A
      leak uploads nothing and prints the harness's redacted report: the kind, the file and line,
      and the line with every credential replaced by its name. With no harness scan at all, page=real
      keeps only `status.json` and `done`. It then runs `verdict.sh` and uploads `soak-<run>`.
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

`schedule=dropouts` (soak.yml; `SOAK_SCHEDULE` on the harness) runs the memory experiment's schedule
instead (`dropout_plan`): a dropout every `dropout_only.every_s` (125: 28.8 an hour, against the
soak's 12) from `dropout_only.phase_s` (60), each `dropout_pause_s` long, and no disconnect. Each
dropout owns a whole period. The period is deliberately not a whole number of minutes: the memory
split's lines come a minute apart on the compositor's beat grid, so a 120 s period would hold every
dropout at the same place between two lines for the whole run, and either every gap between two
dropouts would hold two lines (a control) or none would. At 125 s the lines walk 5 s a period.

`schedule=none` is its control: the same drive with nothing in it, so growth over time alone can be
told from growth per dropout. The verdict measures the same steps at the instants `dropout_plan`
would have had. A cost leg runs no schedule: `dropouts` or `none` with a leg is refused.

| Drive | Dropouts | Disconnects |
|---|---|---|
| 75 min (`minutes=80`, the experiment) | 35, at 60 to 4310 s | none |
| 25 min (`minutes=30`) | 11, at 60 to 1310 s | none |
| 180 min | 85 | none |

## The verdict

`verdict.sh --dir <out> --page real|test --drive-min N --memory-mb MB [--schedule soak|dropouts|none]`
produces the checks below. `--memory-mb` is the compositor Machine's memory (`soak.yml` passes its
`C_MEM`, from `soak.json`'s `compositor_guest`); it is required, whole MB above 0. Every number comes
from `thresholds.json`, `soak.json`, the drive or that memory.

| Check | What it requires |
|---|---|
| `inputs` | — |
| 1 memory | Part 3's median `memMb` within ±40 % of part 1's (`memory_drift_max`; owner, 2026-10-25: "Up to 20% grew is fine", then "Machine is solely for relay so 40% is fine"; it was ±10 %). A drift exactly at the bound passes. |
| 1.projection | The drive's `memMb` fitted against time by least squares (hours since the start action), projected to `memory_projection.session_cap_min` (300: a livestream's 5 h cap), is within `memory_budget_fraction` (half) of the smaller of the Machine's memory and the production guest's (`memory_projection.production_guest`, web's `RUNNER_DEFAULT_GUEST`), so a soak on a bigger guest never loosens the budget (owner ruling A, 2026-10-25; the guest, the coordinator's decision the same day). The projection is judged as reported, to 0.1 MB: exactly at the budget passes. It reports the slope in MB/h, the intercept, the projection, the budget, both memories and the beats fitted. Fewer memMb beats than `5.beats`'s floor, none, no spread in time, or a `production_guest` that `compositor_guest` lacks fail. |
| 1.split | Schedules `dropouts` and `none` only (the memory experiment; the soak's page=real log may be withheld). The compositor log's `memory split` lines in the drive carry the compositor's RSS and a CEF poll; the poll advances from line to line; a CEF child's PSS was read; no kind was listed with none of its children read; and at least one instant's step was measured from the lines and one from the beats' windows. Zero read fails. |
| 2 | Every output outage overlaps a disconnect and ends within `budget_output_reconnect_ms` of it. `pts_gaps` is empty. |
| 3 | Exactly 1 session GET (reason "programme rebuilt"). |
| 4 | The final `droppedFrames` is under 0.1 % of drive × fps. |
| `5.beats`, `5.dropouts`, `5.disconnects` | The anti-vacuity counts, scaled to the drive. A disconnect needs dropped packets and an outage it caused. |
| 5a | `cef alive` lines. |
| 5b | Graphics never `failed`. Every `overlay hidden` names hang, crash or recycle, and none is bare. |
| 5c | Score deliveries per minute, and the page received them: drive beats carry the page's `scoreStale` (its `seazn:score-stale` signal), and none reads true. |
| 5d | An on-air sample per `sample_every_s` slot (the sampler's own unit). |
| 5e | No blank-page reload (`cef reload ... reason=no_frames`, Owed 99), and each page's first ready, per process and generation, has its `cef painted` at or after it in the same process; at least one page checked. Judged on both pages (the PR #92 review's I-1: page=real is the one run on the production page, which paints only on change). A report with no blank-reload facts fails. |
| 6 | Every beat's rung is `full`. A stepped beat fails with "sizing alarm: the guest is short of CPU (C17 runbook)". |

`scoreStale` is the page's feed health: its last live-data poll succeeded within its stale budget.
It does not say the payload advanced. A page polling an answer that never changes reads false all
the same. What shows the score moving is (c)'s deliveries landing (the ledger's seq advancing) and
(d)'s overlay on air; a witness that the painted score follows the ledger is owed (below).

`inputs` also fails when the driver's start action names another schedule than `--schedule` (a
start action without one is the soak's). `5.dropouts` and `5.disconnects` each name an action the
plan does not have (the dropout schedule has no disconnect; none has neither).

### The memory split (soak 40139563554's drift, 2026-10-25)

The 3 h soak 40139563554 failed only check 1 (hour 1's median 994 MB, hour 3's 1220 MB). `memMb` is
the sum of VmRSS over the whole process tree, so it cannot tell the compositor from CEF. The
compositor now logs, once a minute (the first beat with a sample, then the first beat a minute after
the last line), one line beside the beat's unchanged `memMb`:

    memory split input_gen=<g> tree_mb=<n> rss_kb=<n> anon_kb=<n> file_kb=<n> shmem_kb=<n> threads=<n>
      cef_poll=<n> cef_renderer_kb=<n> cef_renderer_n=<n> cef_renderer_listed=<n> cef_gpu_process_kb=…
      cef_utility_kb=… cef_zygote_kb=… cef_other_kb=… (each with its _n and _listed)

The compositor's own fields come from its `/proc/self/status`, read in the beat's existing tree walk.
CEF's are the PSS the CEF child poll already reads for its memory cap (`smaps_rollup`), bucketed by
`--type=`. Per kind, `listed` is how many children of it the poll found, `n` how many of those it
read the PSS of, and `kb` that PSS's sum. `kb` is `-` when it read none: a kind listed and unread is
unknown, never 0. A kind the poll found no child of prints `kb=- n=0 listed=0`. `input_gen` is the
last input generation rebuilt, so a step can be tied to a rebuild. Any absent value prints `-`.

The verdict's `measures.memory_split` reads those lines and the beats' `memMb`, and `verdict.sh`
prints them as `MEASURE` lines under the checks (in the step summary too). They are evidence, not a
gate:

- **Compositor only** and **CEF only**: part 3's median against part 1's, as check 1 reads `memMb`.
  That is RSS, anon, file, shmem and threads for the compositor, and the PSS total and each kind for
  CEF. A kind listed and unread is left out of its median, never counted as 0. CEF's children by
  RSS (`tree_mb` less the compositor's RSS) needs no PSS read, and cross-checks it.
- **Per instant, from the lines**: an instant is a dropout that was done (`ok`), or on
  `schedule=none` each instant `dropout_plan` would have had. Its step is the first line at least
  `memory_step.after_s[0]` (30 s) after the instant, less the last line at most
  `memory_step.before_s[1]` (10 s) before it. Each line sits inside the gap between that instant and
  its neighbouring faults, and a gap with no line leaves the instant unmeasured. This gives the RSS,
  anon, file, CEF and children's steps, each step's span, and the input generations either side. The
  summary gives the mean, median and sum, the first half's mean against the second half's, and the
  rebuilds. A failed dropout bounds its neighbours and is not a step.
- **The control, from the lines**: in each gap between two instants that holds two distinct lines,
  the later line less the earlier. Pooled into a rate, it gives the growth the steps' own mean span
  would show with no dropout at all, and the steps' mean less that is the **excess**. Steady growth
  with no dropout effect reads an excess of about 0. A run whose gaps never hold two lines says "NO
  CONTROL".
- **The windowed step, from the beats** (the diagnosis's `epoch.py`): the median `memMb` over
  `memory_step.after_s` ([30, 110) s after the instant) less the median over `before_s` ([-90, -10)
  s), so this run compares like for like with soak 40139563554's 9.3 MB a dropout. On
  `schedule=dropouts` these windows meet the next dropout's, so the windowed step's control is a
  `schedule=none` run of the same length: the same windows at the same instants, where nothing
  happened.

The memory line's `ts=` is the compositor Machine's clock; the actions' and the beats' times are the
harness Machine's. The skew between them is not measured: the line windows' margins (10 s, 30 s)
assume it is far smaller.

The owner ruled on check 1 on 2026-10-25 (`docs/_INDEX.md`): the drift bound is 40 %, and check
1.projection bounds where the growth would reach at the 5 h session cap. A per-dropout bound (the
other half of ruling A) is owed after memory arm 3, the no-dropout control.

### The memory experiment's knobs

`soak.yml` takes `malloc_arena_max` (a positive integer up to 9999, or empty for unset). The plan
job's knobs step turns it into the compositor Machine's extra environment; a names guard admits
`MALLOC_ARENA_MAX` alone, and the compositor's own keys win a clash. The run's name carries it, for
the soak and for the cost legs. The entrypoint runs the compositor (`supervise`) without clearing
its environment, and CEF's children inherit it, so `MALLOC_ARENA_MAX` reaches them as well as the
compositor. Reaching is not affecting: if Chromium's children allocate through PartitionAlloc
rather than glibc's malloc, the knob changes nothing there, so a flat CEF line under the knob is no
evidence either way (unverified).

There is no GStreamer tracer knob. The leaks tracer reports only at `gst_deinit`, or on a SIGUSR1/2
checkpoint when `GST_LEAKS_TRACER_SIG` is set. The compositor never calls `gst_deinit` (it leaves by
`hard_exit`, F-017), and SIGUSR1/2 are its stop signals (F-016). So a tracer set from the workflow
would add its per-object bookkeeping, on the very allocator this experiment measures, and print
nothing: a silent run that reads as "no leak". PR #82's review I4 took it out; it comes back with
the compositor-side checkpoint (owed below).

On `page=test`, checks 5a–5d read `pending: page source`, and the best status is `plumbing`, not
`pass`. The workflow then prints `::warning::PLUMBING ONLY`, heads the step summary "PLUMBING ONLY —
NOT A SOAK PASS", and the run list names the run `soak page=test <N> min (PLUMBING ONLY)`.

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
4. `web/bundle-check.sh` over what `app.tgz` packs except the third-party `node_modules` (into which
   the build inlines nothing): `server.js` with its inlined config, `.next`, `public` and the rest of
   `apps/web`, and the database dump. It fails on any PostHog project key, Sentry DSN or Stripe key.
   On rehearsal 40131252539's artifact: 7149 files, clean.

The images job layers the result onto the soak image (`web.Dockerfile`). Postgres is restored at
build time, so a dump that does not restore fails the build. `ldd` runs over the node binary and
every native add-on (`*.node`: sharp's, at `7dcb3c42e2`), and any `not found` fails the build; no
add-on at all fails it too. It then runs `web/smoke.sh` inside the
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
6. **The pre-flight.** From the Machine, as the compositor will ask: the page, one `/_next/static`
   script its HTML names, and the overlay poll the page makes (with `sid` and `st`) must each
   answer 200. A page whose chunks were never emitted, or whose live-data route fails, fails here.

During the drive, `score.mjs` records a delivery every `score_every_s` (30 s): two a minute against
check (c)'s one. Runs are even, with boundaries among them. A refused delivery re-reads the position
from the ledger and tries again at once.

ODI rather than T20: T20's 240 balls would end the match at minute 120 of a 180-minute drive, while
ODI's first innings alone is 300 balls. The secrets scan adds the signing key, the page token and
every cookie in the jar to `/out`'s list, except the inert ones named in `soak.json`
`web_cookies_inert` (`seazn_org`, an id, and `seazn_signed_in`, whose value is `1`: no authority),
which the log names as not scanned. A cookie the web adds later is scanned, so the scan fails closed.
A value under `scan_min_chars` (16) cannot be scanned without matching everything, so it fails by
name ("too short to scan") and is never skipped. `secrets.json` names the compositor log it scanned
by its sha256, and the leak gate uploads the job's own copy on page=real only when it is that file. The fetch takes `deliveries.jsonl`, `score.log`, `seed.json`,
`web-up.log`, `web.log` and `postgres.log`.

**Harness Machine size, page=real: performance-8x, 16384 MB** (soak.json `harness_guest.real`).
page=test stays at performance-4x, 8192 MB. The compositor Machine is alone on the production guest
(performance-6x, 12288 MB) either way.

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

## The cost legs

C15's legs A to D (`soak.json` `legs`): 720p and 1080p, `bar` on cricket and `bug` on badminton,
each on the production page from the standalone page source, on the compositor guest `guest` names.
`legs` turns the soak job into a matrix of `leg:camera` runs, one at a time (`max-parallel: 1`, and
`fail-fast: false` so one red leg does not cancel the rest). Each run is a whole soak job: its own
two Machines, named `…-<leg>-<camera>`, its own destroy step and its own artifact. A leg needs
page=real, a drive of at least `leg_min_drive_min` (`minutes=15` is 10 minutes), and a camera of
`leg_cameras` per leg (`A:snow`) or from `camera`.

**What a leg drives.** No dropout and no disconnect. The score driver makes a moment at the page's
pace: cricket a boundary every `cost_ball_every_s` (6 s; fours and sixes alternate, each a doubled
moment of about 5 s, so the bar's cards run nearly back to back, and the compositor's 5 s poll sees
each), badminton a rally every `cost_rally_every_s` (3 s; the game kept level, so a fresh game
point every second rally from 19-19). `leg.sh` kills the newest page's CEF renderer once at the
drive's midpoint; the compositor reads CefCrashed and rebuilds the page as a new generation. The
midpoint is decided on the compositor Machine, by the clock the kill's `at_ms` is read from: `leg.sh`
sends the try 2 s early as `kill-renderer <planned_ms>`, and the Machine holds it until its own clock
reaches `planned_ms` (finding 18). A try whose reply was lost is retried, but `kill-renderer` claims
the kill before it waits and keeps a mark on the Machine, so a retry gets the first kill's lines back
and kills nothing, even one that lands while the first call still waits; and no try starts
unless its kill can land inside the verdict's window (`cost_kill_late_max_s`, 100 s: three tries of
30 s).

**The verdict** (`cost/verdict.sh`, every number from `soak.json`):

| Check | What it requires |
|---|---|
| `run` | One start and one end; the drive at least `leg_min_drive_min`. |
| `samples` | From +`cost_sample_from_s` (15 s) to the end, at least `cost_samples_min_ratio` of a sample a second, each reading the guest's CPU count (so a leg on the wrong size fails). |
| `cpu` | The Machine's CPU p95 (nearest rank) at most `cost_cpu_p95_max_share` (75 %: owner ruling 2026-10-27, F-033 (PR #102); it was Owed 20's 50 %) of the guest's cores. |
| `rung` | Every beat's rung is `full`, the reload included. |
| `drops` | The final `droppedFrames` is 0. |
| `moments` | At least `cost_moments_min_per_min` (1) moment a minute over the drive (a mean; see finding 15), by the page's own feed. A feed that answered for none of the events fails. |
| `reload` | Exactly one kill, which found a renderer and wrote a status of 0, from the midpoint to `cost_kill_late_max_s` after it, inside the drive. Then the compositor's own word, from `compositor.log`: exactly one `cef crashed gen=N` in the drive, after the kill, and a `cef ready gen=N+1` after it (a new build; a held page's in-place reload says ready at its old gen). No log is a red, never a pass. The beats, every 5 s, seldom see a reload, so none is read for it. |
| `stills` | At least one programme still written (stills.tar against stills.log's instants), so there is something to look at. The count is in the column. |

The step summary gets the leg's F-005 column: the Machine's cores avg / p95 / max, the tree's and
each kind's (compositor, renderer + gpu + utility + zygote, Xvfb), the busiest compositor thread,
encoder speed, fps, the final drops, the rungs, the moments a minute by kind and the longest
stretch without one, the kill and the compositor's crash and ready after it, the live renderers
before and after the kill (more than one after is a page painting beside the live one: the Owed
82/93 shape, reported and never judged), the stills, memory, and the Owed 93 projection. The
sampler and its ssh session are inside the Machine's p95 (the column's "sampler itself / the
rest"), so the figure over-reports a little; the F-entry says so. The `cost-table` job joins every leg's column
into one table, each leg:camera's latest attempt (a re-run of a failed leg leaves both artifacts),
and fails naming any run asked for that has no verdict. Each leg's artifact keeps `cost.json`, the raw samples, the kill's lines and
`stills.tar`: eight programme stills (six a second apart at a quarter of the drive, one 45 s after
the kill, one 30 s before the end), to look at and never commit.

**The dispatch.** The 2026-10-23 amendment asks for each pattern once per profile on the
production guest, then the heavier one. So: `A:snow,A:noise,C:snow,C:noise` first, then B on 720p's
heavier pattern and D on 1080p's. If B or D measures above A or C, the brief's sport hypothesis
failed and a football leg is owed before a size is chosen; that leg cannot run as built (no
football driver, and `momentsOf` has no football rule), so it would fail closed on its moments,
never pass. A leg over the size rule on performance-6x reruns with `-f guest=performance-8x`.
GitHub keeps one pending run per concurrency group (`soak`): a dispatch queued behind a running
one replaces any run already pending, silently. Dispatch the next only after the previous has
started. A running run is never cancelled (`cancel-in-progress: false`).

**Dispatch a cost leg before any page=real soak re-run after this merges.** The images job runs
`web/smoke.sh` on every page=real dispatch, and its new boundary and rally checks have never met the
real app (check.yml does not build `web.Dockerfile`; finding 16). A wrong guess there fails the
images job before any Machine starts, so it costs no Fly money, but it blocks the soak too.

**Owed 93** (a held teardown's page keeps painting at about the live renderer's CPU) is measured,
not judged (owner ruling 4A kept): each column's "p95 + two held pages" adds twice the leg's mean
renderer cores to its p95 and compares it with the size rule. Its row is on `main` (`docs/_INDEX.md`,
Owed 93, since #71), and closes from these numbers.

**Owed after the runs:** the F-entry "real overlay cost" in `_FINDINGS.md` (the table, both
patterns per profile, the sport reason or the football leg), and Owed 93's closure. F-030 holds the
first measurement, legs A and C from run 40153136892. Legs B and D are still owed.

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
9. **The page=real rehearsal 40131252539 tripped the leak gate on a harness false positive**
   (`{"scanned":24,"leaks":1}`, nothing uploaded; the harness's log line was
   `FAIL WEB_COOKIES appears in /out`). The scan took every value in the cookie jar, and the web's
   signed-in hint `seazn_signed_in` has the value `1` (`apps/web/src/lib/auth.ts:79` at
   `7dcb3c42e2`), which every file holds. The other four kinds were scanned on their own and were
   clean in all 24 files, `compositor.log`, `web.log` and `fake-control.log` among them: the page
   token (the relay token in the signed URL), the signing key, the job token and the SRT passphrase.
   So the compositor's log did not carry the relay token. Whether the session cookie was also in a
   file the evidence cannot say (the `1` matched first); the next run scans it on its own. The scan
   now takes every cookie but the named inert ones, refuses a value too short to scan, and reports
   a leak by kind, file and line, redacted.
   `test-driver.sh`'s stub jar carries the hint, so the old scan reds.
10. **The review's positive witness for the real page (M8) has no line to read yet.** It proposed
    requiring the compositor's `gen=N: loading <origin>` line (`cef_graphics.rs`, `restart`). In
    rehearsal 40124733978's platform log (1155 lines) no `loading` line appears; the first line of
    generation 1 is `gen=1: ready`. Owed below.
11. **The brief's renderer kill, `pkill -KILL -f cef`, kills the compositor, not the page.** Every
    CEF child dying aborts the compositor (Owed 62), a process restart, and the soak's compositor
    Machine runs `--restart no` and `--rm`, so the leg would end there. `kill-renderer` kills only
    the `--type=renderer` process with the highest client id (the newest page's, as C14 case 16's
    driver): the compositor reads CefCrashed and rebuilds the page as a new generation, which is
    the reload the brief asks for, and the verdict reads it from the compositor's own log.
12. **The page emits no moment signal.** Its console lines are `seazn:ready`, `seazn:alive`,
    `seazn:score-stale` and the phase channel only (web at `7dcb3c42e2`), so nothing outside the page
    sees a moment play. The legs count moments from the page's own feed instead: the public overlay
    route's `recent` item for each event's seq, read through the same moment rules as the page
    (`lib/overlay-moments.ts`). That counts the moments the page was handed, not the ones it painted;
    the stills are the visual check. A `seazn:moment` console line would let the compositor's log
    count what played.
13. **bwf's best of 3 cannot fill a 10-minute leg at a rally every 3 s.** It lasts at most 3 × 59
    rallies, about 8.9 minutes. The seed's badminton division is best of 5 (`config: {bestOf: 5}`
    over the preset), about 14.8 minutes at most.
14. **A four needs `boundary: 4` to be a moment.** `runs.bat` 4 alone gives none, so
    `score.mjs --boundaries` sends `boundary` with each four and six.
15. **"At least one moment a minute" is the drive's mean, not a floor for every minute.** A floor is
    impossible on badminton at a level game: each game runs 0-0 to 19-19 (38 rallies, about 114 s)
    with no moment, then a game point every second rally, so the corner bug carries a slab about a
    third of the time, in bursts, against cricket's nearly back-to-back cards. The column reports
    the longest stretch without a moment, and the F-entry's comparison of B with A has to say it.
16. **The web-side drivers have not met the real app.** `rally.mjs`, `score.mjs --boundaries`,
    `momentsOf` and the seed's `config: {bestOf: 5}` run only on a dispatch (soak.yml's images
    job smoke, then the legs). Their premises were read against web `7dcb3c42e2` (the PR #75
    review) and hold; the first cost-leg dispatch is their first run.
17. **The stills were the only on-screen witness of a moment, and nothing judged them.** A leg now
    fails with no still written, and the column counts them. Whether a still shows a moment card is
    still a person's look, not a check.
18. **The kill's midpoint was decided on the wrong clock, and up to a second early.** Run
    40153136892's A:noise leg went red on `the kill came -0.08 s from the midpoint (0 to 100)` while
    the product reloaded correctly (crash 0.023 s and ready 5.197 s after the kill); A:snow's kill
    read +0.401 s. The two causes, both in `leg.sh`'s wait: it ended when `(planned - now) / 1000`,
    an integer division, reached 0, so up to 999 ms before the midpoint, and only the exec's latency
    (about a second) usually covered it; and it read this runner's clock while the verdict compares
    the compositor Machine's `at_ms` with `planned_ms`. The fix moves the decision to the Machine:
    `kill-renderer <planned_ms>` waits there until the Machine's own clock reaches it, and its
    `at_ms` is the reading that passed, so `at_ms - planned_ms` is never negative by construction.
    `leg.sh` sends the try 2 s early (to the ms), so the exec's latency no longer delays the kill
    either. **The window's late bound** has no such defect: the verdict reads it on the same pair as
    the early one (the Machine's `at_ms` against `planned_ms`), and `leg.sh`'s try guard, on this
    runner's clock, keeps 15 s for a try to land, against a skew of milliseconds between NTP-synced
    hosts. A try whose exec took over 15 s would land late, and the verdict's red would then be true
    (the kill was late), not a misreading. What remains between the two Machines is `planned_ms`
    itself: the harness Machine's drive start, compared with the compositor Machine's clock, a skew
    of milliseconds.

## Owed (not in this PR)

- **The memory gate's ruling**: check 1 is unchanged. Run the dropout experiment (`minutes=80`) with
  and without `malloc_arena_max=2`, and its `schedule=none` control of the same length, then rule
  from the memory split's measures.
- **A leaks-tracer checkpoint, and the tracer knob with it**: no GStreamer tracer reaches the
  compositor (see "The memory experiment's knobs"). A per-rebuild count of the objects a rebuild
  adds needs the compositor to drive the leaks tracer itself: its `activity-start-tracking` and
  `activity-log-checkpoint` actions around each input rebuild fit "objects added per rebuild" better
  than `get-live-objects`. The workflow knob (`GST_TRACERS=leaks`, `GST_DEBUG=GST_TRACER:7`, from an
  allowlist) comes back in the same PR.

- **The cost legs' numbers**: built here (see "The cost legs"); the F-entry and Owed 93's closure
  wait for the dispatches.
- **M7**: a slate outside the scheduled windows (an unplanned input loss) differs from the camera
  and counts as on air. Cross-check the beat's `programmePhase == live` at each sample's instant.
- **M8**: a positive witness that the real page is the one on air (every generation's `loading`
  origin names the web port). The line has to reach the platform log first (finding 10).
- **M10**: `serve.sh`'s environment allowlist is advisory while node runs as root, which can read
  `/proc/1/environ` (the job token). Run node, `seed.mjs` and `score.mjs` as an unprivileged user.
- **N4, a frozen-payload witness**: `scoreStale` false means the page's polls succeed, not that the
  score it paints advanced. Compare the overlay poll's score (or the programme's score region) with
  the ledger at a few instants.
- **The 3 h timing**: multiply the page=real rehearsal's checker and sampler wall times
  (`driver.log`: `checking` to `done`) by 7.2 against the post-check time (about 19 min after the
  drive's end) before the 3 h run.

## Risks the rehearsal measures

- The checker's and the sampler's time on the recordings, which must fit before the hard stop less
  `post_check_reserve_s` (and inside the watch's `poll_after_min`). Read it from `driver.log`:
  `checking` to `done`.
- Whether the log stream is complete. Check 5a's ratio catches lost lines on a real page.
- Whether `flyctl ssh sftp` works with the soak token.
- Whether `--rootfs-size 30` is accepted and the disk holds 3 h of recordings.
- page=real: whether the compositor's CEF loads the page over plain HTTP from the harness's private
  name, and the page goes `seazn:ready` there. Locally it did, in Chromium, from `localhost`.
- page=real: the web job's time (install, migrate, build: about 15 min, beside the CEF build), and
  whether the harness at performance-8x carries fake-camera's encode with the web server and
  Postgres beside it. Read `web.log`, `postgres.log` and the harness's CPU from the run.
