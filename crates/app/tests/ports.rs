//! The control plane's DTOs (C07): their `Debug` never prints a credential (AGENTS.md §7). A session
//! holds four (the SRT stream id and passphrase, the RTMPS stream key, the page token in the overlay
//! URL's query); C08 sends a `SessionConfig` across a channel whose message derives `Debug`, so a
//! derived `Debug` here would print all four into any log or failing assertion that formats it.
use app::ports::{redacted_origin, InputSpec, SessionConfig, TargetSpec};
use domain::config::OutputProfile;

const KEY: &str = "PLANTED-STREAM-KEY-5e6f";
const SID: &str = "PLANTED-SRT-STREAMID-7a8b";
const PASS: &str = "PLANTED-SRT-PASS-9c0d";
const PAGE: &str = "PLANTED-PAGE-TOKEN-1a2b";
const USER: &str = "PLANTED-USERINFO-3c4d";

fn session(target_url: &str, input_url: &str) -> SessionConfig {
    SessionConfig {
        session_id: "3f8a2c1e-7b4d-4e9a-9c2f-5d6e7f8a9b0c".into(),
        fixture_id: Some("a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d".into()),
        input: InputSpec {
            url: input_url.into(),
            stream_id: SID.into(),
            passphrase: PASS.into(),
        },
        target: TargetSpec {
            url: target_url.into(),
            stream_key: KEY.into(),
        },
        output: OutputProfile::P720,
        heartbeat_interval_ms: 5_000,
        overlay_url: format!(
            "https://seazn.club/overlay/fixtures/f?sid=s&st={PAGE}&host=compositor&delay=3100"
        ),
        overlay_delay_ms: 3_100,
    }
}

/// Every credential, wherever a session carries it (its own field, a key in a URL path, an SRT query
/// on a URL that should have none, userinfo), is missing from `Debug`, plain and pretty.
#[test]
fn a_session_never_prints_its_credentials() {
    let shapes = [
        (
            "rtmps://live.cloudflare.com:443/live/",
            "srt://live.cloudflare.com:778",
        ),
        (
            &*format!("rtmps://live.cloudflare.com:443/live/{KEY}"),
            &*format!("srt://h:778?streamid={SID}&passphrase={PASS}"),
        ),
        (
            &*format!("rtmps://{USER}:{USER}@live.cloudflare.com:443/live/x"),
            "srt://live.cloudflare.com:778#frag",
        ),
        ("not a url", PASS),
    ];
    let mut checked = 0;
    for (target, input) in shapes {
        let s = session(target, input);
        for dbg in [
            format!("{s:?}"),
            format!("{s:#?}"),
            format!("{:?}", s.input),
            format!("{:?}", s.target),
        ] {
            for secret in [KEY, SID, PASS, PAGE, USER] {
                assert!(!dbg.contains(secret), "{secret} in {dbg}");
                checked += 1;
            }
        }
        // What is not a secret still prints, so the Debug stays useful.
        let dbg = format!("{s:?}");
        assert!(dbg.contains("3f8a2c1e-7b4d-4e9a-9c2f-5d6e7f8a9b0c"), "{dbg}");
        assert!(dbg.contains("3100"), "{dbg}");
    }
    eprintln!("credential placements checked: {checked}");
    assert_eq!(checked, 4 * 4 * 5);
}

#[test]
fn redacted_origin_keeps_scheme_and_host_only() {
    let table = [
        ("srt://live.cloudflare.com:778", "srt://live.cloudflare.com:778"),
        (
            "rtmps://live.cloudflare.com:443/live/KEY",
            "rtmps://live.cloudflare.com:443/…",
        ),
        (
            "https://seazn.club/overlay/fixtures/f?st=T",
            "https://seazn.club/…",
        ),
        ("srt://h:778?streamid=S", "srt://h:778/…"),
        ("https://u:p@seazn.club/x", "https://seazn.club/…"),
        ("https://seazn.club#x", "https://seazn.club/…"),
        ("KEY-with-no-scheme", "****"),
        ("", "****"),
    ];
    for (url, want) in table {
        assert_eq!(redacted_origin(url), want, "{url}");
    }
    eprintln!("origins checked: {}", table.len());
}
