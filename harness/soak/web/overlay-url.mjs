// The production overlay page's URL for the soak's compositor (C15, page=real): the relay door, as
// web's sessionFactsForJob builds it (/overlay/fixtures/<id>?sid&st&host=compositor&delay&template),
// with a relay-page token signed by this run's AUTH_SECRET (from the environment). The URL carries
// the token, so it is written to --out (mode 600) and never printed.
//
//   AUTH_SECRET=... node overlay-url.mjs --base http://<host>:3000 --sid <SESSION_ID> --fixture <id> \
//     --exp-ms <epoch ms> --delay 3100 --template bar --out <file>
import { writeFileSync } from "node:fs";
import { arg, relayPageToken } from "./lib.mjs";

const base = arg("base").replace(/\/$/, "");
const sid = arg("sid");
const fixture = arg("fixture");
const expMs = Number(arg("exp-ms"));
const delay = arg("delay");
const template = arg("template");
const out = arg("out");
if (!/^https?:\/\/[^/?#]+$/.test(base)) throw new Error("overlay-url: --base is an origin");
if (!/^[0-9a-f-]{36}$/.test(fixture)) throw new Error("overlay-url: --fixture must be a UUID");
if (!Number.isSafeInteger(expMs) || expMs <= Date.now()) throw new Error("overlay-url: --exp-ms must be in the future");
if (!/^[0-9]+$/.test(delay)) throw new Error("overlay-url: --delay is whole ms");
if (!/^[a-z]+$/.test(template)) throw new Error("overlay-url: --template is a template id");

const q = new URLSearchParams({ sid, st: relayPageToken(sid, expMs), host: "compositor", delay, template });
writeFileSync(out, `${base}/overlay/fixtures/${fixture}?${q}\n`, { mode: 0o600 });
console.log(`overlay-url: ${base}/overlay/fixtures/${fixture} (sid, token, host=compositor, delay ${delay}, template ${template})`);
