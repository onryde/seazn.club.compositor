//! URL and error-text redaction (AGENTS.md §7; C07). Every URL or error text that may be logged, or
//! that rides a heartbeat as `exitDetail`, passes through `redact` first.
//!
//! The POC's two rules, kept (spec §3.4: "reused as ideas"):
//! 1. everything after `/live/` or `/live2/` is a stream key: replaced by `****`;
//! 2. every query string is a credential carrier (the overlay page token, an SRT stream id or
//!    passphrase): everything from the `?` is replaced by `?****`.
//!
//! Rules 1 and 2 cut at the **earliest** of the three markers, wherever it is, and drop everything
//! after it. The POC searched `/live2/` before `/live/` and returned on the first it found, so a text
//! holding a `/live/` key before a `/live2/` one kept the first key (C07, false-premise row). Cutting
//! the whole tail over-redacts an error message, which is the safe direction: web scrubs
//! `exitDetail` the same way ("everything from the first `?`").
//!
//! 3. (C07) `streamid=` and `passphrase=` values are always redacted, anywhere in the text and in
//!    any case, not only inside a query: an SRT element's property dump or error names them without a
//!    `?` before them, and rule 2 alone would leave them.
const LIVE_PATHS: [&str; 2] = ["/live/", "/live2/"];
const SRT_SECRET_KEYS: [&str; 2] = ["streamid=", "passphrase="];
const MASK: &str = "****";

/// `url` (or any text holding URLs) with every stream key, query and SRT credential masked.
pub fn redact(url: &str) -> String {
    // Rules 1 and 2: the earliest marker wins, and nothing after it survives.
    let mut cut: Option<(usize, usize)> = url.find('?').map(|i| (i, i + 1));
    for path in LIVE_PATHS {
        if let Some(i) = url.find(path) {
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

/// Rule 3: every `streamid=` / `passphrase=` value (ASCII case-insensitive key) up to the next
/// separator becomes `****`.
fn mask_srt_secrets(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        let hit = SRT_SECRET_KEYS.iter().find(|k| lower[i..].starts_with(*k));
        match hit {
            Some(key) => {
                let mut value_at = i + key.len();
                // A quoted value (`passphrase="…"`): the quote stays, its content is the value.
                if text[value_at..].starts_with(['"', '\'']) {
                    value_at += 1;
                }
                out.push_str(&text[i..value_at]);
                let value_len = text[value_at..]
                    .find(is_separator)
                    .unwrap_or(text.len() - value_at);
                if value_len > 0 {
                    out.push_str(MASK);
                }
                i = value_at + value_len;
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

/// Where a `key=value` value ends in a URL or a property dump.
fn is_separator(c: char) -> bool {
    c.is_whitespace() || matches!(c, '&' | '"' | '\'' | ',' | ';' | ')' | ']' | '}' | '>')
}

#[cfg(test)]
mod tests {
    use super::redact;

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
        assert_eq!(
            redact("streamid= passphrase="),
            "streamid= passphrase=",
            "no value, nothing to mask"
        );
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
}
