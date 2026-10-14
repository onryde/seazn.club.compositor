# 0004. Supervisor runs in-process in Rust; Fly restart policy `on-failure` with max_retries 2

- **Status:** accepted (owner, 2026-10-14)
- **Supersedes:** two Sept rulings: the TypeScript/Node 26 supervisor, and R1's `restart: {policy: "no"}`
  in `runner-fly.ts`.

## Decision

- The supervisor is part of the Rust binary, so the image has no Node runtime.
- The Fly restart policy is `on-failure` with `max_retries: 2`.
- `auto_destroy` stays on.
- A clean exit (`ending`, deadline, SIGTERM) returns 0, and the Machine is destroyed.
- A crash restarts the Machine in place, at most twice.
- On the third crash Fly stops restarting it. Heartbeats then stop, web marks the session `lost`,
  and the backend starts `r<n+1>`.

## Rejected

- **`always`.** It restarts even after a clean exit, which fights `auto_destroy` and leaves billing
  orphans.
- **`no`.** Any crash takes the broadcast down until the backend reacts, which breaks rank-1
  "output never stops".

## Consequences

R1's sweep must count Fly in-place restarts as the **same attempt**. Otherwise one crash
double-spawns and double-bills. This is item 3 of spec §8, and it carries a sequence test.
