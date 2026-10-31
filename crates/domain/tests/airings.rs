//! R4 task SC1: an `AiringLine` exists only within web's bounds (S8's `RelayAiring`, zod 4.4.3), so no
//! line the compositor forwards can make web's strict heartbeat answer 400 (one refused item would
//! be resent, and refused, on every beat after it). Every verdict below is web's, from its declared
//! schema: the lengths are zod's `.max(n)`, which counts UTF-16 code units (`"😀".length` is 2), and
//! each instant's verdict was read off zod's own `z.string().datetime()` by running it (the SC1
//! report pastes that run). None is computed by the code under test.
use domain::airings::{
    is_utc_instant, AiringLine, AiringSource, AiringStyle, LineRefusal, AIRING_BREAK_KIND_MAX_UNITS,
    AIRING_ID_MAX_UNITS,
};

const AT: &str = "2026-10-31T12:00:00.000Z";

fn start(moment: &str, card: &str, break_kind: Option<&str>, at: &str) -> Result<AiringLine, LineRefusal> {
    AiringLine::start(
        moment.to_owned(),
        card.to_owned(),
        AiringStyle::LowerThird,
        AiringSource::Rotation,
        break_kind.map(str::to_owned),
        at.to_owned(),
    )
}

/// Web's bounds, as declared: 80 for the ids, 40 for the break kind.
#[test]
fn the_bounds_are_webs() {
    assert_eq!(AIRING_ID_MAX_UNITS, 80, "S8: momentId, cardId z.string().max(80)");
    assert_eq!(
        AIRING_BREAK_KIND_MAX_UNITS, 40,
        "S8: breakKind z.string().max(40)"
    );
}

/// A start's moment id, card id and break kind, and web's verdict on them.
type Case = (String, String, Option<String>, Result<(), LineRefusal>);

/// Each text bound at its edge, in the unit zod counts. "é" is 1 unit and 2 bytes, so a byte count
/// would refuse 80 of them; "😀" is 2 units and 1 character, so a character count would take 41.
#[test]
fn each_text_bound_is_counted_in_utf16_units_at_its_edge() {
    let a = |n: usize| "a".repeat(n);
    let e_acute = |n: usize| "é".repeat(n);
    let emoji = |n: usize| "😀".repeat(n);
    // (moment, card, breakKind, web's verdict)
    let cases: Vec<Case> = vec![
        (a(80), a(80), Some(a(40)), Ok(())),
        (e_acute(80), e_acute(80), Some(e_acute(40)), Ok(())),
        (emoji(40), emoji(40), Some(emoji(20)), Ok(())),
        (String::new(), String::new(), Some(String::new()), Ok(())),
        (a(1), a(1), None, Ok(())),
        (a(81), a(1), None, Err(LineRefusal::TooLong("momentId"))),
        (e_acute(81), a(1), None, Err(LineRefusal::TooLong("momentId"))),
        (emoji(41), a(1), None, Err(LineRefusal::TooLong("momentId"))),
        (
            format!("{}a", emoji(40)),
            a(1),
            None,
            Err(LineRefusal::TooLong("momentId")),
        ),
        (a(1), a(81), None, Err(LineRefusal::TooLong("cardId"))),
        (a(1), emoji(41), None, Err(LineRefusal::TooLong("cardId"))),
        (a(1), a(1), Some(a(41)), Err(LineRefusal::TooLong("breakKind"))),
        (
            a(1),
            a(1),
            Some(emoji(21)),
            Err(LineRefusal::TooLong("breakKind")),
        ),
        (
            a(1),
            a(1),
            Some(e_acute(41)),
            Err(LineRefusal::TooLong("breakKind")),
        ),
        // The first bound broken, in the spec's key order.
        (a(81), a(81), Some(a(41)), Err(LineRefusal::TooLong("momentId"))),
        (a(1), a(81), Some(a(41)), Err(LineRefusal::TooLong("cardId"))),
    ];
    let mut checked = 0;
    for (moment, card, kind, want) in &cases {
        let got = start(moment, card, kind.as_deref(), AT).map(|_| ());
        assert_eq!(&got, want, "start {moment:?} {card:?} {kind:?}");
        let end = AiringLine::end(moment.clone(), AT.to_owned(), false).map(|_| ());
        let end_want = if moment.encode_utf16().count() > 80 {
            Err(LineRefusal::TooLong("momentId"))
        } else {
            Ok(())
        };
        assert_eq!(end, end_want, "end {moment:?}");
        checked += 1;
    }
    eprintln!("text bounds checked: {checked} starts and their ends");
    assert_eq!(checked, cases.len());
    assert!(checked >= 16, "anti-vacuity");
}

/// zod 4.4.3's `z.string().datetime()` verdict for each, read by running it (the SC1 report).
const INSTANTS: &[(&str, bool)] = &[
    ("2026-10-31T12:00:00.000Z", true),
    ("2026-10-31T12:00:00Z", true),
    ("2026-10-31T12:00Z", true),
    ("2026-10-31T12:00:00.1Z", true),
    ("2026-10-31T12:00:00.123456789Z", true),
    ("2026-10-31T23:59:59.999Z", true),
    ("2026-10-31T00:00:00.000Z", true),
    ("2024-02-29T00:00:00Z", true),
    ("2000-02-29T00:00:00Z", true),
    ("0000-02-29T00:00:00Z", true),
    ("2026-12-31T00:00:00Z", true),
    ("2026-04-30T00:00:00Z", true),
    ("2026-01-31T00:00:00Z", true),
    ("2026-02-28T00:00:00Z", true),
    ("2026-02-29T00:00:00Z", false),
    ("1900-02-29T00:00:00Z", false),
    ("2026-04-31T00:00:00Z", false),
    ("2026-13-01T00:00:00Z", false),
    ("2026-00-01T00:00:00Z", false),
    ("2026-10-00T00:00:00Z", false),
    ("2026-10-32T00:00:00Z", false),
    ("2026-10-31T24:00:00Z", false),
    ("2026-10-31T12:60:00Z", false),
    ("2026-10-31T12:00:60Z", false),
    ("2026-10-31T12:00:00.Z", false),
    ("2026-10-31T12:00:00.000", false),
    ("2026-10-31T12:00:00.000+00:00", false),
    ("2026-10-31T12:00:00.000z", false),
    ("2026-10-31 12:00:00.000Z", false),
    ("2026-10-31t12:00:00.000Z", false),
    ("2026-10-31T12Z", false),
    ("2026-10-31T12:0Z", false),
    ("2026-10-31T12:00:0Z", false),
    ("2026-10-31", false),
    ("26-10-31T12:00:00Z", false),
    ("+2026-10-31T12:00:00Z", false),
    ("２026-10-31T12:00:00Z", false),
    ("2026-10-31T12:00:00.000Z ", false),
    (" 2026-10-31T12:00:00.000Z", false),
    ("", false),
    ("Z", false),
];

#[test]
fn an_instant_is_what_webs_datetime_takes() {
    let mut checked = 0;
    let (mut taken, mut refused) = (0, 0);
    for (s, want) in INSTANTS {
        assert_eq!(is_utc_instant(s), *want, "{s:?}");
        let line = start("m", "c", None, s);
        if *want {
            assert!(line.is_ok(), "{s:?}: {line:?}");
            taken += 1;
        } else {
            assert_eq!(line, Err(LineRefusal::NotAnInstant("at")), "{s:?}");
            assert_eq!(
                AiringLine::end("m".to_owned(), (*s).to_owned(), true),
                Err(LineRefusal::NotAnInstant("at")),
                "{s:?}"
            );
            refused += 1;
        }
        checked += 1;
    }
    eprintln!("instants checked: {checked} ({taken} taken, {refused} refused)");
    assert_eq!(checked, INSTANTS.len());
    assert!(taken >= 10 && refused >= 20, "anti-vacuity: both verdicts");
}

/// The accessors give back what was given.
#[test]
fn a_start_and_an_end_keep_their_values() {
    let Ok(AiringLine::Start(s)) = AiringLine::start(
        "4:sponsor.set_break".to_owned(),
        "card-9".to_owned(),
        AiringStyle::Banner,
        AiringSource::Scene,
        None,
        AT.to_owned(),
    ) else {
        panic!("a start");
    };
    assert_eq!(
        (
            s.moment_id(),
            s.card_id(),
            s.style(),
            s.source(),
            s.break_kind(),
            s.at()
        ),
        (
            "4:sponsor.set_break",
            "card-9",
            AiringStyle::Banner,
            AiringSource::Scene,
            None,
            AT
        )
    );
    let Ok(AiringLine::End(e)) = AiringLine::end("4:sponsor.set_break".to_owned(), AT.to_owned(), true)
    else {
        panic!("an end");
    };
    assert_eq!(
        (e.moment_id(), e.at(), e.cut_short()),
        ("4:sponsor.set_break", AT, true)
    );
}
