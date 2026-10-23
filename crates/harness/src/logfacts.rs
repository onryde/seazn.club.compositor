//! The compositor log's facts (C14): the lines the chaos cases judge, parsed into the report so a
//! verdict reads them as data (`report.log`), never by grepping text.
//!
//! The log is `docker logs --timestamps` of the compositor's container (run-case.sh), so a line is
//! `<docker RFC 3339, ns> ts=<RFC 3339, ms> level=<l> msg=<text>` (`compositor::logged`), or the
//! docker stamp and an unformatted text: Chromium's own FATAL lines, bash's job report, CEF's
//! preload lines (Owed 74). Every fact's `at_ms` is the docker stamp when there is one (the clock the
//! docker events and the case's kill stamps share, C14 case 15), else the line's own `ts=`. Across an
//! in-place restart the log holds every process's lines; `boots` marks where each starts.
//!
//! The lines (each read by its exact message shape; a page or a URL that quotes the words is not one):
//! - `app::driver`: `cef ready gen=<g>`, `cef crashed gen=<g>`, `cef alive gen=<g>`,
//!   `overlay hidden reason=<r> gen=<g>`, and C11b's
//!   `cef health zygote_gone=<g|-> leaked_teardowns=<n> gpu_process_deaths=<n>` (on a change only);
//! - `compositor::logged`: `graphics restart gen=<g>`, `sink swap gen=<g> target=<redacted>`,
//!   `programme started`;
//! - `compositor::wiring` and `main`: `compositor started drivers=…` (a boot), `exit code=<n>`;
//! - C11's dispatcher: `gen=<g>: page console: hang starts` (the harness page's hang, its own line)
//!   and `gen=<g>: score stale=<true|false>`;
//! - `docker/supervise.sh`: `the compositor was killed by SIG<name> (status <n>)`;
//! - Chromium, unformatted: `GPU process isn't usable` (its abort, F-013).
use serde::Serialize;

use crate::clock::{log_ts_ms, rfc3339_utc_ms};

/// One line, split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine<'a> {
    /// `docker logs --timestamps`'s stamp, if the line has one.
    pub docker_ms: Option<i64>,
    /// The compositor's own `ts=`, if the line is in its format.
    pub ts_ms: Option<i64>,
    /// `level=`'s word; empty for an unformatted line.
    pub level: &'a str,
    /// `msg=`'s text, or the whole text after the docker stamp for an unformatted line.
    pub msg: &'a str,
    /// Whether the line is in the compositor's `ts= level= msg=` format.
    pub formatted: bool,
}

impl LogLine<'_> {
    /// The docker stamp, else `ts=`.
    pub fn at_ms(&self) -> Option<i64> {
        self.docker_ms.or(self.ts_ms)
    }
}

/// Splits a line. A docker stamp is a first word that parses as RFC 3339 UTC.
pub fn parse_line(line: &str) -> LogLine<'_> {
    let (docker_ms, rest) = match line.split_once(' ') {
        Some((first, rest)) => match rfc3339_utc_ms(first) {
            Some(ms) => (Some(ms), rest),
            None => (None, line),
        },
        None => (
            rfc3339_utc_ms(line),
            if rfc3339_utc_ms(line).is_some() { "" } else { line },
        ),
    };
    if let Some(ts_ms) = log_ts_ms(rest) {
        let after_ts = rest.split_once(' ').map_or("", |(_, r)| r);
        if let Some(lv) = after_ts.strip_prefix("level=") {
            if let Some((level, tail)) = lv.split_once(' ') {
                if let Some(msg) = tail.strip_prefix("msg=") {
                    return LogLine {
                        docker_ms,
                        ts_ms: Some(ts_ms),
                        level,
                        msg,
                        formatted: true,
                    };
                }
            }
        }
    }
    LogLine {
        docker_ms,
        ts_ms: None,
        level: "",
        msg: rest,
        formatted: false,
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct GenAt {
    pub at_ms: i64,
    pub gen: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Hidden {
    pub at_ms: i64,
    pub reason: String,
    pub gen: u64,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct Health {
    pub at_ms: i64,
    /// `zygote_gone=<g>`: the generation a dead zygote was seen in; `None` for `-`.
    pub zygote_gone: Option<u64>,
    pub leaked_teardowns: u64,
    pub gpu_process_deaths: u64,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct Stale {
    pub at_ms: i64,
    pub gen: u64,
    pub value: bool,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct Exit {
    pub at_ms: i64,
    pub code: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Killed {
    pub at_ms: i64,
    pub signal: String,
    pub status: i64,
}

/// The log's facts, in log order.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct LogFacts {
    pub lines: u64,
    /// Lines with a docker stamp (a log read without `--timestamps` has none).
    pub docker_stamped: u64,
    /// Lines not in the compositor's format (Chromium's, bash's, CEF's preload).
    pub unformatted: u64,
    /// Lines with no stamp at all: no fact is taken from them.
    pub unstamped: u64,
    /// `compositor started`: one per process.
    pub boots: Vec<i64>,
    pub programme_started: Vec<i64>,
    pub cef_ready: Vec<GenAt>,
    pub cef_crashed: Vec<GenAt>,
    pub cef_alive: Vec<GenAt>,
    pub graphics_restart: Vec<GenAt>,
    pub overlay_hidden: Vec<Hidden>,
    pub cef_health: Vec<Health>,
    pub sink_swaps: Vec<GenAt>,
    pub page_hangs: Vec<GenAt>,
    pub score_stale: Vec<Stale>,
    /// Chromium's abort line (`GPU process isn't usable`).
    pub gpu_unusable: Vec<i64>,
    pub exits: Vec<Exit>,
    pub killed: Vec<Killed>,
}

fn gen_after(msg: &str, prefix: &str) -> Option<u64> {
    msg.strip_prefix(prefix)?.parse().ok()
}

/// `k=v` tokens of a message, in order.
fn pairs(msg: &str) -> impl Iterator<Item = (&str, &str)> {
    msg.split(' ').filter_map(|t| t.split_once('='))
}

fn health(msg: &str, at_ms: i64) -> Option<Health> {
    let rest = msg.strip_prefix("cef health ")?;
    let mut zygote = None;
    let mut leaked = None;
    let mut gpu = None;
    for (k, v) in pairs(rest) {
        match k {
            "zygote_gone" => zygote = Some(if v == "-" { None } else { Some(v.parse().ok()?) }),
            "leaked_teardowns" => leaked = Some(v.parse().ok()?),
            "gpu_process_deaths" => gpu = Some(v.parse().ok()?),
            _ => return None,
        }
    }
    Some(Health {
        at_ms,
        zygote_gone: zygote?,
        leaked_teardowns: leaked?,
        gpu_process_deaths: gpu?,
    })
}

/// `gen=<g>: <what>` (C11's dispatcher lines): the generation and the text after it.
fn dispatcher(msg: &str) -> Option<(u64, &str)> {
    let rest = msg.strip_prefix("gen=")?;
    let (g, what) = rest.split_once(": ")?;
    Some((g.parse().ok()?, what))
}

fn killed(msg: &str, at_ms: i64) -> Option<Killed> {
    let rest = msg.strip_prefix("the compositor was killed by ")?;
    let (signal, status) = rest.split_once(" (status ")?;
    Some(Killed {
        at_ms,
        signal: signal.to_owned(),
        status: status.strip_suffix(')')?.parse().ok()?,
    })
}

/// Reads every fact of the log.
pub fn log_facts(text: &str) -> LogFacts {
    let mut f = LogFacts::default();
    for raw in text.lines() {
        if raw.trim().is_empty() {
            continue;
        }
        f.lines += 1;
        let l = parse_line(raw);
        if l.docker_ms.is_some() {
            f.docker_stamped += 1;
        }
        if !l.formatted {
            f.unformatted += 1;
        }
        let Some(at_ms) = l.at_ms() else {
            f.unstamped += 1;
            continue;
        };
        let m = l.msg;
        if m.contains("GPU process isn't usable") {
            f.gpu_unusable.push(at_ms);
        }
        if !l.formatted {
            continue;
        }
        let ga = |g| GenAt { at_ms, gen: g };
        if m.starts_with("compositor started ") {
            f.boots.push(at_ms);
        } else if m == "programme started" {
            f.programme_started.push(at_ms);
        } else if let Some(g) = gen_after(m, "cef ready gen=") {
            f.cef_ready.push(ga(g));
        } else if let Some(g) = gen_after(m, "cef crashed gen=") {
            f.cef_crashed.push(ga(g));
        } else if let Some(g) = gen_after(m, "cef alive gen=") {
            f.cef_alive.push(ga(g));
        } else if let Some(g) = gen_after(m, "graphics restart gen=") {
            f.graphics_restart.push(ga(g));
        } else if let Some(rest) = m.strip_prefix("overlay hidden reason=") {
            if let Some((reason, g)) = rest.split_once(" gen=") {
                if let Ok(g) = g.parse() {
                    f.overlay_hidden.push(Hidden {
                        at_ms,
                        reason: reason.to_owned(),
                        gen: g,
                    });
                }
            }
        } else if let Some(h) = health(m, at_ms) {
            f.cef_health.push(h);
        } else if let Some(rest) = m.strip_prefix("sink swap gen=") {
            if let Some(Ok(g)) = rest.split(' ').next().map(str::parse) {
                f.sink_swaps.push(ga(g));
            }
        } else if let Some(code) = gen_after(m, "exit code=") {
            f.exits.push(Exit {
                at_ms,
                code: code as i64,
            });
        } else if let Some(k) = killed(m, at_ms) {
            f.killed.push(k);
        } else if let Some((g, what)) = dispatcher(m) {
            match what {
                "page console: hang starts" => f.page_hangs.push(ga(g)),
                "score stale=true" => f.score_stale.push(Stale {
                    at_ms,
                    gen: g,
                    value: true,
                }),
                "score stale=false" => f.score_stale.push(Stale {
                    at_ms,
                    gen: g,
                    value: false,
                }),
                _ => {}
            }
        }
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    const D: &str = "2026-10-23T12:00:00.500000000Z";
    // 2026-10-23T12:00:00Z from the calendar (clock.rs's tests: day 20 749).
    const NOON_MS: i64 = (20_749 * 86_400 + 12 * 3_600) * 1_000;

    #[test]
    fn a_docker_stamp_is_preferred_and_both_forms_split() {
        let text = format!("{D} ts=2026-10-23T12:00:00.123Z level=info msg=cef ready gen=1");
        let l = parse_line(&text);
        assert_eq!(l.docker_ms, Some(NOON_MS + 500));
        assert_eq!(l.ts_ms, Some(NOON_MS + 123));
        assert_eq!(l.at_ms(), Some(NOON_MS + 500));
        assert_eq!((l.level, l.msg, l.formatted), ("info", "cef ready gen=1", true));
        let bare = parse_line("ts=2026-10-23T12:00:00.123Z level=warn msg=x y");
        assert_eq!((bare.docker_ms, bare.at_ms()), (None, Some(NOON_MS + 123)));
        assert_eq!(bare.msg, "x y");
        let text = format!("{D} [1023/120000.5:FATAL:gpu.cc(1)] GPU process isn't usable. Goodbye.");
        let chromium = parse_line(&text);
        assert!(!chromium.formatted);
        assert_eq!(chromium.at_ms(), Some(NOON_MS + 500));
        assert!(chromium.msg.starts_with("[1023"));
        let none = parse_line("no stamp at all");
        assert_eq!((none.at_ms(), none.formatted), (None, false));
    }

    fn line(ms_after_noon: i64, msg: &str) -> String {
        let s = ms_after_noon / 1000;
        let ms = ms_after_noon % 1000;
        format!(
            "2026-10-23T12:{:02}:{:02}.{ms:03}000000Z ts=2026-10-23T12:00:00.000Z level=info msg={msg}\n",
            s / 60,
            s % 60
        )
    }

    #[test]
    fn every_fact_is_read_by_its_exact_shape() {
        let mut log = String::new();
        let lines = [
            (
                0,
                "compositor started drivers=live deadline=2026-10-23T12:15:00.000Z",
            ),
            (100, "programme started"),
            (1_000, "cef ready gen=1"),
            (3_000, "cef alive gen=1"),
            (5_000, "cef alive gen=1"),
            (6_000, "gen=1: page console: hang starts"),
            (6_500, "gen=1: score stale=true"),
            (16_000, "overlay hidden reason=hang gen=1"),
            (17_000, "graphics restart gen=2"),
            (19_000, "cef ready gen=2"),
            (
                20_000,
                "cef health zygote_gone=- leaked_teardowns=0 gpu_process_deaths=1",
            ),
            (
                21_000,
                "cef health zygote_gone=2 leaked_teardowns=0 gpu_process_deaths=1",
            ),
            (
                22_000,
                "sink swap gen=2 target=rtmp://mediamtx:1935/live/[redacted]",
            ),
            (23_000, "cef crashed gen=2"),
            (24_000, "exit code=75"),
            (25_000, "the compositor was killed by SIGTRAP (status 133)"),
            // Near misses: none is a fact.
            (26_000, "gen=1: page console: page said cef ready gen=9"),
            (26_100, "page url http://x/?q=cef crashed gen=3"),
            (
                26_200,
                "cef health zygote_gone=- leaked_teardowns=0 gpu_process_deaths=1 extra=1",
            ),
            (26_300, "overlay hidden reason=hang gen=x"),
            (26_400, "cef ready gen=1 (retired)"),
        ];
        for (at, m) in lines {
            log.push_str(&line(at, m));
        }
        log.push_str(&format!("{D} [FATAL] GPU process isn't usable. Goodbye.\n"));
        log.push_str("/entrypoint.sh: line 43: 17 Trace/breakpoint trap\n");
        let f = log_facts(&log);
        assert_eq!(f.lines, lines.len() as u64 + 2);
        assert_eq!(
            (f.docker_stamped, f.unformatted, f.unstamped),
            (f.lines - 1, 2, 1)
        );
        assert_eq!(f.boots, vec![NOON_MS]);
        assert_eq!(f.programme_started, vec![NOON_MS + 100]);
        assert_eq!(f.cef_ready.iter().map(|g| g.gen).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(f.cef_alive.len(), 2);
        assert_eq!(
            f.page_hangs,
            vec![GenAt {
                at_ms: NOON_MS + 6_000,
                gen: 1
            }]
        );
        assert_eq!(
            f.score_stale,
            vec![Stale {
                at_ms: NOON_MS + 6_500,
                gen: 1,
                value: true
            }]
        );
        assert_eq!(
            f.overlay_hidden,
            vec![Hidden {
                at_ms: NOON_MS + 16_000,
                reason: "hang".into(),
                gen: 1
            }]
        );
        assert_eq!(
            f.graphics_restart,
            vec![GenAt {
                at_ms: NOON_MS + 17_000,
                gen: 2
            }]
        );
        assert_eq!(
            f.cef_health.len(),
            2,
            "the line with an unknown key is not a health line"
        );
        assert_eq!(f.cef_health[0].zygote_gone, None);
        assert_eq!(f.cef_health[1].zygote_gone, Some(2));
        assert_eq!(f.cef_health[1].gpu_process_deaths, 1);
        assert_eq!(
            f.sink_swaps,
            vec![GenAt {
                at_ms: NOON_MS + 22_000,
                gen: 2
            }]
        );
        assert_eq!(f.cef_crashed.len(), 1, "a URL quoting the words is not a crash");
        assert_eq!(
            f.exits,
            vec![Exit {
                at_ms: NOON_MS + 24_000,
                code: 75
            }]
        );
        assert_eq!(f.killed[0].signal, "SIGTRAP");
        assert_eq!(f.killed[0].status, 133);
        assert_eq!(f.gpu_unusable, vec![NOON_MS + 500]);
        eprintln!("log lines checked: {}", f.lines);
    }

    #[test]
    fn an_empty_log_has_no_facts() {
        let f = log_facts("");
        assert_eq!(f, LogFacts::default());
        assert_eq!(f.lines, 0);
    }
}
