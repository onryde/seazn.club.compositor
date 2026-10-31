//! R4 task SC1: the overlay page's proof-of-play lines (spec 2026-10-27 §3.6.1-2), read as web's
//! producer writes them (`sponsorLine`, `apps/web/src/lib/overlay-compositor.ts`, S7: the signal, one
//! space, a JSON object with exactly the spec's keys, at most 512 bytes) and refused as web's strict
//! heartbeat would refuse them (S8's `RelayAiring`). Every verdict is the spec's or S8's, written here.
use adapters::sponsor_line::{
    parse_sponsor_line, source_word, style_word, END_KEYS, SOURCE_WORDS, SPONSOR_LINE_MAX_BYTES, START_KEYS,
    STYLE_WORDS,
};
use domain::airings::{AiringLine, AiringSource, AiringStyle, LineRefusal};
use serde_json::{json, Value};

const AT: &str = "2026-10-31T12:00:00.000Z";

fn start_body() -> Value {
    json!({
        "momentId": "12:sponsor.set_break",
        "cardId": "0b8c2a3e-1f4d-4c6b-9a7e-5d2f8e1c3b4a",
        "style": "lower_third",
        "source": "rotation",
        "breakKind": "set_break",
        "at": AT,
    })
}

fn end_body() -> Value {
    json!({ "momentId": "12:sponsor.set_break", "at": AT, "cutShort": false })
}

fn start_line(body: &Value) -> String {
    format!("seazn:sponsor:start {body}")
}

fn end_line(body: &Value) -> String {
    format!("seazn:sponsor:end {body}")
}

fn with(mut body: Value, key: &str, v: Value) -> Value {
    body[key] = v;
    body
}

fn without(mut body: Value, key: &str) -> Value {
    body.as_object_mut().unwrap().remove(key);
    body
}

/// Spec §3.6.1's key sets, as web's producer writes them.
#[test]
fn the_key_sets_are_the_specs() {
    assert_eq!(
        START_KEYS,
        ["momentId", "cardId", "style", "source", "breakKind", "at"]
    );
    assert_eq!(END_KEYS, ["momentId", "at", "cutShort"]);
    assert_eq!(
        SPONSOR_LINE_MAX_BYTES, 512,
        "spec §3.6.2, bounded JSON ≤ 512 bytes"
    );
}

#[test]
fn a_rotation_start_a_scene_start_and_an_end_are_read() {
    let Some(Ok(AiringLine::Start(s))) = parse_sponsor_line(&start_line(&start_body())) else {
        panic!("a rotation start");
    };
    assert_eq!(s.moment_id(), "12:sponsor.set_break");
    assert_eq!(s.card_id(), "0b8c2a3e-1f4d-4c6b-9a7e-5d2f8e1c3b4a");
    assert_eq!(
        (s.style(), s.source()),
        (AiringStyle::LowerThird, AiringSource::Rotation)
    );
    assert_eq!((s.break_kind(), s.at()), (Some("set_break"), AT));
    // Web's `carouselLines`: a scene start carries source "scene" and breakKind null.
    let scene = with(
        with(
            with(start_body(), "source", json!("scene")),
            "breakKind",
            Value::Null,
        ),
        "style",
        json!("banner"),
    );
    let Some(Ok(AiringLine::Start(s))) = parse_sponsor_line(&start_line(&scene)) else {
        panic!("a scene start");
    };
    assert_eq!(
        (s.style(), s.source(), s.break_kind()),
        (AiringStyle::Banner, AiringSource::Scene, None)
    );
    // "operator" is a word S8 declares; web sets it, and the reader takes it too.
    let operator = with(start_body(), "source", json!("operator"));
    assert!(matches!(
        parse_sponsor_line(&start_line(&operator)),
        Some(Ok(AiringLine::Start(s))) if s.source() == AiringSource::Operator
    ));
    let Some(Ok(AiringLine::End(e))) =
        parse_sponsor_line(&end_line(&with(end_body(), "cutShort", json!(true))))
    else {
        panic!("an end");
    };
    assert_eq!(
        (e.moment_id(), e.at(), e.cut_short()),
        ("12:sponsor.set_break", AT, true)
    );
}

/// The page writes the spec's order; the reader reads keys, not their order.
#[test]
fn key_order_is_not_read() {
    let reordered = r#"seazn:sponsor:end {"cutShort":false,"at":"2026-10-31T12:00:00.000Z","momentId":"m"}"#;
    assert!(matches!(
        parse_sponsor_line(reordered),
        Some(Ok(AiringLine::End(_)))
    ));
}

#[test]
fn bad_json_oversize_and_unknown_key_are_refused() {
    let pad = |body: &Value, total: usize| {
        // JSON whitespace before the closing brace: still the same object, `total` bytes long.
        let text = body.to_string();
        let open = &text[..text.len() - 1];
        format!("{open}{}}}", " ".repeat(total - text.len()))
    };
    let at_bound = pad(&start_body(), SPONSOR_LINE_MAX_BYTES);
    assert_eq!(at_bound.len(), 512);
    let over_bound = pad(&start_body(), SPONSOR_LINE_MAX_BYTES + 1);
    let cases: Vec<(String, LineRefusal)> = vec![
        // Oversize: 513 bytes, refused before any parse (a valid object, or none at all).
        (format!("seazn:sponsor:start {over_bound}"), LineRefusal::Oversize),
        (
            format!("seazn:sponsor:start {}", "[".repeat(100_000)),
            LineRefusal::Oversize,
        ),
        (
            format!("seazn:sponsor:end {}", "é".repeat(257)),
            LineRefusal::Oversize,
        ),
        // Not JSON, or not an object.
        ("seazn:sponsor:start {not json".to_owned(), LineRefusal::NotJson),
        ("seazn:sponsor:start ".to_owned(), LineRefusal::NotJson),
        (
            format!("{} trailing", start_line(&start_body())),
            LineRefusal::NotJson,
        ),
        ("seazn:sponsor:start [1,2]".to_owned(), LineRefusal::NotAnObject),
        (
            "seazn:sponsor:end \"momentId\"".to_owned(),
            LineRefusal::NotAnObject,
        ),
        ("seazn:sponsor:end null".to_owned(), LineRefusal::NotAnObject),
        // A key outside the spec's set (the compositor adds `kind`; the page never sends it), or one
        // missing (breakKind is nullable, never absent), or the other kind's keys.
        (
            start_line(&with(start_body(), "kind", json!("start"))),
            LineRefusal::KeySet,
        ),
        (
            end_line(&with(end_body(), "extra", json!(1))),
            LineRefusal::KeySet,
        ),
        (
            start_line(&without(start_body(), "breakKind")),
            LineRefusal::KeySet,
        ),
        (end_line(&without(end_body(), "cutShort")), LineRefusal::KeySet),
        (start_line(&end_body()), LineRefusal::KeySet),
        (end_line(&start_body()), LineRefusal::KeySet),
        ("seazn:sponsor:end {}".to_owned(), LineRefusal::KeySet),
        // A value of the wrong JSON type.
        (
            start_line(&with(start_body(), "momentId", json!(12))),
            LineRefusal::WrongType("momentId"),
        ),
        (
            start_line(&with(start_body(), "cardId", Value::Null)),
            LineRefusal::WrongType("cardId"),
        ),
        (
            start_line(&with(start_body(), "style", json!(1))),
            LineRefusal::WrongType("style"),
        ),
        (
            start_line(&with(start_body(), "source", json!([]))),
            LineRefusal::WrongType("source"),
        ),
        (
            start_line(&with(start_body(), "breakKind", json!(5))),
            LineRefusal::WrongType("breakKind"),
        ),
        (
            start_line(&with(start_body(), "at", json!(1_793_000_000_000u64))),
            LineRefusal::WrongType("at"),
        ),
        (
            end_line(&with(end_body(), "cutShort", json!("false"))),
            LineRefusal::WrongType("cutShort"),
        ),
        (
            end_line(&with(end_body(), "momentId", json!({}))),
            LineRefusal::WrongType("momentId"),
        ),
        (
            end_line(&with(end_body(), "at", Value::Null)),
            LineRefusal::WrongType("at"),
        ),
        // A word S8 does not declare (exact: no case folding).
        (
            start_line(&with(start_body(), "style", json!("Banner"))),
            LineRefusal::UnknownWord("style"),
        ),
        (
            start_line(&with(start_body(), "style", json!("big"))),
            LineRefusal::UnknownWord("style"),
        ),
        (
            start_line(&with(start_body(), "source", json!("manual"))),
            LineRefusal::UnknownWord("source"),
        ),
        // Past web's bounds.
        (
            start_line(&with(start_body(), "momentId", json!("m".repeat(81)))),
            LineRefusal::TooLong("momentId"),
        ),
        (
            start_line(&with(start_body(), "breakKind", json!("b".repeat(41)))),
            LineRefusal::TooLong("breakKind"),
        ),
        (
            end_line(&with(end_body(), "at", json!("yesterday"))),
            LineRefusal::NotAnInstant("at"),
        ),
        (
            start_line(&with(start_body(), "at", json!("2026-10-31T12:00:00+01:00"))),
            LineRefusal::NotAnInstant("at"),
        ),
    ];
    let mut checked = 0;
    for (line, want) in &cases {
        let got = parse_sponsor_line(line);
        assert_eq!(got, Some(Err(*want)), "{}", &line[..line.len().min(120)]);
        checked += 1;
    }
    // At the bound exactly: read.
    assert!(
        matches!(
            parse_sponsor_line(&format!("seazn:sponsor:start {at_bound}")),
            Some(Ok(_))
        ),
        "512 bytes is within the bound"
    );
    checked += 1;
    eprintln!("sponsor lines refused or bounded: {checked}");
    assert_eq!(checked, cases.len() + 1);
    assert!(checked >= 30, "anti-vacuity");
}

/// A message that is not a sponsor line is none of the reader's business: `page_signal` maps it.
#[test]
fn a_message_that_is_not_a_sponsor_line_is_none() {
    let body = start_body().to_string();
    let cases = [
        "seazn:sponsor:start".to_owned(),
        "seazn:sponsor:end".to_owned(),
        format!("seazn:sponsor:start{body}"),
        format!("seazn:sponsor:startx {body}"),
        format!("seazn:sponsor:middle {body}"),
        format!(" seazn:sponsor:start {body}"),
        format!("SEAZN:sponsor:start {body}"),
        "seazn:ready".to_owned(),
        String::new(),
    ];
    for m in &cases {
        assert_eq!(parse_sponsor_line(m), None, "{m:?}");
    }
    eprintln!("non-sponsor messages checked: {}", cases.len());
    assert_eq!(cases.len(), 9);
}

/// The two word tables are complete (an exhaustive match: a new variant fails to compile), one word
/// each, and read back to the same variant through the reader.
#[test]
fn each_style_and_source_has_one_word_and_reads_back() {
    let styles = [AiringStyle::LowerThird, AiringStyle::Banner];
    for s in styles {
        match s {
            AiringStyle::LowerThird | AiringStyle::Banner => {}
        }
    }
    let sources = [
        AiringSource::Rotation,
        AiringSource::Scene,
        AiringSource::Operator,
    ];
    for s in sources {
        match s {
            AiringSource::Rotation | AiringSource::Scene | AiringSource::Operator => {}
        }
    }
    assert_eq!(STYLE_WORDS.len(), styles.len());
    assert_eq!(SOURCE_WORDS.len(), sources.len());
    let mut checked = 0;
    for style in styles {
        for source in sources {
            let body = with(
                with(start_body(), "style", json!(style_word(style))),
                "source",
                json!(source_word(source)),
            );
            let Some(Ok(AiringLine::Start(s))) = parse_sponsor_line(&start_line(&body)) else {
                panic!("{style:?} {source:?}");
            };
            assert_eq!((s.style(), s.source()), (style, source));
            checked += 1;
        }
    }
    let words: Vec<&str> = STYLE_WORDS
        .iter()
        .map(|(_, w)| *w)
        .chain(SOURCE_WORDS.iter().map(|(_, w)| *w))
        .collect();
    assert_eq!(
        words,
        ["lower_third", "banner", "rotation", "scene", "operator"],
        "S8's words"
    );
    eprintln!("style and source pairs read back: {checked}");
    assert_eq!(checked, 6);
}
