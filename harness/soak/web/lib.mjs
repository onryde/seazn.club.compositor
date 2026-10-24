// The standalone web app's HTTP door, for the soak's seed and score driver (C15). No dependencies:
// Node's own fetch and crypto. The session cookie is held in memory and in a mode-600 file the
// caller names, never in argv, a log or /out. The cookie is `Secure` under production, so it is
// sent by hand in a Cookie header (fetch over plain HTTP would drop it).
import { createHmac } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";

export function arg(name, fallback) {
  const i = process.argv.indexOf(`--${name}`);
  if (i < 0 || i + 1 >= process.argv.length) {
    if (fallback !== undefined) return fallback;
    console.error(`missing --${name}`);
    process.exit(64);
  }
  return process.argv[i + 1];
}

export class Door {
  constructor(base, cookies = {}) {
    this.base = base.replace(/\/$/, "");
    this.cookies = cookies;
  }

  static fromCookieFile(base, path) {
    return new Door(base, JSON.parse(readFileSync(path, "utf8")));
  }

  saveCookies(path) {
    writeFileSync(path, JSON.stringify(this.cookies), { mode: 0o600 });
  }

  async call(path, method = "GET", body) {
    for (let attempt = 0; ; attempt++) {
      const headers = { "content-type": "application/json" };
      const jar = Object.entries(this.cookies).map(([k, v]) => `${k}=${v}`).join("; ");
      if (jar) headers.cookie = jar;
      let res;
      try {
        res = await fetch(this.base + path, {
          method, headers, body: body === undefined ? undefined : JSON.stringify(body),
          signal: AbortSignal.timeout(20_000),
        });
      } catch (e) {
        return { status: 0, json: { ok: false, error: { code: "NETWORK", message: String(e?.cause?.code ?? e?.name ?? e) } } };
      }
      for (const sc of res.headers.getSetCookie?.() ?? []) {
        const m = sc.match(/^([^=]+)=([^;]*)/);
        if (!m) continue;
        if (m[2] === "") delete this.cookies[m[1]];
        else this.cookies[m[1]] = m[2];
      }
      // Scoring is rate-limited per fixture: back off rather than fail (smoke-sports.ts does too).
      if (res.status === 429 && attempt < 5) {
        await new Promise((r) => setTimeout(r, 400));
        continue;
      }
      const json = await res.json().catch(() => ({ ok: false }));
      return { status: res.status, json };
    }
  }

  async must(path, method = "GET", body) {
    const r = await this.call(path, method, body);
    if (!r.json?.ok) {
      const e = r.json?.error;
      const detail = typeof e === "string" ? e : `${e?.code ?? ""} ${e?.message ?? ""}`;
      throw new Error(`${method} ${path}: ${r.status} ${detail}`);
    }
    return r.json.data;
  }
}

const b64url = (buf) => Buffer.from(buf).toString("base64url");

// The relay page token, as web's mintRelayToken makes it (apps/web/src/server/relay/tokens.ts:
// HS256 over AUTH_SECRET's UTF-8 bytes, typ JWT, audience seazn-relay, claims {sid, scope:
// "relay-page"} plus iat and exp). The secret comes from the environment only.
export function relayPageToken(sid, expMs, secret = process.env.AUTH_SECRET) {
  if (!secret) throw new Error("AUTH_SECRET is not set");
  if (!/^[0-9a-f-]{36}$/.test(sid)) throw new Error("sid must be a UUID");
  const header = b64url(JSON.stringify({ alg: "HS256", typ: "JWT" }));
  const now = Math.floor(Date.now() / 1000);
  const payload = b64url(JSON.stringify({ sid, scope: "relay-page", iat: now, exp: Math.floor(expMs / 1000), aud: "seazn-relay" }));
  const sig = createHmac("sha256", Buffer.from(secret, "utf8")).update(`${header}.${payload}`).digest("base64url");
  return `${header}.${payload}.${sig}`;
}
