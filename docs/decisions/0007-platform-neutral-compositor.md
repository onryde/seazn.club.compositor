# 0007. The compositor is platform-neutral; Fly is one host behind a contract

**Status:** Accepted (owner ruling 2026-10-15: "it must not depend on fly.io, we can move somewhere
else easily").

## Context

R2 runs on Fly Machines because R1's runner already creates them. The owner wants to be able to move
the compositor to another host (another container platform, Kubernetes, a VM with Docker) without
rewriting it.

## Decision

The compositor is an OCI image with a **host contract**. Any host that meets the contract can run it
unchanged:

| # | The host must | Why |
|---|---|---|
| 1 | Run one `linux/amd64` OCI container per match, on 4 dedicated-class vCPU and 8 GB | the C00 item-5 budget |
| 2 | Pass `SESSION_ID`, `JOB_TOKEN`, `APP_URL`, `RELAY_DEADLINE_AT` as env. Nothing else is read | spec §5.2 |
| 3 | Allow outbound HTTPS (to `APP_URL`), SRT over UDP (Cloudflare `:778`) and RTMPS over TCP 443. No inbound port, volume or service discovery | spec §5 |
| 4 | Stop it with SIGINT **or** SIGTERM, then wait ≥ 10 s before SIGKILL. Both signals are handled the same | C12 |
| 5 | Restart the container in place on a non-zero exit, at most 2 times, and never after exit 0 | ADR 0004. Exit 78 may restart: the restarted process is refused at bootstrap and exits 78 again, so the cap still bounds it |
| 6 | Nothing else. Liveness is the heartbeat to web, never a host health check | spec §5.3 |

Equivalents for item 5: Fly `restart: on-failure, max_retries: 2`; Docker
`--restart on-failure:2`; Kubernetes Job `backoffLimit: 2` + `restartPolicy: OnFailure`; Nomad
`restart { attempts = 2 }`; Cloud Run jobs `maxRetries: 2`.

**Where Fly is allowed:**
- `seazn.club`'s runner driver (`server/relay/runner-fly.ts`, behind the existing `drivers.ts` seam).
  Moving host means writing a sibling driver there.
- This repo's deploy workflows (C16), where the registry is a single variable.
- The soak runner (C15), which is test infrastructure.

**Where Fly is forbidden:** `crates/` and `docker/`. No `FLY_*` env reads, Fly API calls, `.internal`
names or Fly-specific paths. C01's `scripts/check-portable.sh` enforces it in `check.yml`.

**Proof it holds:** the C13 harness runs the same image under plain `docker compose`. That's a second
host on every PR.

## Consequences

- Moving host = one new web runner driver + one registry variable. The compositor is unchanged.
- In-place restart depends on the host providing item 5. A host without it falls back to web's
  `lost` → new attempt (R1), which costs a cold start but stays correct.
