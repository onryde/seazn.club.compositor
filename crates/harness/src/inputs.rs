//! The checker's text inputs: the camera's `emit.jsonl`, fake-control's `heartbeats.jsonl`, the
//! score timeline and the compositor's log. Each refuses a malformed line with its file and line
//! number, except a cut-off LAST line, which a process stopped mid-write leaves behind: that one is
//! counted as truncated, never silently read as data.
use std::path::Path;

use serde_json::Value;

/// One line of the camera's `emit.jsonl`: a frame pushed to the encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Emit {
    pub counter: u32,
    pub wall_ms: i64,
}

/// One line of fake-control's `heartbeats.jsonl`.
#[derive(Debug, Clone, PartialEq)]
pub struct Beat {
    pub received_ms: i64,
    pub body: Value,
}

/// The score timeline as fake-control serves it at `/control/timeline`: absolute times.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timeline {
    pub start_wall_ms: i64,
    /// The session's `overlayDelayMs`: the page shows an event at `at + delay_ms`.
    pub delay_ms: i64,
    /// `(index, at)`, in index order.
    pub events: Vec<(u16, i64)>,
}

impl Timeline {
    /// When the page should first show `index`, if the timeline has it.
    pub fn due(&self, index: u16) -> Option<i64> {
        self.events
            .iter()
            .find(|(i, _)| *i == index)
            .map(|(_, at)| at + self.delay_ms)
    }
}

/// Lines read from a JSONL file, and whether its last line was cut off.
#[derive(Debug)]
pub struct Lines<T> {
    pub items: Vec<T>,
    pub truncated: bool,
}

fn read_text(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn jsonl<T>(path: &Path, parse: impl Fn(&Value) -> Option<T>) -> Result<Lines<T>, String> {
    let text = read_text(path)?;
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut items = Vec::with_capacity(lines.len());
    let mut truncated = false;
    for (i, line) in lines.iter().enumerate() {
        match serde_json::from_str::<Value>(line).ok().as_ref().and_then(&parse) {
            Some(item) => items.push(item),
            // A cut-off last line (no newline after it) is a stopped writer, not data.
            None if i + 1 == lines.len() && !text.ends_with('\n') => truncated = true,
            None => return Err(format!("{}:{}: not a valid line", path.display(), i + 1)),
        }
    }
    Ok(Lines { items, truncated })
}

pub fn parse_emit(v: &Value) -> Option<Emit> {
    Some(Emit {
        counter: u32::try_from(v.get("counter")?.as_u64()?).ok()?,
        wall_ms: v.get("wall_ms")?.as_i64()?,
    })
}

pub fn parse_beat(v: &Value) -> Option<Beat> {
    let body = v.get("body").filter(|b| b.is_object())?;
    let received_ms = v.get("received_ms")?.as_i64()?;
    Some(Beat {
        received_ms,
        body: body.clone(),
    })
}

pub fn read_emit(path: &Path) -> Result<Lines<Emit>, String> {
    jsonl(path, parse_emit)
}

pub fn read_beats(path: &Path) -> Result<Lines<Beat>, String> {
    jsonl(path, parse_beat)
}

pub fn parse_timeline(v: &Value) -> Result<Timeline, String> {
    let int = |k: &str| {
        v.get(k)
            .and_then(Value::as_i64)
            .ok_or(format!("timeline: no integer {k}"))
    };
    let events = v
        .get("events")
        .and_then(Value::as_array)
        .ok_or("timeline: no events array")?;
    let mut out = Vec::with_capacity(events.len());
    for e in events {
        let index = e
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|i| u16::try_from(i).ok());
        let at = e.get("at").and_then(Value::as_i64);
        match (index, at) {
            (Some(i), Some(t)) if i > 0 => out.push((i, t)),
            _ => return Err(format!("timeline: a bad event {e}")),
        }
    }
    out.sort_unstable();
    if out.windows(2).any(|w| w[0].0 == w[1].0) {
        return Err("timeline: an index appears twice".into());
    }
    Ok(Timeline {
        start_wall_ms: int("start_wall_ms")?,
        delay_ms: int("delay_ms")?,
        events: out,
    })
}

pub fn read_timeline(path: &Path) -> Result<Timeline, String> {
    let text = read_text(path)?;
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    parse_timeline(&v)
}

/// The compositor log's graphics crashes (`app::driver`'s `cef crashed gen=N`), as epoch ms.
pub fn crash_times(log: &str) -> Vec<i64> {
    log.lines()
        .filter(|l| l.contains(" msg=cef crashed gen="))
        .filter_map(crate::clock::log_ts_ms)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str, text: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("harness-inputs-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, text).unwrap();
        p
    }

    #[test]
    fn emit_lines_read_and_a_cut_last_line_is_counted_not_read() {
        let p = tmp(
            "emit.jsonl",
            "{\"counter\":1,\"wall_ms\":100}\n{\"counter\":2,\"wall_ms\":133}\n{\"coun",
        );
        let l = read_emit(&p).unwrap();
        assert_eq!(
            l.items,
            vec![
                Emit {
                    counter: 1,
                    wall_ms: 100
                },
                Emit {
                    counter: 2,
                    wall_ms: 133
                }
            ]
        );
        assert!(l.truncated);
        let p = tmp(
            "emit2.jsonl",
            "{\"counter\":1,\"wall_ms\":100}\nnot json\n{\"counter\":2,\"wall_ms\":133}\n",
        );
        assert!(
            read_emit(&p).unwrap_err().contains(":2:"),
            "a bad middle line names its line"
        );
        let p = tmp("emit3.jsonl", "");
        let l = read_emit(&p).unwrap();
        assert!(l.items.is_empty() && !l.truncated);
        assert!(read_emit(Path::new("/nonexistent/emit.jsonl")).is_err());
    }

    #[test]
    fn beats_need_a_body_object_and_a_receipt_time() {
        let ok = serde_json::json!({"received_ms": 5, "body": {"state": "playing"}});
        assert_eq!(parse_beat(&ok).unwrap().received_ms, 5);
        assert!(parse_beat(&serde_json::json!({"received_ms": 5, "body": "x"})).is_none());
        assert!(parse_beat(&serde_json::json!({"body": {}})).is_none());
    }

    #[test]
    fn a_timeline_is_sorted_and_refuses_index_0_and_duplicates() {
        let v = serde_json::json!({"start_wall_ms": 1000, "delay_ms": 3100,
            "events": [{"index": 2, "at": 7000}, {"index": 1, "at": 4000}]});
        let t = parse_timeline(&v).unwrap();
        assert_eq!(t.events, vec![(1, 4000), (2, 7000)]);
        assert_eq!(t.due(2), Some(10_100));
        assert_eq!(t.due(3), None);
        let zero = serde_json::json!({"start_wall_ms": 0, "delay_ms": 0, "events": [{"index": 0, "at": 1}]});
        assert!(parse_timeline(&zero).is_err());
        let dup = serde_json::json!({"start_wall_ms": 0, "delay_ms": 0,
            "events": [{"index": 1, "at": 1}, {"index": 1, "at": 2}]});
        assert!(parse_timeline(&dup).is_err());
    }

    #[test]
    fn crash_lines_are_read_by_their_message_only() {
        let log = "ts=2026-10-23T12:00:01.000Z level=info msg=cef ready gen=1\n\
                   ts=2026-10-23T12:00:02.500Z level=warn msg=cef crashed gen=1\n\
                   ts=2026-10-23T12:00:03.000Z level=info msg=page said cef crashed gen=9\n\
                   ts=2026-10-23T12:00:04.000Z level=warn msg=cef crashed gen=2\n";
        let t = crash_times(log);
        assert_eq!(
            t.len(),
            2,
            "a page line that quotes the words is not a crash: {t:?}"
        );
        assert_eq!(t[1] - t[0], 1_500);
    }
}
