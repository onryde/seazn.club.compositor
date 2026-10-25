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
  1. (Superseded 2026-10-25: see the amendment "no GitHub environment" below.) If `secrets.FLY_API_TOKEN_STG` is empty: `echo "::warning::FLY_API_TOKEN_STG unset — push skipped"`,
     then exit 0.
  2. `flyctl auth docker`
  3. Build `docker/Dockerfile` (same `GSTCEF_COMMIT` build-arg as `image.yml`, read from one place:
     `docker/GSTCEF_COMMIT`, a one-line file C12 creates, which **both** workflows read). (Amended
     2026-10-24, Owed 82: build the default target or `--target relay`, never `relay-test-hooks`;
     see the amendment "Owed 82" below.)
  4. Tag `registry.fly.io/seazn-relay-stg:${{ github.sha }}`, and push.
  5. Write to the job summary: the image ref, plus the exact promote command (step 6 of
     `promote.yml`), not run.
- **`deploy-prod.yml`:** on `push` of tags `v*.*.*`. Same as stg, with
  `registry.fly.io/seazn-relay-prod:<tag>` and secret `FLY_API_TOKEN_PROD`. It refuses unless the tag
  points at a commit on `main` (`git merge-base --is-ancestor $GITHUB_SHA origin/main`).
- **`promote.yml`:** (Superseded 2026-10-25: see the amendment "no GitHub environment" below.) `workflow_dispatch` with inputs `env` (stg|prod) and `ref` (a sha or tag that
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

**Rollback** is `promote.yml` with the previous ref. The runbook (C17) says so. (Superseded
2026-10-25: rollback is `scripts/promote.sh` with the previous ref, which it prints.)

## Files

- Create: `.github/workflows/{deploy-stg,deploy-prod,promote}.yml` (amended 2026-10-25: no
  `promote.yml`; create `scripts/promote.sh`)
- Create: `fly/stg.toml`, `fly/prod.toml`
- Create: `docker/GSTCEF_COMMIT`. If C12 already created it, use it.
- Modify: `.github/workflows/image.yml`, to read `docker/GSTCEF_COMMIT`
- Create: `scripts/check-workflows.sh`. It runs `actionlint` (pinned version) and three static greps:
  - every workflow that uses a Fly token has the `::warning::` guard
  - none of them echo a `secrets.` value
  - `promote.yml` declares an `environment:` (superseded 2026-10-25: no workflow declares one)

  `check.yml` runs it.

## Steps

- [ ] **Step 1: Write `scripts/check-workflows.sh`,** and get it red against an empty set, so it
  fails when it found no workflows (anti-vacuity: `checked N workflows`, `N == 0` ⇒ fail).
- [ ] **Step 2: Write the three workflows and the fly tomls.** Get the check green.
- [ ] **Step 3: Mutate once each.**
  1. Remove the `::warning::` guard from `deploy-stg.yml`. Expected: check red.
  2. Remove `environment:` from `promote.yml`. Expected: check red. (Retargeted 2026-10-25: add
     `environment: relay-stg-push` to deploy-stg.yml's push job.)
- [ ] **Step 4: Live proof.**
  0. Re-pin the relay app: `flyctl apps list | grep seazn-relay`, and confirm `FLY_RELAY_APP` on
     `seazn-club-stg` names it (`flyctl secrets list -a seazn-club-stg`, or its `[env]`). If the app
     has another name, use that name in every registry ref and record the deviation.
  1. Merge to `main`, and confirm `deploy-stg` pushed. Paste the run URL and image ref.
  2. Run `promote.yml env=stg` with that sha (amended 2026-10-25: the owner runs
     `scripts/promote.sh stg <sha>@sha256:<digest>`, the command deploy-stg's summary prints). **This is a live change on stg: confirm with the owner
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
- (Amended 2026-10-23, the PR #47 review's I-1) No deploy workflow reads or writes
  `ghcr.io/onryde/seazn-relay-cache` (no `cache-from`, no `cache-to`, no `build-contexts` naming it),
  `check-workflows.sh` refuses the name in all three, and mutant 5 went red, so 5 mutants are red in all.
- (Amended 2026-10-24, Owed 82, the PR #62 review's m5) The deploy builds use the default target or
  `--target relay`. `check-workflows.sh` refuses `relay-test-hooks`, `rust-build-test-hooks` and
  `test-hooks` in the three workflows, and mutant 6 went red, so 6 mutants are red in all.

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
  4. Delete the gate step from `promote.yml`. Expected: `check-workflows.sh` red. (Retargeted
     2026-10-25: delete it from deploy-prod.yml.)

## Amendment 2026-10-23 (the CI build cache is untrusted for shipping)

The PR #47 review's I-1 (compositor `_INDEX.md` decision log, "CI: CEF layer from GHCR"). image.yml's
`cache` job writes BuildKit's layer cache to `ghcr.io/onryde/seazn-relay-cache:cef-<key>`, and any
repository writer can write that tag: a `workflow_dispatch` runs the dispatched ref's copy of
image.yml, and a same-repository PR's own copy can ask for `packages: write`. BuildKit serves a
cached layer without rebuilding it, so whatever manifest sits at the tag becomes the image's layers.

- **The rule.** The deploy builds (`deploy-stg.yml`, `deploy-prod.yml`) build every layer from source
  or from a cache only a protected path writes. They never name `seazn-relay-cache`: no `cache-from`,
  no `cache-to`, no `build-contexts`. `promote.yml` builds nothing, and is checked anyway. A shared
  cache for deploys, if ever wanted, is a new cache written only from a protected environment, and it
  needs its own decision row.
- **The gate.** `check-workflows.sh` gains a fifth static grep: `seazn-relay-cache` appears in no
  deploy or promote workflow.
- **New mutant:**
  5. Add `cache-from: type=registry,ref=ghcr.io/onryde/seazn-relay-cache:cef-x` to `deploy-stg.yml`.
     Expected: `check-workflows.sh` red.

## Amendment 2026-10-24 (Owed 82: a deploy builds the relay target, never the test-hooks one)

The PR #62 review's m5 (compositor `_INDEX.md` decision log "Owed 82: the held-teardown hook").
PR #62 adds two stages to `docker/Dockerfile`: `rust-build-test-hooks` and `relay-test-hooks`.
The second is the relay image with a compositor built with its `test-hooks` feature, for C14 case
16 only: `COMPOSITOR_TEST_HOLD_TEARDOWNS` makes that binary hold CEF teardowns for good. Its last
line is `FROM relay`, so a build with no target is still the relay image. image.yml proves its
build carries no hook. A deploy build is a separate build of the same Dockerfile, so it is pinned
too. This is defense in depth: no deploy workflow exists yet.

- **The rule.** `deploy-stg.yml` and `deploy-prod.yml` build the default target or
  `--target relay`. Neither names any other target. `promote.yml` builds nothing, and is checked
  anyway.
- **The gate.** `check-workflows.sh` gains a sixth static grep: `relay-test-hooks`,
  `rust-build-test-hooks` and `test-hooks` appear in no deploy or promote workflow.
- **Optional.** The deploy job runs image.yml's two-marker check, "The relay image carries no test
  hook (Owed 82)", on the image it pushes. The check reads `/usr/local/bin/compositor`, checks the
  ELF magic, and requires 0 of `COMPOSITOR_TEST_HOLD_TEARDOWNS` and of `test hook (Owed 82)`. Its
  positive pair needs a `relay-test-hooks` build. If the deploy job adds this check, it builds that
  image beside the pushed one and never pushes it.
- **New mutant:**
  6. Add `target: relay-test-hooks` to `deploy-stg.yml`'s build. Expected: `check-workflows.sh`
     red.

## Amendment 2026-10-25 (no GitHub environment: a relay-only repository token, and a local promote)

Owner, 2026-10-25, in chat, relayed by the coordinator: "Ok, I don't want to create the environment
in github. and we cand add the rules in workflow so don't need all C16 rigts?", then, after a GHCR
design: "No, I want to have Dockerfile and workflow build the image and push to fly". The reasoning
is the compositor `_INDEX.md` decision row "C16 without GitHub environments". This replaces the
environments of fix rounds 1 and 2 (PR #85) and the promote workflow.

- **Push.** `deploy-stg.yml` (push to `main`, and `workflow_dispatch`) and `deploy-prod.yml` (`v*`
  tags) build `docker/Dockerfile`'s relay target as before, log in with `flyctl auth docker`, and
  push `registry.fly.io/seazn-relay-<env>:<sha or tag>`. The token is a **repository** secret,
  `FLY_API_TOKEN_RELAY_STG` (prod: `FLY_API_TOKEN_RELAY_PROD`, minted only after C17), and it is a
  deploy token for the relay app alone: `fly tokens create deploy -a seazn-relay-<env>`. No workflow
  declares `environment:`, and the step that checked an environment's policy is gone.
- **Main only, as hygiene.** The push job runs on `if: github.ref == 'refs/heads/main'` (prod:
  `startsWith(github.ref, 'refs/tags/v')`), and its provenance step refuses any other ref. **This is
  not a guard.** Any branch's own copy of a workflow can drop both and read a repository secret. The
  owner accepted that risk. What it can do is bounded by the next bullet.
- **A squat never ships.** Every push is attested (`actions/attest`, with `id-token` and
  `attestations` write on the push job alone). Both paths, pushed and found in the registry, verify
  the record with `gh attestation verify --cert-identity
  https://github.com/onryde/seazn.club.compositor/.github/workflows/deploy-<env>.yml@<refs/heads/main
  or refs/tags/<tag>>`, `--source-ref`, `--source-digest` and `--signer-digest` at the commit, and
  `--deny-self-hosted-runners`. An image a branch pushed under a future main sha's tag fails main's
  own run, and promote refuses it before it calls the web app.
- **Promote is `scripts/promote.sh`, run by the owner on their machine** with their stored flyctl
  login (`flyctl auth login`), so no web-app token is ever in GitHub. `promote.yml` is removed.
  - `scripts/promote.sh stg <sha>@sha256:<digest>`, or `scripts/promote.sh prod
    <vX.Y.Z>@sha256:<digest> --confirm-prod`: the command deploy-<env>'s summary prints.
  - In order, it refuses: a ref without its digest; prod without `--confirm-prod`, and stg with it; a
    missing tool (git, jq, flyctl, gh, docker with buildx); a copy of itself that is not main's; a
    commit not on `main`, or whose own tree fails the CEF bump gate (main's copy of the gate); a tag
    that does not resolve to the pin (read with flyctl's docker login, since Fly's registry refuses
    anonymous reads; only "not found" means not pushed); a digest with no deploy-<env>.yml
    attestation for that commit at its ref; an `[env]` RELAY_IMAGE on the web app (it prints the
    seazn.club diff); and any other secret on the web app that is not Deployed.
  - Then it prints the previous RELAY_IMAGE, from its own log (`~/.local/state/seazn-relay-promote/<env>.log`,
    matched by Fly's digest of the live value, which is write-only), stages
    `RELAY_IMAGE=registry.fly.io/seazn-relay-<env>@sha256:<pin>` (the digest alone), runs
    `flyctl secrets deploy`, and proves the new digest reads Deployed.
  - It drops FLY_API_TOKEN and FLY_ACCESS_TOKEN from its own environment, and never reads, prints or
    needs a token value.
- **Rollback** is `scripts/promote.sh` with the previous ref, which it prints.
- **The gate.** `check-workflows.sh` adds these rules:
  - no `environment:` in any workflow;
  - a deploy workflow names only its own relay token, and only in its push job;
  - no workflow names a web-app token or hands flyctl a `seazn-club-*` app;
  - soak.yml's `FLY_API_TOKEN_SOAK` is the one other Fly secret, exempt by name;
  - every step after the token guard reads it;
  - write grants appear only as `id-token` and `attestations` on the two push jobs, plus image.yml's
    cache job's `packages: write`;
  - the attest step comes after the push, and the record check after the attest, with no `if` but
    the guard's own;
  - no promote workflow exists, by name or by a `flyctl secrets set|deploy` line.

  `deploy_checks_test.sh` runs promote.sh under fake flyctl, docker and gh, reaching every refusal.
- **Mutants 2 and 4 retarget** (promote.yml is gone):
  2. Add `environment: relay-stg-push` to deploy-stg.yml's push job. Expected: `check-workflows.sh`
     red.
  4. Delete the CEF bump gate step from deploy-prod.yml. Expected: `check-workflows.sh` red.
- **Acceptance, amended.** deploy-stg pushes and attests on `main` with the repository token. The
  owner runs `scripts/promote.sh stg` once, on their go-ahead. No workflow names an environment or a
  web-app token. The 6 brief mutants and each new rule's mutant are red.

## Do not touch

The `seazn.club` repo, except reading it. Web's own deploy is not this repo's job.

## Output cap

Under 15 lines.
