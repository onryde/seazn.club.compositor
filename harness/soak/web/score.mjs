// The soak's score driver (C15, check (c)): records cricket deliveries on the seeded fixture through
// the standalone app's own API, as the seed's account, one every --every-s seconds until --until-ms,
// and writes one line per attempt to --out:
//
//   {at_ms, ok, status, seq, innings, over, ball, runs} or {..., ok: false, code}
//
// The deliveries follow the engine's rules (packages/engine/src/sports/cricket/cricket.ts): over and
// ballInOver match the ledger, the striker and non-striker are the ledger's, the bowler changes
// every over and never bowls two running (fielding[over % 5]), and no bowler passes the variant's
// quota. Runs are even (no end change mid-over), with boundaries among them so the overlay repaints.
// The ends change at each over's end. After the innings' quota the sides swap: the engine closes the
// innings itself at its ball quota, and this reads the state to make sure before it carries on.
// A refused delivery is recorded, the position re-read from the ledger, and the next one tried at
// once (three refusals running wait out the interval instead).
//
//   node score.mjs --base http://127.0.0.1:3000 --cookies <file> --seed <seed.json> --every-s 30 \
//     --until-ms <epoch ms> --out <deliveries.jsonl> [--max <n>]
import { appendFileSync, readFileSync } from "node:fs";
import { Door, arg } from "./lib.mjs";

const base = arg("base");
const seed = JSON.parse(readFileSync(arg("seed"), "utf8"));
const everyMs = Number(arg("every-s")) * 1000;
const untilMs = Number(arg("until-ms"));
const out = arg("out");
const max = Number(arg("max", "0"));
if (!(everyMs >= 0) || !Number.isSafeInteger(untilMs)) throw new Error("score: --every-s and --until-ms are numbers");
const door = Door.fromCookieFile(base, arg("cookies"));
const fx = seed.cricket.fixture_id;
const RUNS = [0, 4, 0, 2, 6, 0];

let stopping = false;
process.on("SIGTERM", () => { stopping = true; });
process.on("SIGINT", () => { stopping = true; });

const state = async () => {
  const r = await door.call(`/api/v1/fixtures/${fx}/state`);
  if (!r.json?.ok) throw new Error(`score: state ${r.status}`);
  return r.json.data;
};

// The position comes from the ledger, at the start and after any refusal, so a restarted driver or
// a delivery that landed although its answer was lost (a timeout) never desyncs it: the innings in
// play, its legal balls, and the striker, which follows from the balls because runs are even (the
// ends change only at an over's end).
const st0 = await state();
const cfg = st0.state.cfg;
const perOver = cfg.ballsPerOver ?? 6;
const quota = cfg.ballsPerInnings;
if (!Number.isInteger(quota) || quota <= 0) throw new Error("score: the variant has no ball quota (a limited-overs variant is needed)");
const sides = { home: seed.cricket.home, away: seed.cricket.away };
const first = st0.state.battingFirst === "away" ? "away" : "home";
const other = (s) => (s === "home" ? "away" : "home");
let seq = 0;
let innings = 0;
let balls = 0;
let striker = 0;
const sync = (st) => {
  seq = st.last_seq;
  const list = st.state.innings ?? [];
  const last = list[list.length - 1];
  if (!last) { innings = 0; balls = 0; }
  else if (last.closed) { innings = list.length; balls = 0; }
  else { innings = list.length - 1; balls = last.legalBalls; }
  striker = Math.floor(balls / perOver) % 2;
};
sync(st0);

const deliver = async () => {
  const batting = innings % 2 === 0 ? first : other(first);
  const bat = sides[batting];
  const field = sides[other(batting)];
  const over = Math.floor(balls / perOver);
  const ballInOver = (balls % perOver) + 1;
  const runs = innings % 2 === 0 ? RUNS[balls % RUNS.length] : (balls % 6 === 3 ? 4 : 0);
  const payload = {
    over, ballInOver,
    striker: bat[striker], nonStriker: bat[1 - striker], bowler: field[over % 5],
    runs: { bat: runs },
  };
  const at = Date.now();
  const r = await door.call(`/api/v1/fixtures/${fx}/events`, "POST", { expected_seq: seq, type: "cricket.ball", payload });
  const row = { at_ms: at, ok: !!r.json?.ok, status: r.status, innings: innings + 1, over, ball: ballInOver, runs };
  if (row.ok) {
    seq = r.json.data.seq;
    row.seq = seq;
    balls += 1;
    if (balls % perOver === 0) striker = 1 - striker; // the ends change at the over's end
  } else {
    row.code = r.json?.error?.code ?? null;
    // Carry on from the ledger: the next attempt goes at once, from its position.
    try { sync(await state()); } catch { /* the next attempt reports it */ }
  }
  appendFileSync(out, JSON.stringify(row) + "\n");
  return row;
};

let n = 0;
let next = Date.now();
let refusedRun = 0;
while (!stopping && Date.now() < untilMs && (max === 0 || n < max)) {
  const row = await deliver();
  n += 1;
  if (row.ok && balls === quota) {
    // The quota is bowled: the engine closes the innings itself. Make sure, then go on from the ledger.
    let st = await state();
    if (!st.state.innings?.[innings]?.closed) {
      const c = await door.call(`/api/v1/fixtures/${fx}/events`, "POST", { expected_seq: st.last_seq, type: "cricket.innings.close", payload: { reason: "other" } });
      // Not a delivery: logged, never in --out (check (c) counts that file's rows).
      console.log(`score: closed innings ${innings + 1} by hand (${c.status})`);
      st = await state();
    }
    sync(st);
  }
  if (!row.ok) {
    refusedRun += 1;
    if (refusedRun >= 3) {
      // Three refusals in a row: wait out the interval rather than hammer the door.
      refusedRun = 0;
    } else continue;
  } else refusedRun = 0;
  next += everyMs;
  const wait = next - Date.now();
  if (wait > 0) await new Promise((res) => setTimeout(res, Math.min(wait, Math.max(0, untilMs - Date.now()))));
  else next = Date.now();
}
console.log(`score: ${n} attempts, innings ${innings + 1}, ball ${balls} of ${quota}`);
