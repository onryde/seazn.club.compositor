// Seeds the standalone web app for the soak (C15), through the app's own API, as one dedicated
// account it signs in with a dev magic link (AUTH_DEV_LINKS=1, the isolated Machine only). It
// creates:
//
// - a public competition, "Seazn Soak";
// - a cricket division (odi) with two teams of six, its league fixture started, both lineups set and
//   the match started (core.start). ODI, not T20: at score.mjs's two deliveries a minute, T20's 240
//   balls would end the match at minute 120 of a 180-minute drive, while ODI's first innings alone
//   is 300 balls. Five bowlers a side cover an innings (10 overs each, never two overs running);
//   no wicket falls, so two batters suffice;
// - a badminton division (bwf, made best of 5 through the division's config, which the module's own
//   schema checks) with two players and its fixture started, for the cost legs: rally.mjs records a
//   rally every 3 s for at least 10 minutes, and bwf's best of 3 lasts at most about 8.9 (rally.mjs).
//
// It writes the ids (nothing secret) to --out, and the session cookies to --cookies (mode 600).
// The magic-link token is used once, in memory, and never printed.
//
//   node harness/soak/web/seed.mjs --base http://127.0.0.1:3000 --email <e2e address> \
//     --out <seed.json> --cookies <file>
import { writeFileSync } from "node:fs";
import { Door, arg } from "./lib.mjs";

const base = arg("base");
const email = arg("email");
const out = arg("out");
const cookies = arg("cookies");
if (!/^delivered\+[a-z0-9-]+@resend\.dev$/.test(email)) {
  console.error("seed: the account is an e2e address (delivered+<tag>@resend.dev), never a person's");
  process.exit(64);
}
const tag = email.slice("delivered+".length, email.indexOf("@"));
const door = new Door(base);

const link = await door.must("/api/auth/magic-link", "POST", { email });
if (!link?.login_url) throw new Error("seed: no login_url (is AUTH_DEV_LINKS=1 set on the server?)");
const token = new URL(link.login_url, base).searchParams.get("token");
if (!token) throw new Error("seed: the login_url carries no token");
await door.must("/api/auth/magic-link/consume", "POST", { token });
door.saveCookies(cookies);

const person = async (full_name) => (await door.must("/api/v1/persons", "POST", { full_name })).id;

const comp = await door.must("/api/v1/competitions", "POST", {
  name: `Seazn Soak ${tag}`, ends_on: "2030-12-31", visibility: "public",
});

// Cricket: home and away rosters of six, keyed by the fixture's own sides below.
const roster = async (side) => {
  const ids = [];
  for (let i = 1; i <= 6; i++) ids.push(await person(`Soak ${side}${i} ${tag}`));
  return ids;
};
const teamA = await roster("A");
const teamB = await roster("B");
const cricket = await door.must(`/api/v1/competitions/${comp.id}/divisions`, "POST", {
  name: "Soak cricket", sport_key: "cricket", variant_key: "odi",
});
const cEntrants = await door.must(`/api/v1/divisions/${cricket.id}/entrants`, "POST", [
  { kind: "team", display_name: `Soak Strikers ${tag}`, seed: 1, members: teamA.map((person_id) => ({ person_id })) },
  { kind: "team", display_name: `Soak Rovers ${tag}`, seed: 2, members: teamB.map((person_id) => ({ person_id })) },
]);
const rosterOf = { [cEntrants[0].id]: teamA, [cEntrants[1].id]: teamB };
const cStage = await door.must(`/api/v1/divisions/${cricket.id}/stages`, "POST", { seq: 1, kind: "league", name: "League" });
const cGen = await door.must(`/api/v1/stages/${cStage.id}/generate`, "POST");
const cfx = cGen.fixtures[0];
await door.must(`/api/v1/divisions/${cricket.id}/start`, "POST");
const home = rosterOf[cfx.home_entrant_id];
const away = rosterOf[cfx.away_entrant_id];
for (const [entrant, ids] of [[cfx.home_entrant_id, home], [cfx.away_entrant_id, away]]) {
  await door.must(`/api/v1/fixtures/${cfx.id}/lineups/${entrant}`, "PUT", {
    slots: ids.map((person_id, i) => ({ person_id, order_no: i + 1 })),
  });
}
const cStart = await door.must(`/api/v1/fixtures/${cfx.id}/events`, "POST", { expected_seq: 0, type: "core.start", payload: {} });

// Badminton, for the cost legs: two players, one fixture, started; best of 5 (rally.mjs).
const p1 = await person(`Soak P1 ${tag}`);
const p2 = await person(`Soak P2 ${tag}`);
const badminton = await door.must(`/api/v1/competitions/${comp.id}/divisions`, "POST", {
  name: "Soak badminton", sport_key: "badminton", variant_key: "bwf", config: { bestOf: 5 },
});
await door.must(`/api/v1/divisions/${badminton.id}/entrants`, "POST", [
  { kind: "individual", display_name: `Soak P1 ${tag}`, seed: 1, members: [{ person_id: p1 }] },
  { kind: "individual", display_name: `Soak P2 ${tag}`, seed: 2, members: [{ person_id: p2 }] },
]);
const bStage = await door.must(`/api/v1/divisions/${badminton.id}/stages`, "POST", { seq: 1, kind: "league", name: "League" });
const bGen = await door.must(`/api/v1/stages/${bStage.id}/generate`, "POST");
const bfx = bGen.fixtures[0];
await door.must(`/api/v1/divisions/${badminton.id}/start`, "POST");
const bStart = await door.must(`/api/v1/fixtures/${bfx.id}/events`, "POST", { expected_seq: 0, type: "core.start", payload: {} });

const seed = {
  email,
  competition_id: comp.id,
  cricket: {
    division_id: cricket.id, fixture_id: cfx.id, home_entrant_id: cfx.home_entrant_id,
    away_entrant_id: cfx.away_entrant_id, home, away, seq: cStart.seq,
  },
  badminton: {
    division_id: badminton.id, fixture_id: bfx.id, home_entrant_id: bfx.home_entrant_id,
    away_entrant_id: bfx.away_entrant_id, seq: bStart.seq,
  },
};
writeFileSync(out, JSON.stringify(seed, null, 2) + "\n");
console.log(`seed: competition ${comp.id}, cricket fixture ${cfx.id}, badminton fixture ${bfx.id}`);
