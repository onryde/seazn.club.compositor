//! The overlay page's proof-of-play lines (R4 sponsors on stream, task SC1; spec 2026-10-27 §3.6.1-2),
//! on the console channel that carries `seazn:ready` (`cef_graphics::page_signal` hands them here):
//!
//! ```text
//! seazn:sponsor:start {"momentId","cardId","style","source","breakKind","at"}
//! seazn:sponsor:end {"momentId","at","cutShort"}
//! ```
//!
//! The producer is web's `sponsorLine` (`apps/web/src/lib/overlay-compositor.ts`, S7): the signal, one
//! space, then a JSON object with exactly the spec's keys, at most `SPONSOR_LINE_MAX_BYTES` bytes of it.
//! The reader holds it to the same: a body over the bound is refused before it is parsed (bounded
//! work), and so is one that is not a JSON object, or has a key outside the set or one missing, or a
//! value of the wrong type or outside web's bounds (`domain::airings`, which builds the line). Key order
//! is not checked: the page writes the spec's, and web's schema reads keys, not their order. The heartbeat writes
//! the same words back (`style_word`, `source_word`), from the same two tables.
use domain::airings::{AiringLine, AiringSource, AiringStyle, LineRefusal};
use serde_json::{Map, Value};

/// Spec 2026-10-27 §3.6.2, "bounded JSON ≤ 512 bytes"; web's `SPONSOR_LINE_MAX_BYTES` (S7), which counts
/// the body's UTF-8 bytes, the JSON after the signal and its space.
pub const SPONSOR_LINE_MAX_BYTES: usize = 512;

/// The start line's signal and the space before its body (web's `SIGNAL_SPONSOR_START`).
pub const START_SIGNAL: &str = "seazn:sponsor:start ";
/// The end line's signal and the space before its body (web's `SIGNAL_SPONSOR_END`).
pub const END_SIGNAL: &str = "seazn:sponsor:end ";

/// Spec §3.6.1's start keys, in its order (web's `START_KEYS`).
pub const START_KEYS: [&str; 6] = ["momentId", "cardId", "style", "source", "breakKind", "at"];
/// Spec §3.6.1's end keys, in its order (web's `END_KEYS`).
pub const END_KEYS: [&str; 3] = ["momentId", "at", "cutShort"];

/// Each style and its word, both directions (S8: `style: z.enum(["lower_third", "banner"])`).
pub const STYLE_WORDS: [(AiringStyle, &str); 2] = [
    (AiringStyle::LowerThird, "lower_third"),
    (AiringStyle::Banner, "banner"),
];
/// Each source and its word, both directions (S8: `source: z.enum(["rotation", "scene", "operator"])`).
pub const SOURCE_WORDS: [(AiringSource, &str); 3] = [
    (AiringSource::Rotation, "rotation"),
    (AiringSource::Scene, "scene"),
    (AiringSource::Operator, "operator"),
];

pub fn style_word(s: AiringStyle) -> &'static str {
    STYLE_WORDS.iter().find(|(v, _)| *v == s).map_or("", |(_, w)| w)
}

pub fn source_word(s: AiringSource) -> &'static str {
    SOURCE_WORDS.iter().find(|(v, _)| *v == s).map_or("", |(_, w)| w)
}

/// A page message as a sponsor line: `None` when it is not one (no sponsor signal and its space at
/// its start), else the line or why it was refused.
pub fn parse_sponsor_line(message: &str) -> Option<Result<AiringLine, LineRefusal>> {
    if let Some(body) = message.strip_prefix(START_SIGNAL) {
        return Some(object(body, &START_KEYS).and_then(|o| start(&o)));
    }
    let body = message.strip_prefix(END_SIGNAL)?;
    Some(object(body, &END_KEYS).and_then(|o| end(&o)))
}

/// The body as a JSON object with exactly `keys`, its size bounded before it is parsed.
fn object(body: &str, keys: &[&str]) -> Result<Map<String, Value>, LineRefusal> {
    if body.len() > SPONSOR_LINE_MAX_BYTES {
        return Err(LineRefusal::Oversize);
    }
    let value: Value = serde_json::from_str(body).map_err(|_| LineRefusal::NotJson)?;
    let Value::Object(o) = value else {
        return Err(LineRefusal::NotAnObject);
    };
    if o.len() != keys.len() || !keys.iter().all(|k| o.contains_key(*k)) {
        return Err(LineRefusal::KeySet);
    }
    Ok(o)
}

/// `key`'s value. Never an index: serde_json's `Map` index panics on a missing key, and this runs
/// inside GStreamer's log callback on CEF's UI thread, where a panic aborts the process (fix round 1,
/// I1). A missing key is the key set's refusal, whatever `object` let through.
fn field<'a>(o: &'a Map<String, Value>, key: &str) -> Result<&'a Value, LineRefusal> {
    o.get(key).ok_or(LineRefusal::KeySet)
}

fn text(o: &Map<String, Value>, key: &'static str) -> Result<String, LineRefusal> {
    field(o, key)?
        .as_str()
        .map(str::to_owned)
        .ok_or(LineRefusal::WrongType(key))
}

/// A declared word of `table`, or the refusal naming `key`.
fn word<T: Copy>(o: &Map<String, Value>, key: &'static str, table: &[(T, &str)]) -> Result<T, LineRefusal> {
    let w = field(o, key)?.as_str().ok_or(LineRefusal::WrongType(key))?;
    table
        .iter()
        .find(|(_, t)| *t == w)
        .map(|(v, _)| *v)
        .ok_or(LineRefusal::UnknownWord(key))
}

fn start(o: &Map<String, Value>) -> Result<AiringLine, LineRefusal> {
    let moment_id = text(o, "momentId")?;
    let card_id = text(o, "cardId")?;
    let style = word(o, "style", &STYLE_WORDS)?;
    let source = word(o, "source", &SOURCE_WORDS)?;
    let break_kind = match field(o, "breakKind")? {
        Value::Null => None,
        Value::String(k) => Some(k.clone()),
        _ => return Err(LineRefusal::WrongType("breakKind")),
    };
    let at = text(o, "at")?;
    AiringLine::start(moment_id, card_id, style, source, break_kind, at)
}

fn end(o: &Map<String, Value>) -> Result<AiringLine, LineRefusal> {
    let moment_id = text(o, "momentId")?;
    let at = text(o, "at")?;
    let cut_short = field(o, "cutShort")?
        .as_bool()
        .ok_or(LineRefusal::WrongType("cutShort"))?;
    AiringLine::end(moment_id, at, cut_short)
}
