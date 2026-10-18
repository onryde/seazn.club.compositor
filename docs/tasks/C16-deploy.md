# C16: Deploy workflows (image to the Fly registry, explicit `RELAY_IMAGE` bump)

**Repo:** `seazn.club.compositor`. **Depends on:** C12.

## Context

Pinned 2026-10-14 in `seazn.club`; re-pin before relying on it.

- The web apps are `seazn-club-stg` (`fly.stg.toml`) and `seazn-club-prod` (`fly.toml`).
- R1 creates relay Machines in **one Fly app per deployment**, named by the web env `FLY_RELAY_APP`
  (`apps/web/src/server/relay/config.ts:335` `liveRunnerIdentity`; its error text names
  `seazn-relay-stg` / `seazn-relay-prod`). It refuses the retired shared default.
- The image comes from the web env `RELAY_IMAGE` (`runner-fly.ts:200`). Unset ⇒ the live runner
  refuses to construct.

**Portability (ADR 0007):** the registry is one repo variable, `RELAY_REGISTRY` (default
`registry.fly.io`), read by every workflow. No workflow hardcodes the Fly registry outside that
default. Moving host changes the variable and the web runner driver, nothing in `crates/`.

So a deploy has two parts:
1. **Push** `registry.fly.io/seazn-relay-<env>:<sha>`. This changes nothing live. A Machine uses an
   image only when web creates one.
2. **Promote:** set `RELAY_IMAGE` on the matching web app. That's a live change for every **new**
   session (running Machines keep their image). Owner ruling: the bump is **explicit**, never
   automatic.

The relay apps hold no long-running Machines, so there is no `fly deploy` for them. Their only
config is the app itself. `fly/stg.toml` and `fly/prod.toml` record app name, org and primary region
`lhr` as documentation and for `flyctl apps create --config`. They are never deployed.

## Workflows

- **`deploy-stg.yml`:** on `push` to `main`, and `workflow_dispatch`.
  1. If `secrets.FLY_API_TOKEN_STG` is empty: `echo "::warning::FLY_API_TOKEN_STG unset — push skipped"`,
     then exit 0.
  2. `flyctl auth docker`
  3. Build `docker/Dockerfile` (same `GSTCEF_COMMIT` build-arg as `image.yml`, read from one place:
     `docker/GSTCEF_COMMIT`, a one-line file C12 creates, which **both** workflows read)
  4. Tag `registry.fly.io/seazn-relay-stg:${{ github.sha }}`, and push.
  5. Write to the job summary: the image ref, plus the exact promote command (step 6 of
     `promote.yml`), not run.
- **`deploy-prod.yml`:** on `push` of tags `v*.*.*`. Same as stg, with
  `registry.fly.io/seazn-relay-prod:<tag>` and secret `FLY_API_TOKEN_PROD`. It refuses unless the tag
  points at a commit on `main` (`git merge-base --is-ancestor $GITHUB_SHA origin/main`).
- **`promote.yml`:** `workflow_dispatch` with inputs `env` (stg|prod) and `ref` (a sha or tag that
  must already exist in the registry), plus a GitHub **environment** `relay-<env>` with required
  reviewers (the owner), so promotion needs a human approval click.
  1. Same secret guard as above.
  2. `flyctl image show`, or a manifest HEAD, to prove the ref exists. Otherwise fail with "not
     pushed".
  3. `flyctl secrets set RELAY_IMAGE=registry.fly.io/seazn-relay-<env>:<ref> -a seazn-club-<env> --stage`,
     followed by `flyctl secrets deploy -a seazn-club-<env>` (this restarts web with the new env).
     **Re-pin** whether `RELAY_IMAGE` is a secret or a `[env]` entry in `fly.*.toml` before writing
     this step. If it's an `[env]` entry, promotion is a `seazn.club` PR instead, and this workflow
     only prints the diff. Record which one it is in the brief's commit.
  4. Write the previous and new `RELAY_IMAGE` to the summary, for rollback.

**Rollback** is `promote.yml` with the previous ref. The runbook (C17) says so.

## Files

- Create: `.github/workflows/{deploy-stg,deploy-prod,promote}.yml`
- Create: `fly/stg.toml`, `fly/prod.toml`
- Create: `docker/GSTCEF_COMMIT`. If C12 already created it, use it.
- Modify: `.github/workflows/image.yml`, to read `docker/GSTCEF_COMMIT`
- Create: `scripts/check-workflows.sh`. It runs `actionlint` (pinned version) and three static greps:
  - every workflow that uses a Fly token has the `::warning::` guard
  - none of them echo a `secrets.` value
  - `promote.yml` declares an `environment:`

  `check.yml` runs it.

## Steps

- [ ] **Step 1: Write `scripts/check-workflows.sh`,** and get it red against an empty set, so it
  fails when it found no workflows (anti-vacuity: `checked N workflows`, `N == 0` ⇒ fail).
- [ ] **Step 2: Write the three workflows and the fly tomls.** Get the check green.
- [ ] **Step 3: Mutate once each.**
  1. Remove the `::warning::` guard from `deploy-stg.yml`. Expected: check red.
  2. Remove `environment:` from `promote.yml`. Expected: check red.
- [ ] **Step 4: Live proof.**
  0. Re-pin the relay app: `flyctl apps list | grep seazn-relay`, and confirm `FLY_RELAY_APP` on
     `seazn-club-stg` names it (`flyctl secrets list -a seazn-club-stg`, or its `[env]`). If the app
     has another name, use that name in every registry ref and record the deviation.
  1. Merge to `main`, and confirm `deploy-stg` pushed. Paste the run URL and image ref.
  2. Run `promote.yml env=stg` with that sha. **This is a live change on stg: confirm with the owner
     first.** Then confirm with `flyctl secrets list -a seazn-club-stg` (digest changed) and paste it.
  3. Do not promote prod in this task. Prod promotion happens after C17 passes.
- [ ] **Step 5: Commit and open the PR.** `ci(deploy): push relay images per env and promote by explicit approval (C16)`

## Acceptance

- The workflow check is green, and 2 mutants went red.
- An stg image is pushed, and promoted once with the owner's go-ahead.
- Prod is untouched.
- (Amended 2026-10-18, Owed 22) `docker/CEF_STEP0_PASS` is seeded. `check-cef-bump.sh` runs in
  `check.yml` and first in all three deploy workflows. The C17 runbook has the "CEF bump" entry,
  and mutants 3 and 4 went red, so 4 mutants are red in all.

## Amendment 2026-10-18 (a CEF bump is gated on C11 Step 0)

Owner, 2026-10-18, in chat, relayed by the coordinator: "fix the product gaps along with tasks". Owed 22: a CEF bump past Chrome 142 is expected to break loopback, because Local Network
Access is a permission no header satisfies, so C11 Step 0 must be re-run on every bump. This makes
that a **required, enforced step**, not a note.

- **The record.** Create `docker/CEF_STEP0_PASS`, with two lines: the `docker/GSTCEF_COMMIT` value
  C11 Step 0 last passed on, and that run's `_FINDINGS.md` F-id. Seed it with the pinned commit and
  `F-009`.
  - "Passed" means loopback reached: outcome (a), or outcome (a) with a recorded grant (Owed 22's
    candidates).
  - Read a re-run by V2's `seazn:probe=` line, by whether any request reached the server, and by
    `probe-ms`. Never read it by V1 (false-premise row "V1 is the control").
- **The gate.** `scripts/check-cef-bump.sh` fails when:
  - `docker/GSTCEF_COMMIT` differs from line 1 of the record, or
  - line 2 is not a `## F-0xx` heading in `_FINDINGS.md`.

  It prints `cef bump gate: GSTCEF_COMMIT=<sha> step0=<sha> <F-id>`. `check.yml` runs it, so a PR
  that bumps CEF stays red until Step 0 is re-run and recorded.
- **Deploys refuse too.** `deploy-stg.yml`, `deploy-prod.yml` and `promote.yml` each run
  `scripts/check-cef-bump.sh` as their first step after checkout. On a failure they stop with
  `::error::CEF bump without a passing C11 Step 0 re-run (compositor docs/_INDEX.md Owed 22)`.
  `check-workflows.sh` gains a fourth static grep: every workflow that pushes or promotes runs the
  gate.
- **The checklist.** The runbook (C17) gets a "CEF bump" entry:
  1. Bump `docker/GSTCEF_COMMIT`.
  2. Build the image.
  3. Re-run C11 Step 0 on it.
  4. Record F-0xx.
  5. Update `docker/CEF_STEP0_PASS`.
  6. Only then merge, deploy and promote.

  An outcome (b) or (c) means picking a grant from Owed 22's candidates and re-running Step 0.
  Never update the record over a failing run.
- **New mutants:**
  3. Change `docker/GSTCEF_COMMIT` without touching the record. Expected: `check-cef-bump.sh` red.
  4. Delete the gate step from `promote.yml`. Expected: `check-workflows.sh` red.

## Do not touch

The `seazn.club` repo, except reading it. Web's own deploy is not this repo's job.

## Output cap

Under 15 lines.
