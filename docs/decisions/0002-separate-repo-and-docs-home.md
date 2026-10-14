# 0002. Separate repo; this repo is the docs home for R2

- **Status:** accepted (owner, 2026-10-14)

## Decision

The compositor lives in `onryde/seazn.club.compositor`, not in `seazn.club/services/`. All R2 specs,
plans, decisions and task briefs live here under `docs/`. That includes the briefs for changes to
`seazn.club`'s control plane. The code for those changes still lands as `seazn.club` PRs.

## Consequences

- `seazn.club` holds only a pointer row in its streaming programme `_INDEX.md`, plus a superseded
  header on the old `R2-compositor.md`.
- Contracts are authored web-side in `docs/contracts/`. This repo vendors them by hand with a sha256
  pin, following the `seazn.club.capture` convention. Neither CI fetches the other repo.
- House conventions are copied from `seazn.club.capture`: AGENTS.md rulebook, enforced layering,
  fakes first, `_FINDINGS.md` F-ids.
