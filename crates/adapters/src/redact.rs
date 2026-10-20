//! URL and error-text redaction (AGENTS.md §7; C07). Every URL or error text that may be logged, or
//! that rides a heartbeat as `exitDetail`, passes through `redact` first.
//!
//! The POC's two rules, kept (spec §3.4: "reused as ideas"):
//! 1. everything after `/live/` or `/live2/` is a stream key: replaced by `****`;
//! 2. every query string is a credential carrier (the overlay page token, an SRT stream id or
//!    passphrase): everything from the `?` is replaced by `?****`.
//!
//! Rules 1 and 2 cut at the **earliest** of the markers, wherever it is, and drop everything after
//! it. The POC searched `/live2/` before `/live/` and returned on the first it found, so a text
//! holding a `/live/` key before a `/live2/` one kept the first key (C07, false-premise row). Cutting
//! the whole tail over-redacts an error message, which is the safe direction: web scrubs
//! `exitDetail` the same way ("everything from the first `?`"). Rule 1's markers match in any ASCII
//! case and percent-encoded (`%2Flive%2F`), as a URL quoted inside another or logged encoded shows
//! them (C07 review I3).
//!
//! 3. (C07) `streamid` and `passphrase` values are always redacted, anywhere in the text and in any
//!    ASCII case, not only inside a query: an SRT element's error or property dump names them
//!    without a `?` before them. The key is followed by `\s*[=:]\s*` (`key=value`, gst-launch's
//!    `key = value`, `key: value`), then an optional GStreamer type (`(string)`), then the value: a
//!    quoted one to its closing quote (JSON `"passphrase":"…"`, a multi-word `"two words"`), any
//!    other to the next separator (C07 review I3).
//!
//! A credential that follows no marker at all (`stream 'KEY' publish failed`) is beyond any rule:
//! `Scrubber` masks the values the plane knows, by value, and then applies `redact`.
use std::fmt;

use app::ports::SessionConfig;

/// Rule 1's markers, matched against the ASCII-lowercased text.
const LIVE_PATHS: [&str; 4] = ["/live/", "/live2/", "%2flive%2f", "%2flive2%2f"];
const SRT_SECRET_KEYS: [&str; 2] = ["streamid", "passphrase"];
const MASK: &str = "****";
/// A value shorter than this is never scrubbed by value (the coordinator, 2026-10-19, after web's
/// `MIN_SECRET_LENGTH`): it would mask ordinary words. No credential is that short: an SRT
/// passphrase is 10 to 79 characters, and stream keys and job tokens are far longer.
const MIN_SECRET_LENGTH: usize = 8;

/// `url` (or any text holding URLs) with every stream key, query and SRT credential masked.
pub fn redact(url: &str) -> String {
    // ASCII lowercasing keeps every byte offset, so an index in `lower` is one in `url`.
    let lower = url.to_ascii_lowercase();
    // Rules 1 and 2: the earliest marker wins, and nothing after it survives.
    let mut cut: Option<(usize, usize)> = url.find('?').map(|i| (i, i + 1));
    for path in LIVE_PATHS {
        if let Some(i) = lower.find(path) {
            let keep = i + path.len();
            if cut.is_none_or(|(at, _)| i < at) {
                cut = Some((i, keep));
            }
        }
    }
    let kept = match cut {
        Some((_, keep)) => format!("{}{MASK}", &url[..keep]),
        None => url.to_owned(),
    };
    mask_srt_secrets(&kept)
}

/// Rule 3: every `streamid` / `passphrase` value (ASCII case-insensitive key) becomes `****`.
fn mask_srt_secrets(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        let value = SRT_SECRET_KEYS
            .iter()
            .filter(|k| lower[i..].starts_with(*k))
            .find_map(|k| secret_value(text, i + k.len()));
        match value {
            Some((start, end)) => {
                out.push_str(&text[i..start]);
                if end > start {
                    out.push_str(MASK);
                }
                i = end;
            }
            None => {
                let ch = text[i..]
                    .chars()
                    .next()
                    .expect("i is on a char boundary below len");
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    out
}

/// The value's span after a key that ends at `at`: the key's own closing quote (JSON), `\s*[=:]\s*`,
/// an optional `(type)`, then a quoted value to its closing quote (a backslash escapes one byte; an
/// unclosed quote runs to the end) or a bare one to the next separator. `None` when no `=` or `:`
/// follows: prose that only names the key.
fn secret_value(text: &str, at: usize) -> Option<(usize, usize)> {
    let b = text.as_bytes();
    let mut j = at;
    if matches!(b.get(j), Some(b'"' | b'\'')) {
        j += 1;
    }
    j = skip_space(text, j);
    if !matches!(b.get(j), Some(b'=' | b':')) {
        return None;
    }
    j = skip_space(text, j + 1);
    if b.get(j) == Some(&b'(') {
        if let Some(len) = text[j + 1..].find(')') {
            let ty = &text[j + 1..j + 1 + len];
            if !ty.is_empty() && ty.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
                j = skip_space(text, j + 2 + len);
            }
        }
    }
    match b.get(j) {
        Some(&quote @ (b'"' | b'\'')) => {
            let start = j + 1;
            let mut k = start;
            while k < b.len() && b[k] != quote {
                k += if b[k] == b'\\' { 2 } else { 1 };
            }
            Some((start, k.min(b.len())))
        }
        _ => {
            let end = text[j..].find(is_separator).map_or(text.len(), |n| j + n);
            Some((j, end))
        }
    }
}

/// `from` moved past any whitespace (Unicode, as `\s`).
fn skip_space(text: &str, from: usize) -> usize {
    let rest = &text[from..];
    from + rest.len() - rest.trim_start().len()
}

/// Where a bare `key=value` value ends in a URL or a property dump.
fn is_separator(c: char) -> bool {
    c.is_whitespace() || matches!(c, '&' | '"' | '\'' | ',' | ';' | ')' | ']' | '}' | '>')
}

/// The credential values the control plane knows (C07 review I2, I3): the job token from the start,
/// and once a session is fetched its stream key, SRT stream id and passphrase, and page token.
/// `scrub` masks each value, raw and percent-encoded (upper- and lowercase hex), longest first,
/// and then applies `redact`, so a credential that follows no marker is masked too. Its `Debug`
/// prints how many forms it holds, never one of them.
#[derive(Clone, Default)]
pub struct Scrubber {
    values: Vec<String>,
}

impl Scrubber {
    /// Learn `value` and its percent-encoded forms. A value shorter than `MIN_SECRET_LENGTH`
    /// characters is ignored (an empty one would mask every gap between two characters); the
    /// pattern rules of `redact` still apply to it.
    pub fn add(&mut self, value: &str) {
        if value.chars().count() < MIN_SECRET_LENGTH {
            return;
        }
        for form in [
            value.to_owned(),
            percent_encoded(value, b"0123456789ABCDEF"),
            percent_encoded(value, b"0123456789abcdef"),
        ] {
            if !self.values.contains(&form) {
                self.values.push(form);
            }
        }
        // Longest first: a value that contains another is masked whole.
        self.values.sort_by_key(|v| std::cmp::Reverse(v.len()));
    }

    /// Learn a session's four credentials (C12, lifted from C07's `HttpControlPlane::learn`, which
    /// calls this too): the SRT stream id and passphrase, the RTMPS stream key, and the page token in
    /// `overlay_url`'s query (`st=`; the contract's `pageToken`, not a `SessionConfig` field). One
    /// list for the plane and the binary's log, so they cannot drift.
    pub fn add_session(&mut self, session: &SessionConfig) {
        self.add(&session.input.stream_id);
        self.add(&session.input.passphrase);
        self.add(&session.target.stream_key);
        /*MUT*/ let _ = page_token_in(&session.overlay_url);
    }

    /// `text` with every known value masked, then `redact`ed.
    pub fn scrub(&self, text: &str) -> String {
        let mut out = text.to_owned();
        for v in &self.values {
            out = out.replace(v.as_str(), MASK);
        }
        redact(&out)
    }
}

impl fmt::Debug for Scrubber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Scrubber")
            .field("values", &self.values.len())
            .finish()
    }
}

/// The page token in `overlayUrl`'s query (`st=`), the fourth credential a session carries.
fn page_token_in(overlay_url: &str) -> Option<&str> {
    let (_, query) = overlay_url.split_once('?')?;
    query.split('&').find_map(|kv| kv.strip_prefix("st="))
}

/// RFC 3986 percent-encoding of every byte outside the unreserved set, with this hex alphabet. C10's
/// `srt_uri` encodes the SRT stream id and passphrase with the uppercase alphabet, which `Scrubber::add`
/// also learns, so the scrubber masks exactly the form the URI carries.
pub(crate) fn percent_encoded(value: &str, hex: &[u8; 16]) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(char::from(hex[usize::from(b >> 4)]));
            out.push(char::from(hex[usize::from(b & 0xf)]));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{redact, Scrubber};

    #[test]
    fn a_stream_key_after_live_is_masked() {
        assert_eq!(
            redact("rtmps://live.cloudflare.com:443/live/abc123"),
            "rtmps://live.cloudflare.com:443/live/****"
        );
        assert_eq!(
            redact("rtmp://a.rtmp.youtube.com/live2/abcd-efgh-ijkl"),
            "rtmp://a.rtmp.youtube.com/live2/****"
        );
    }

    #[test]
    fn a_query_is_masked_whole() {
        assert_eq!(redact("srt://h:778?streamid=x&passphrase=y"), "srt://h:778?****");
        assert_eq!(
            redact("https://seazn.club/overlay/fixtures/f?sid=s&st=PAGE-TOKEN&host=compositor&delay=3100"),
            "https://seazn.club/overlay/fixtures/f?****"
        );
    }

    #[test]
    fn a_url_with_no_secret_is_unchanged() {
        for s in [
            "https://seazn.club/api/internal/relay/sessions/3f8a2c1e-7b4d-4e9a-9c2f-5d6e7f8a9b0c/heartbeat",
            "srt://live.cloudflare.com:778",
            "",
            "decoder: not-negotiated",
        ] {
            assert_eq!(redact(s), s);
        }
    }

    // The POC looked for `/live2/` first and stopped there, so an earlier `/live/` key survived.
    #[test]
    fn the_earliest_marker_cuts_whatever_follows_it() {
        assert_eq!(
            redact("a rtmps://h/live/K1 b rtmp://y/live2/K2"),
            "a rtmps://h/live/****"
        );
        assert_eq!(
            redact("a rtmp://y/live2/K2 b rtmps://h/live/K1"),
            "a rtmp://y/live2/****"
        );
        assert_eq!(
            redact("GET https://x/y?st=T1 then rtmps://h/live/K1"),
            "GET https://x/y?****"
        );
        assert_eq!(redact("rtmps://h/live/K1?st=T1"), "rtmps://h/live/****");
    }

    #[test]
    fn srt_credentials_are_masked_outside_a_query_too() {
        assert_eq!(
            redact("srtsrc0: streamid=SID-1 passphrase=PASS-1 failed"),
            "srtsrc0: streamid=**** passphrase=**** failed"
        );
        assert_eq!(
            redact("StreamId=SID-2, PassPhrase='PASS-2'"),
            "StreamId=****, PassPhrase='****'"
        );
        assert_eq!(redact("streamid="), "streamid=", "no value, nothing to mask");
        assert_eq!(
            redact("passphrase=\"\""),
            "passphrase=\"\"",
            "an empty quoted value"
        );
        // Whitespace after the `=` is skipped (GStreamer prints `key = value`), so a key with no value
        // takes the next word as its value: over-masking, the safe direction.
        assert_eq!(redact("streamid= passphrase="), "streamid= ****");
    }

    /// C07 review I3: the reviewer's table, row by row. Each output is written from the rule (the key,
    /// `\s*[=:]\s*`, an optional `(type)`, then the value to its separator or its closing quote), never
    /// from the code. The last row has no marker at all: only `Scrubber` (by value) masks it.
    #[test]
    fn rule_3_masks_every_dump_format_the_reviewer_measured() {
        let rows = [
            // GstStructure serialisation
            (
                "srtsrc0: passphrase=(string)SECRET1 failed",
                "srtsrc0: passphrase=(string)**** failed",
            ),
            (
                "srtsrc0: streamid=(string)SECRET2, mode=caller",
                "srtsrc0: streamid=(string)****, mode=caller",
            ),
            (
                "s: passphrase=(string)\"two words\";",
                "s: passphrase=(string)\"****\";",
            ),
            // gst-launch deep-notify
            (
                "/GstPipeline:pipeline0/GstSRTSrc:srtsrc0: passphrase = SECRET3",
                "/GstPipeline:pipeline0/GstSRTSrc:srtsrc0: passphrase = ****",
            ),
            ("streamid: SECRET4", "streamid: ****"),
            // JSON
            (
                "{\"passphrase\":\"SECRET5\",\"streamId\":\"SECRET6\"}",
                "{\"passphrase\":\"****\",\"streamId\":\"****\"}",
            ),
            ("{\"passphrase\": \"a\\\"b c\"}", "{\"passphrase\": \"****\"}"),
            ("streamid=\tSECRET18", "streamid=\t****"),
            ("passphrase=\"two words SECRET7\"", "passphrase=\"****\""),
            ("passphrase='two words' then", "passphrase='****' then"),
            ("passphrase=\"never closed SECRET19", "passphrase=\"****"),
            // rule 1, percent-encoded and in any case
            (
                "rtmps%3A%2F%2Flive.cloudflare.com%3A443%2Flive%2FSECRET8",
                "rtmps%3A%2F%2Flive.cloudflare.com%3A443%2Flive%2F****",
            ),
            ("a%2flive2%2fSECRET20 b", "a%2flive2%2f****"),
            (
                "rtmps://live.cloudflare.com:443/LIVE/SECRET9",
                "rtmps://live.cloudflare.com:443/LIVE/****",
            ),
            // no marker: by value only
            (
                "stream 'SECRET11' publish failed",
                "stream 'SECRET11' publish failed",
            ),
        ];
        for (input, want) in rows {
            let got = redact(input);
            assert_eq!(got, want, "{input:?}");
            assert_eq!(redact(&got), got, "idempotent on {input:?}");
        }
        eprintln!("reviewer rows checked: {}", rows.len());
        assert_eq!(rows.len(), 15);
    }

    /// C07 review I3 (a): a credential that follows no marker is masked by value, raw and
    /// percent-encoded (upper and lower case hex), and `redact` still runs after it.
    #[test]
    fn a_scrubber_masks_known_values_raw_and_percent_encoded() {
        let mut s = Scrubber::default();
        s.add("SK/1+2=key");
        s.add("PASS-9-SRT");
        s.add("");
        // A value inside another: the longer is masked whole, whichever was learned first.
        s.add("ABCDEFGH");
        s.add("ABCDEFGH-IJ");
        let rows = [
            (
                "stream 'SK/1+2=key' publish failed",
                "stream '****' publish failed",
            ),
            ("key SK%2F1%2B2%3Dkey in a query", "key **** in a query"),
            ("key SK%2f1%2b2%3dkey lower", "key **** lower"),
            ("bare PASS-9-SRT here", "bare **** here"),
            ("PASS-9-SRTPASS-9-SRT", "********"),
            ("rtmps://h/live/OTHER and SK/1+2=key", "rtmps://h/live/****"),
            ("nothing known here", "nothing known here"),
            ("x ABCDEFGH-IJ y ABCDEFGH", "x **** y ****"),
        ];
        for (input, want) in rows {
            assert_eq!(s.scrub(input), want, "{input:?}");
        }
        // An empty value is never a value: it would mask every gap between characters.
        assert_eq!(Scrubber::default().scrub("abc"), "abc");
        let dbg = format!("{s:?}");
        assert!(!dbg.contains("SK/1") && !dbg.contains("PASS-9"), "{dbg}");
        eprintln!("scrubber rows checked: {}", rows.len());
    }

    /// A value shorter than 8 characters is never scrubbed by value (the coordinator, 2026-10-19,
    /// after web's `MIN_SECRET_LENGTH`): it would mask ordinary words, and no credential is that
    /// short (an SRT passphrase is 10 to 79). The pattern rules still apply to it. The length counts
    /// characters, not bytes.
    #[test]
    fn a_value_shorter_than_eight_characters_is_not_scrubbed_by_value() {
        let mut s = Scrubber::default();
        s.add("SEVEN-7");
        s.add("EIGHT-88");
        s.add("P\u{e4}SS-77");
        assert_eq!("SEVEN-7".chars().count(), 7);
        assert_eq!("EIGHT-88".chars().count(), 8);
        assert_eq!(("P\u{e4}SS-77".chars().count(), "P\u{e4}SS-77".len()), (7, 8));
        let rows = [
            ("x SEVEN-7 y", "x SEVEN-7 y"),
            ("x EIGHT-88 y", "x **** y"),
            ("x P\u{e4}SS-77 y", "x P\u{e4}SS-77 y"),
            ("x P%C3%A4SS-77 y", "x P%C3%A4SS-77 y"),
            ("rtmps://h/live/SEVEN-7", "rtmps://h/live/****"),
            ("passphrase=SEVEN-7 next", "passphrase=**** next"),
        ];
        for (input, want) in rows {
            assert_eq!(s.scrub(input), want, "{input:?}");
        }
        eprintln!("minimum-length rows checked: {}", rows.len());
        assert_eq!(rows.len(), 6);
    }

    /// Deterministic fuzz over marker fragments (the reviewer's P4): `redact` never panics, is
    /// idempotent, and nothing after the earliest marker survives.
    #[test]
    fn redact_is_idempotent_and_total_over_marker_fragments() {
        let atoms = [
            "/live/",
            "/LIVE2/",
            "%2Flive%2F",
            "?",
            "streamid",
            "passphrase",
            "PASSPHRASE",
            "=",
            ":",
            "\"",
            "'",
            "\\",
            " ",
            "\t",
            "&",
            "😀",
            "é",
            "/",
            "(",
            ")",
            "(string)",
            "K3Y",
            "\u{a0}",
        ];
        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut checked = 0;
        for _ in 0..50_000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let mut k = seed;
            let mut text = String::new();
            for _ in 0..(seed % 12) {
                k = k
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                text.push_str(atoms[(k >> 33) as usize % atoms.len()]);
            }
            let once = redact(&text);
            assert_eq!(redact(&once), once, "idempotent on {text:?}");
            let lower = text.to_ascii_lowercase();
            let earliest = ["/live/", "/live2/", "%2flive%2f", "?"]
                .iter()
                .filter_map(|m| lower.find(m).map(|i| i + m.len()))
                .min();
            if let Some(after) = earliest {
                if !text[..after].contains("K3Y") && text[after..].contains("K3Y") {
                    assert!(!once.contains("K3Y"), "{text:?} -> {once:?}");
                }
            }
            checked += 1;
        }
        eprintln!("fuzz cases checked: {checked}");
        assert_eq!(checked, 50_000);
    }

    /// Every planted secret, in every carrier the compositor meets, before and after prose, alone and
    /// with others: none survives, and redacting twice changes nothing.
    #[test]
    fn no_planted_secret_survives_any_carrier() {
        let carriers: [fn(&str) -> String; 8] = [
            |s| format!("rtmps://live.cloudflare.com:443/live/{s}"),
            |s| format!("rtmp://a.rtmp.youtube.com/live2/{s}"),
            |s| format!("srt://live.cloudflare.com:778?streamid={s}&passphrase=x"),
            |s| format!("srt://live.cloudflare.com:778?mode=caller&passphrase={s}"),
            |s| format!("https://seazn.club/overlay/fixtures/f?sid=a&st={s}&delay=3100"),
            |s| format!("srtsrc: streamid={s} rejected"),
            |s| format!("passphrase={s}"),
            |s| format!("PASSPHRASE=\"{s}\""),
        ];
        let prose = ["", "push failed: ", "😀 ünïcode ", "a?b "];
        let tails = ["", " (os error 104)", " then rtmps://h/live/OTHER", " ?q=1"];
        let secret = "PLANTED-SECRET-7f3e";
        let mut checked = 0;
        for c in carriers {
            for p in prose {
                for t in tails {
                    let text = format!("{p}{}{t}", c(secret));
                    let once = redact(&text);
                    assert!(!once.contains(secret), "{text:?} → {once:?}");
                    assert!(!once.contains("OTHER"), "{text:?} → {once:?}");
                    assert_eq!(redact(&once), once, "idempotent on {text:?}");
                    checked += 1;
                }
            }
        }
        // libtest captures this on a pass; CI prints it with --show-output (AGENTS.md §5).
        eprintln!("redaction carriers checked: {checked}");
        assert_eq!(checked, 8 * 4 * 4, "anti-vacuity: every carrier × prose × tail");
    }

    /// C12's lifted list: each of a session's four credentials, planted BARE (no `/live/`, no `?`,
    /// no `streamid=`, so `redact` alone cannot see it), is masked once `add_session` learned it, and a
    /// text naming none of them comes back unchanged (the positive pair: nothing else is masked).
    #[test]
    fn add_session_learns_all_four_credentials() {
        use app::ports::{InputSpec, SessionConfig, TargetSpec};
        use domain::config::OutputProfile;
        let session = SessionConfig {
            session_id: "sid-not-a-secret".into(),
            fixture_id: None,
            input: InputSpec {
                url: "srt://h:778".into(),
                stream_id: "PLANTED-STREAM-ID-11".into(),
                passphrase: "PLANTED-PASSPHRASE-22".into(),
            },
            target: TargetSpec {
                url: "rtmps://h:443/live/".into(),
                stream_key: "PLANTED-STREAM-KEY-33".into(),
            },
            output: OutputProfile::P720,
            heartbeat_interval_ms: 5_000,
            overlay_url: "https://h/overlay/fixtures/f?sid=s&st=PLANTED-PAGE-TOKEN-44&delay=3100".into(),
            overlay_delay_ms: 3_100,
        };
        let before = Scrubber::default();
        let mut s = Scrubber::default();
        s.add_session(&session);
        let mut checked = 0;
        for secret in [
            "PLANTED-STREAM-ID-11",
            "PLANTED-PASSPHRASE-22",
            "PLANTED-STREAM-KEY-33",
            "PLANTED-PAGE-TOKEN-44",
        ] {
            let text = format!("push failed: '{secret}' refused");
            assert!(
                before.scrub(&text).contains(secret),
                "redact alone keeps it: the plant is bare"
            );
            assert!(!s.scrub(&text).contains(secret), "{secret} survived add_session");
            checked += 1;
        }
        let plain = "push failed: 'sid-not-a-secret' refused";
        assert_eq!(s.scrub(plain), plain, "nothing else is masked");
        eprintln!("session credentials checked: {checked}");
        assert_eq!(checked, 4);
    }
}
