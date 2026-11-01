# relay-job.v1 fixtures

Hand-written vectors for `docs/contracts/relay-job.v1.json` (R2002). They are never generated from the
code they test. `apps/web/src/server/api-v1/__tests__/relay-contract.test.ts` sweeps every `*.json` here
against the zod twins and against the published file under ajv, and pins the count. The compositor
vendors this directory with the contract (C07).

## Names

The prefix names the shape:

| Prefix | Shape |
|---|---|
| `session-` | `#/$defs/session`: the `data` of `GET /api/internal/relay/sessions/{sid}` |
| `heartbeat-` | `#/$defs/heartbeat`: the body of `POST …/heartbeat` |
| `reply-` | `#/$defs/heartbeatReply`: the `data` of that POST's 200 |
| `phase-reply-` | `#/$defs/phaseReply`: the compositor's loopback `GET /phase` reply, which the overlay page reads (R2007 PR B) |

What follows the prefix says how a reader must treat the vector:

- **A plain name** (`session-composed-720p`) is valid: the schema admits it, and so does a consumer.
- **`invalid-`** (`heartbeat-invalid-unknown-field`) is refused by the JSON Schema itself.
  `heartbeat-invalid-exit-while-playing` is a one-field edit of `heartbeat-final-fatal`, refused by
  the schema's `if`/`then`: `exitReason` and `exitDetail` are sent only with `state: "stopped"`.
- **`refused-`** (`session-refused-delay-mismatch`, `session-refused-delay-absent`) is admitted by the
  JSON Schema and refused by a rule that JSON Schema cannot state, which the contract states in the
  field's prose: a composed session's `overlayUrl` carries `delay=` exactly once, equal to
  `overlayDelayMs`. A consumer must refuse these. Each is a one-field edit of `session-composed-720p`.

## The envelope

On the wire a 200 is `{ "ok": true, "data": … }`. The `session-` and `reply-` vectors are the `data`.
The heartbeat body is sent bare. The `phase-reply-` vectors are bare too: the compositor serves that
reply, not web, so it has no envelope.

## Absent is not zero

A Machine omits a field it has not measured. It is never sent as 0, because
a present 0 is a measured zero.
`heartbeat-zero-is-a-measurement` carries `fps: 0`: it is valid, and it means the programme was
measured at 0 frames per second, never "not measured". `heartbeat-minimal` carries `state` alone: every
other field is unmeasured. The server stores what was sent, so an omitted field is an absent key in the
stored beat, never a `null` and never a `0`.

## Values

- `session-composed-720p` carries 3100 ms, the delay compositor `_FINDINGS.md` F-002 measured and the
  provisional default when this contract was cut (web `config.ts` `RELAY_OVERLAY_DELAY_DEFAULT_MS` is
  the authority, not this file). `session-composed-1080p` carries 2500 ms, an organiser's override, so
  no one constant satisfies both.
- The output profiles are the compositor's Global Constraints: 1280×720 at 30 fps and 4000 kbps, and
  1920×1080 at 30 fps and 6000 kbps.
- `heartbeat-full` carries every heartbeat field except the final beat's two. `heartbeat-final-fatal`
  is a Machine's last beat: `state` `stopped`, the reason it is exiting (`exitReason`, the
  compositor's `ExitReason`) and what went wrong in words (`exitDetail`, redacted by the compositor).
  The server's bound on `exitDetail` is 200 UTF-16 code units. The schema publishes 100 characters,
  the bound under which any text fits. `heartbeat-invalid-exit-reason` names a reason outside the
  vocabulary. The server records both fields with the beat, scrubbed again, and does nothing else with
  them.
- `heartbeat-final-restart` is the final beat of an in-place restart (compositor C11b): `exitReason: "restart"`
  and an `exitDetail` of the form `restart in place: <cause>`. The process then exits 75 and the host restarts it on
  the same Machine; the server records the beat and does not count it as a crash.
- `heartbeat-full` carries `phaseChannel: "web"`, the fallback. `heartbeat-final-hold` is a beat
  during the final hold (compositor C06): `programmePhase: "final"` on the primary channel,
  `phaseChannel: "loopback"`. `heartbeat-invalid-phase-channel` is a one-field edit of it, refused on
  `phaseChannel` alone. `phase-reply-final` is the loopback reply in `final`;
  `phase-reply-invalid-unknown-field` adds a key the strict reply does not declare.
- `reply-live-edge` is a reply carrying the phone-live edge counter (Owed 108 fix b): the phone has been lost and come
  back twice in this session. A Machine acts once per INCREASE of the value it has seen. `reply-live-edge-zero` is the
  counter at its first appearance: the phone's first-ever live, which is no edge, so a present 0 is a counter and is not
  "absent". `reply-live-edge-null` is the unknown spelled out; web omits the key instead (`reply-live` is that reply),
  and a reader treats null and absent alike. `reply-invalid-live-edge-negative` and `reply-invalid-live-edge-fractional`
  are not counts.
- `heartbeat-airings` carries R4's proof of play (sponsors on stream spec 2026-10-27 §3.6): the overlay page's
  `seazn:sponsor:start` and `seazn:sponsor:end` console lines, each tagged by the compositor with `kind` from the
  line's name. It holds a break card (`12:sponsor.over_end`, a lower third the rotation chose) and a scene carousel
  showing (`scene:reconnecting:<10-second bucket>`, a banner with `breakKind: null`), each with its end, and the
  scene's end is cut short. Every `at` is UTC `Z`, as the page writes it. `heartbeat-invalid-airing-unknown-key`
  adds `sponsorName` to the first item. The page never writes that key, and because both the item and the beat are
  strict, that one key refuses the whole beat. Validate each line before buffering it.
- `heartbeat-full` also carries `airingsDropped: 3` without `airings`. The compositor sends the count on every
  beat once it is above 0: it is a total for the process, an upper bound, and it starts again from 0 when the
  process restarts. The server keeps it with the beat. It never keeps `airings` there: it records the list as
  sponsor airings and strips it from the stored beat.
- Every id, key, passphrase and token here is a placeholder, never a real credential.
