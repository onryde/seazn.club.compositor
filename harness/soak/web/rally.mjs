// The cost legs' badminton driver (C15, legs B and D): records a rally on the seeded badminton
// fixture through the standalone app's own API, as the seed's account, one every --every-s seconds
// until --until-ms, and writes one line per attempt to --out:
//
//   {at_ms, ok, status, seq, game, home, away, moments} or {..., ok: false, code}
//
// The rallies keep every game as close as it can be, so the corner bug's moments come as often as
// badminton allows: the game's leader (home in games 1, 3 and 5, away in 2 and 4) wins a rally at a
// level score and the trailer wins otherwise. So each game runs 1-0, 1-1, ... 20-19 (game point),
// 20-20, 21-20 (game point), ... to the cap, 29-29, and the leader takes it 30-29 (game won): a
// fresh game point every second rally from 19-19. The score is read from the ledger (the state
// route) after every rally, never counted here, and the driver stops when the match is decided.
//
// `moments` are the ones the overlay page makes of the rally, read from the page's own feed (the
// public overlay route's `recent` item for the rally's seq: lib.mjs momentsOf). seed.mjs makes the
// division best of 5 (bwf's preset otherwise): at a rally every 3 s, bwf's best of 3 lasts at most
// 3 x 59 rallies, about 8.9 minutes, short of a 10-minute leg; best of 5 lasts about 14.8.
//
//   node rally.mjs --base http://127.0.0.1:3000 --cookies <file> --seed <seed.json> --every-s 3 \
//     --until-ms <epoch ms> --out <deliveries.jsonl> [--max <n>]
import { appendFileSync, readFileSync } from "node:fs";
import { Door, arg, momentsAt } from "./lib.mjs";

const base = arg("base");
const seed = JSON.parse(readFileSync(arg("seed"), "utf8"));
const everyMs = Number(arg("every-s")) * 1000;
const untilMs = Number(arg("until-ms"));
const out = arg("out");
const max = Number(arg("max", "0"));
if (!(everyMs >= 0) || !Number.isSafeInteger(untilMs)) throw new Error("rally: --every-s and --until-ms are numbers");
const door = Door.fromCookieFile(base, arg("cookies"));
const fx = seed.badminton.fixture_id;

let stopping = false;
process.on("SIGTERM", () => { stopping = true; });
process.on("SIGINT", () => { stopping = true; });

const state = async () => {
  const r = await door.call(`/api/v1/fixtures/${fx}/state`);
  if (!r.json?.ok) throw new Error(`rally: state ${r.status}`);
  return r.json.data;
};

// The game in play and its score, from the ledger's state (closed games first, then at most one
// open one).
const position = (st) => {
  const sets = st.state.sets ?? [];
  const closed = sets.filter((s) => s.closed).length;
  const open = sets.find((s) => !s.closed) ?? { home: 0, away: 0 };
  return { game: closed, home: open.home, away: open.away, phase: st.state.phase, entrants: st.state.entrants };
};

let st = await state();
let seq = st.last_seq;
let n = 0;
let next = Date.now();
let refusedRun = 0;
while (!stopping && Date.now() < untilMs && (max === 0 || n < max)) {
  const p = position(st);
  if (p.phase !== "live") {
    console.log(`rally: the match is ${p.phase}: stopping`);
    break;
  }
  const leader = p.game % 2 === 0 ? "home" : "away";
  const winner = p.home === p.away ? leader : (p.home < p.away ? "home" : "away");
  const at = Date.now();
  const r = await door.call(`/api/v1/fixtures/${fx}/events`, "POST", {
    expected_seq: seq, type: "badminton.rally", payload: { wonBy: p.entrants[winner] },
  });
  const row = { at_ms: at, ok: !!r.json?.ok, status: r.status, game: p.game + 1, home: p.home, away: p.away, won_by: winner };
  if (row.ok) {
    seq = r.json.data.seq;
    row.seq = seq;
    row.moments = await momentsAt(door, fx, seq);
  } else {
    row.code = r.json?.error?.code ?? null;
  }
  try {
    st = await state();
    seq = st.last_seq;
  } catch { /* the next attempt reports it */ }
  appendFileSync(out, JSON.stringify(row) + "\n");
  n += 1;
  if (!row.ok) {
    refusedRun += 1;
    if (refusedRun < 3) continue;
    refusedRun = 0;
  } else refusedRun = 0;
  next += everyMs;
  const wait = next - Date.now();
  if (wait > 0) await new Promise((res) => setTimeout(res, Math.min(wait, Math.max(0, untilMs - Date.now()))));
  else next = Date.now();
}
const p = position(st);
console.log(`rally: ${n} attempts, game ${p.game + 1} at ${p.home}-${p.away}, games ${st.state.setsWon?.home ?? "?"}-${st.state.setsWon?.away ?? "?"}`);
