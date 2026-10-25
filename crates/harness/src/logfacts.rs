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
//!   `programme started`; and `app::driver`'s `sink swap reason=<r> gen=<g>` just before the swap
//!   line (Owed 87, PR #57);
//! - `adapters::gst_output`: `output gen=<g> error: <text>` (a sink's bus error);
//! - `compositor::wiring` and `main`: `compositor started drivers=…` (a boot), `exit code=<n>`;
//! - C11's dispatcher: `gen=<g>: page console: hang starts` (the harness page's hang, its own line)
//!   and `gen=<g>: score stale=<true|false>`;
//! - `docker/supervise.sh`: `the compositor was killed by SIG<name> (status <n>)`;
//! - Chromium, unformatted: `GPU process isn't usable` (its abort, F-013);
//! - `adapters::cef_graphics` (C14 case 16, Owed 82): each build's `gen=<g>: loading <origin>`, and
//!   from a `test-hooks` build the arming line `WARN test hook (Owed 82): the next <n> CEF teardowns
//!   never return` and each held teardown's `WARN test hook (Owed 82) gen=<g>: this CEF teardown
//!   never returns (<left> more to hold)`; the reaper's `WARN gen=<g>: teardown has not returned
//!   after <ms> ms; SIGKILL sent to renderer pids [..] (of [..])` and its leak count
//!   `WARN gen=<g>: teardown still has not returned <ms> ms after the kill: … (<n> leaked; Owed 55)`;
//! - `adapters::gst_programme` (C14 case 18, Owed 94), from a `test-hooks` build: the arming line
//!   `WARN test hook (Owed 94): sink gen=1 stalls (<hold|hang>) <ms> ms after its build` and the
//!   plant `WARN test hook (Owed 94): sink stall=<hold|hang> gen=1 planted=<true|false>`.
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
    /// The line's stamp ([`LogLine::at_ms`]): docker's, else `ts=`.
    pub at_ms: i64,
    /// The process's own `ts=` (every formatted line has one): the clock its timers run on, so an
    /// interval the product counts (a back-off) is judged on it. Docker stamps each line as it reads
    /// it, one by one, so two of its stamps can sit closer than the product put the lines (the PR
    /// #73 confirm pass's b-1: 499.38 ms for a 500 ms back-off).
    pub ts_ms: i64,
    pub gen: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Hidden {
    pub at_ms: i64,
    pub reason: String,
    pub gen: u64,
}

/// `sink swap reason=<r> gen=<g>`: why generation g was built (the domain's `SwapReason`, named
/// `first`, `error`, `connect_timeout` or `egress_stalled` (Owed 94) by the driver; Owed 87 retired
/// `stall`).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SwapReasonLine {
    pub at_ms: i64,
    pub gen: u64,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct Health {
    pub at_ms: i64,
    /// `zygote_gone=<g>`: the generation a dead zygote was seen in; `None` for `-`.
    pub zygote_gone: Option<u64>,
    pub leaked_teardowns: u64,
    pub gpu_process_deaths: u64,
}

/// `WARN test hook (Owed 82): the next <n> CEF teardowns never return`: a `test-hooks` build armed
/// (once per process, before `compositor started`).
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct Armed {
    pub at_ms: i64,
    pub n: u64,
}

/// `WARN test hook (Owed 82) gen=<g>: this CEF teardown never returns (<left> more to hold)`: one
/// retirement held for good.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct HeldTeardown {
    pub at_ms: i64,
    pub gen: u64,
    pub left: u64,
}

/// `WARN test hook (Owed 94): sink gen=1 stalls (<hold|hang>) <ms> ms after its build`: a
/// `test-hooks` build armed the output-hung stall (case 18's precondition).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SinkStallArmed {
    pub at_ms: i64,
    pub stall: String,
    pub after_ms: u64,
}

/// `WARN test hook (Owed 94): sink stall=<hold|hang> gen=1 planted=<true|false>`: the stall's moment
/// came, and whether it went in on sink 1. Case 18's fault, timed on `ts_ms`, the process's own
/// stamp, as the swap it causes is (both are the product's intervals; [`GenAt::ts_ms`]).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SinkStallPlanted {
    pub at_ms: i64,
    pub ts_ms: i64,
    pub stall: String,
    pub planted: bool,
}

/// `WARN gen=<g>: teardown has not returned after <ms> ms; SIGKILL sent to renderer pids [..] (of
/// [..])`: the reaper's kill of a retired build's renderers, the pids it signalled of those it found.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReaperKill {
    pub at_ms: i64,
    pub gen: u64,
    pub after_ms: u64,
    pub killed: Vec<u64>,
    pub of: Vec<u64>,
}

/// `WARN gen=<g>: teardown still has not returned <ms> ms after the kill: … (<n> leaked; Owed 55)`:
/// the teardown counted leaked, and how many are leaked at once.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct TeardownLeak {
    pub at_ms: i64,
    pub gen: u64,
    pub leaked: u64,
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
    /// `sink swap reason=<r> gen=<g>`, one per swap line (lib.jq's `stall_swap` checks they pair).
    pub sink_swap_reasons: Vec<SwapReasonLine>,
    /// `output gen=<g> error: …`: a sink's bus error, logged before its `OutputError` and never once
    /// the sink is retired (gst_output.rs's `Watch`), so a swap whose outgoing sink logged none was
    /// not the error path's (C14 fix round 3).
    pub output_errors: Vec<GenAt>,
    /// `output gen=<g>: the sink can send after <n> ms; …` (gst_output.rs's publish gate, Owed 84):
    /// the sink's publish was accepted, so it sends from here (case 17's measure, the PR #65
    /// review's m-4: a publish under a trickle is Owed 88's write-stall path).
    pub sink_publishes: Vec<GenAt>,
    pub page_hangs: Vec<GenAt>,
    pub score_stale: Vec<Stale>,
    /// `gen=<g>: loading <origin>`: each build's load, so a case can tell the newest gen (case 16).
    pub cef_loading: Vec<GenAt>,
    /// Owed 82's arming line (a `test-hooks` build; case 16's precondition).
    pub hold_armed: Vec<Armed>,
    /// Each teardown the hook held (case 16).
    pub teardowns_held: Vec<HeldTeardown>,
    /// The reaper's kill of each retired build whose teardown had not returned.
    pub reaper_kills: Vec<ReaperKill>,
    /// Each teardown counted leaked, with the count at once.
    pub teardown_leaks: Vec<TeardownLeak>,
    /// Owed 94's arming line (a `test-hooks` build; case 18's precondition).
    pub sink_stall_armed: Vec<SinkStallArmed>,
    /// Owed 94's plant (case 18's fault).
    pub sink_stall_planted: Vec<SinkStallPlanted>,
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

/// `sink swap reason=<r> gen=<g>`: the whole message, one word for the reason.
fn swap_reason(msg: &str, at_ms: i64) -> Option<SwapReasonLine> {
    let (reason, g) = msg.strip_prefix("sink swap reason=")?.split_once(" gen=")?;
    if reason.is_empty() || reason.contains(' ') {
        return None;
    }
    Some(SwapReasonLine {
        at_ms,
        gen: g.parse().ok()?,
        reason: reason.to_owned(),
    })
}

/// `output gen=<g>: the sink can send after <n> ms; …`: the generation, with the wait a number.
fn sink_publish(msg: &str) -> Option<u64> {
    let (g, rest) = msg
        .strip_prefix("output gen=")?
        .split_once(": the sink can send after ")?;
    let (ms, _) = rest.split_once(" ms; ")?;
    ms.parse::<u64>().ok()?;
    g.parse().ok()
}

/// Digits only (`u64::from_str` also takes a leading `+`).
fn num(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// `[a, b]` as Rust's `{:?}` prints a `Vec<i32>` of pids; `[]` is empty.
fn pid_list(s: &str) -> Option<Vec<u64>> {
    let inner = s.strip_prefix('[')?.strip_suffix(']')?;
    if inner.is_empty() {
        return Some(vec![]);
    }
    inner.split(", ").map(num).collect()
}

fn armed(msg: &str, at_ms: i64) -> Option<Armed> {
    let n = msg
        .strip_prefix("WARN test hook (Owed 82): the next ")?
        .strip_suffix(" CEF teardowns never return")?;
    Some(Armed { at_ms, n: num(n)? })
}

/// The stall's word, `hold` or `hang` (gst_programme.rs's `stall_name`).
fn stall_word(s: &str) -> Option<String> {
    matches!(s, "hold" | "hang").then(|| s.to_owned())
}

fn sink_stall_armed(msg: &str, at_ms: i64) -> Option<SinkStallArmed> {
    let (stall, rest) = msg
        .strip_prefix("WARN test hook (Owed 94): sink gen=1 stalls (")?
        .split_once(") ")?;
    let ms = rest.strip_suffix(" ms after its build")?;
    Some(SinkStallArmed {
        at_ms,
        stall: stall_word(stall)?,
        after_ms: num(ms)?,
    })
}

fn sink_stall_planted(msg: &str, at_ms: i64, ts_ms: i64) -> Option<SinkStallPlanted> {
    let (stall, planted) = msg
        .strip_prefix("WARN test hook (Owed 94): sink stall=")?
        .split_once(" gen=1 planted=")?;
    Some(SinkStallPlanted {
        at_ms,
        ts_ms,
        stall: stall_word(stall)?,
        planted: match planted {
            "true" => true,
            "false" => false,
            _ => return None,
        },
    })
}

fn held(msg: &str, at_ms: i64) -> Option<HeldTeardown> {
    let (g, rest) = msg
        .strip_prefix("WARN test hook (Owed 82) gen=")?
        .split_once(": this CEF teardown never returns (")?;
    let left = rest.strip_suffix(" more to hold)")?;
    Some(HeldTeardown {
        at_ms,
        gen: num(g)?,
        left: num(left)?,
    })
}

fn reaper_kill(msg: &str, at_ms: i64) -> Option<ReaperKill> {
    let (g, rest) = msg
        .strip_prefix("WARN gen=")?
        .split_once(": teardown has not returned after ")?;
    let (ms, rest) = rest.split_once(" ms; SIGKILL sent to renderer pids ")?;
    let (killed, of) = rest.split_once(" (of ")?;
    Some(ReaperKill {
        at_ms,
        gen: num(g)?,
        after_ms: num(ms)?,
        killed: pid_list(killed)?,
        of: pid_list(of.strip_suffix(')')?)?,
    })
}

fn teardown_leak(msg: &str, at_ms: i64) -> Option<TeardownLeak> {
    let (g, rest) = msg
        .strip_prefix("WARN gen=")?
        .split_once(": teardown still has not returned ")?;
    let (ms, rest) = rest.split_once(
        " ms after the kill: its thread, pipeline and renderer stay until the process restarts (",
    )?;
    num(ms)?;
    Some(TeardownLeak {
        at_ms,
        gen: num(g)?,
        leaked: num(rest.strip_suffix(" leaked; Owed 55)")?)?,
    })
}

/// `output gen=<g> error: <text>` (gst_output.rs's bus error): the generation.
fn output_error(msg: &str) -> Option<u64> {
    let (g, _) = msg.strip_prefix("output gen=")?.split_once(" error: ")?;
    g.parse().ok()
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
        let (true, Some(ts_ms)) = (l.formatted, l.ts_ms) else {
            continue;
        };
        let ga = |g| GenAt { at_ms, ts_ms, gen: g };
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
        } else if let Some(r) = swap_reason(m, at_ms) {
            f.sink_swap_reasons.push(r);
        } else if let Some(g) = output_error(m) {
            f.output_errors.push(ga(g));
        } else if let Some(g) = sink_publish(m) {
            f.sink_publishes.push(ga(g));
        } else if let Some(code) = gen_after(m, "exit code=") {
            f.exits.push(Exit {
                at_ms,
                code: code as i64,
            });
        } else if let Some(k) = killed(m, at_ms) {
            f.killed.push(k);
        } else if let Some(a) = armed(m, at_ms) {
            f.hold_armed.push(a);
        } else if let Some(h) = held(m, at_ms) {
            f.teardowns_held.push(h);
        } else if let Some(r) = reaper_kill(m, at_ms) {
            f.reaper_kills.push(r);
        } else if let Some(l) = teardown_leak(m, at_ms) {
            f.teardown_leaks.push(l);
        } else if let Some(a) = sink_stall_armed(m, at_ms) {
            f.sink_stall_armed.push(a);
        } else if let Some(p) = sink_stall_planted(m, at_ms, ts_ms) {
            f.sink_stall_planted.push(p);
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
                w if w.starts_with("loading ") => f.cef_loading.push(ga(g)),
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
                ts_ms: NOON_MS,
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
                ts_ms: NOON_MS,
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
                ts_ms: NOON_MS,
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

    // PR #71's CI run 40137725801 (the PR #73 confirm pass's b-1): the error and the swap the
    // back-off later, as docker stamped them (read one line at a time, 499.38 ms apart) and as the
    // process stamped them itself (500 ms apart, the clock its back-off runs on). A fact carries both.
    #[test]
    fn a_generation_fact_carries_the_processs_own_stamp_beside_dockers() {
        let log = "2026-10-24T22:20:28.138389528Z ts=2026-10-24T22:20:28.137Z level=info \
                   msg=output gen=1 error: Connection error: Socket I/O timed out (x)\n\
                   2026-10-24T22:20:28.637769066Z ts=2026-10-24T22:20:28.637Z level=info \
                   msg=sink swap gen=2 target=rtmp://mediamtx:1935/live/****\n";
        let f = log_facts(log);
        let ms = |s: &str| rfc3339_utc_ms(s).unwrap();
        let got = [f.output_errors.as_slice(), f.sink_swaps.as_slice()]
            .concat()
            .iter()
            .map(|g| (g.gen, g.at_ms, g.ts_ms))
            .collect::<Vec<_>>();
        assert_eq!(
            got,
            vec![
                (1, ms("2026-10-24T22:20:28.138Z"), ms("2026-10-24T22:20:28.137Z")),
                (2, ms("2026-10-24T22:20:28.637Z"), ms("2026-10-24T22:20:28.637Z")),
            ]
        );
        assert_eq!((got[1].1 - got[0].1, got[1].2 - got[0].2), (499, 500));
    }

    /// Case 18's lines (Owed 94's hook; `adapters::gst_programme`): the arming line and the plant,
    /// each by its exact shape, the plant with the process's own stamp (the case's clock).
    #[test]
    fn case_18s_hook_lines_are_read_by_their_exact_shape() {
        let mut log = String::new();
        let lines = [
            (
                0,
                "WARN test hook (Owed 94): sink gen=1 stalls (hold) 25000 ms after its build",
            ),
            (
                100,
                "WARN test hook (Owed 94): sink gen=1 stalls (hang) 7 ms after its build",
            ),
            (
                25_000,
                "WARN test hook (Owed 94): sink stall=hold gen=1 planted=true",
            ),
            (
                25_100,
                "WARN test hook (Owed 94): sink stall=hang gen=1 planted=false",
            ),
            // Near misses: none is one of these facts.
            (
                26_000,
                "WARN test hook (Owed 94): sink gen=1 stalls (sleep) 25000 ms after its build",
            ),
            (
                26_100,
                "WARN test hook (Owed 94): sink gen=1 stalls (hold) +25 ms after its build",
            ),
            (
                26_200,
                "WARN test hook (Owed 94): sink gen=2 stalls (hold) 25 ms after its build",
            ),
            (
                26_300,
                "WARN test hook (Owed 94): sink gen=1 stalls (hold) 25 ms after its build now",
            ),
            (26_400, "test hook (Owed 94): sink stall=hold gen=1 planted=true"),
            (
                26_500,
                "WARN test hook (Owed 94): sink stall=hold gen=1 planted=yes",
            ),
            (
                26_600,
                "WARN test hook (Owed 94): sink stall=held gen=1 planted=true",
            ),
            (
                26_700,
                "WARN test hook (Owed 94): sink stall=hold gen=2 planted=true",
            ),
            (
                26_800,
                "gen=1: page console: WARN test hook (Owed 94): sink stall=hold gen=1 planted=true",
            ),
        ];
        for (at, m) in lines {
            log.push_str(&line(at, m));
        }
        let f = log_facts(&log);
        assert_eq!(
            f.sink_stall_armed,
            vec![
                SinkStallArmed {
                    at_ms: NOON_MS,
                    stall: "hold".into(),
                    after_ms: 25_000
                },
                SinkStallArmed {
                    at_ms: NOON_MS + 100,
                    stall: "hang".into(),
                    after_ms: 7
                }
            ]
        );
        assert_eq!(
            f.sink_stall_planted,
            vec![
                SinkStallPlanted {
                    at_ms: NOON_MS + 25_000,
                    ts_ms: NOON_MS,
                    stall: "hold".into(),
                    planted: true
                },
                SinkStallPlanted {
                    at_ms: NOON_MS + 25_100,
                    ts_ms: NOON_MS,
                    stall: "hang".into(),
                    planted: false
                }
            ]
        );
        // The producers, each once, in the adapter's source: a reworded line reds here, not on CI.
        let src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../adapters/src/gst_programme.rs"
        ))
        .expect("gst_programme.rs");
        let mut pinned = 0;
        for producer in [
            "\"WARN test hook (Owed 94): sink gen=1 stalls ({}) {} ms after its build\"",
            "\"WARN test hook (Owed 94): sink stall={} gen=1 planted={planted}\"",
            "SinkStall::Hold => \"hold\",",
            "SinkStall::Hang => \"hang\",",
        ] {
            assert_eq!(
                src.matches(producer).count(),
                1,
                "gst_programme.rs's line changed: update logfacts ({producer})"
            );
            pinned += 1;
        }
        let read = f.sink_stall_armed.len() + f.sink_stall_planted.len();
        eprintln!(
            "log lines checked: {}, producers pinned: {pinned}, facts read: {read}",
            f.lines
        );
        assert_eq!((f.lines, pinned, read), (lines.len() as u64, 4, 4));
    }

    /// An output sink's bus error (C14 fix round 3, item 2): the line a swap's cause is read from.
    /// The lines are case 5's in CI (run 40056474269), keys scrubbed as the compositor writes them.
    #[test]
    fn an_output_error_is_read_with_its_generation() {
        let mut log = String::new();
        let lines = [
            (0, "sink swap gen=1 target=rtmp://mediamtx:1935/live/****"),
            (1, "output gen=1: sink rtmp://mediamtx:1935/live/****"),
            (
                5_750,
                "output gen=1 error: Connection error: Socket I/O timed out (../gst/rtmp2/gstrtmp2sink.c(1085): \
                 error_callback (): /GstPipeline:pipeline2/GstRtmp2Sink:rtmp2sink0:\\ndomain g-io-error-quark)",
            ),
            (6_270, "sink swap gen=2 target=rtmp://mediamtx:1935/live/****"),
            (
                11_270,
                "output gen=2 error: Internal data stream error. (../libs/gst/base/gstbasesrc.c(3177))",
            ),
            // Near misses: none is an output error.
            (12_000, "output gen=3: sink rtmp://mediamtx:1935/live/****"),
            (12_100, "output gen=3 warning: Could not write (x)"),
            (12_200, "swap_sink gen=4: output gen=4: did not start: x"),
            (12_300, "output gen=x error: y (z)"),
            (12_400, "gen=1: page console: output gen=1 error: z"),
        ];
        for (at, m) in lines {
            log.push_str(&line(at, m));
        }
        let f = log_facts(&log);
        assert_eq!(
            f.output_errors,
            vec![
                GenAt {
                    at_ms: NOON_MS + 5_750,
                    ts_ms: NOON_MS,
                    gen: 1
                },
                GenAt {
                    at_ms: NOON_MS + 11_270,
                    ts_ms: NOON_MS,
                    gen: 2
                }
            ]
        );
        assert_eq!(f.sink_swaps.iter().map(|g| g.gen).collect::<Vec<_>>(), vec![1, 2]);
        // The producer's own format, so the parser and the line cannot drift apart unseen.
        let src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../adapters/src/gst_output.rs"
        ))
        .expect("gst_output.rs");
        assert_eq!(
            src.matches("\"output gen={} error: {} ({})\"").count(),
            1,
            "gst_output.rs's bus-error line changed: update `output_errors`"
        );
        eprintln!("log lines checked: {}", f.lines);
    }

    /// A swap's reason (Owed 87, PR #57's `sink swap reason=<r> gen=<g>`, logged just before the
    /// decorator's `sink swap gen=<g>`): the line cases 5, 12 and 17 read a swap's cause from. The
    /// lines are case 12's in CI (run 40106770881), keys scrubbed as the compositor writes them.
    #[test]
    fn a_swap_reason_is_read_with_its_generation() {
        let mut log = String::new();
        let lines = [
            (0, "sink swap reason=first gen=1"),
            (0, "sink swap gen=1 target=rtmp://mediamtx:1935/live/****"),
            (31_066, "sink swap reason=error gen=2"),
            (31_066, "sink swap gen=2 target=rtmp://mediamtx:1935/live/****"),
            (37_067, "sink swap reason=connect_timeout gen=3"),
            (37_067, "sink swap gen=3 target=rtmp://mediamtx:1935/live/****"),
            (40_000, "sink swap reason=error gen=4"),
            (50_000, "sink swap reason=egress_stalled gen=5"), // Owed 94's backstop
            // Near misses: none is a reason line.
            (41_000, "sink swap reason=error gen=x"),
            (41_100, "sink swap reason= gen=5"),
            (41_200, "sink swap reason=error gen=5 target=x"),
            (41_300, "gen=1: page console: sink swap reason=error gen=6"),
            (41_400, "sink swap reason=two words gen=7"),
        ];
        for (at, m) in lines {
            log.push_str(&line(at, m));
        }
        let f = log_facts(&log);
        let read: Vec<(i64, u64, &str)> = f
            .sink_swap_reasons
            .iter()
            .map(|r| (r.at_ms - NOON_MS, r.gen, r.reason.as_str()))
            .collect();
        assert_eq!(
            read,
            vec![
                (0, 1, "first"),
                (31_066, 2, "error"),
                (37_067, 3, "connect_timeout"),
                (40_000, 4, "error"),
                (50_000, 5, "egress_stalled")
            ]
        );
        assert_eq!(
            f.sink_swaps.iter().map(|g| g.gen).collect::<Vec<_>>(),
            vec![1, 2, 3],
            "a reason line is not a swap line"
        );
        // The producer's own format and its four names (Owed 94 added the backstop's), so the parser
        // and the line cannot drift apart unseen; and not the name Owed 87 retired.
        let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../app/src/driver.rs"))
            .expect("driver.rs");
        assert_eq!(
            src.matches("\"sink swap reason={} gen={gen}\"").count(),
            1,
            "driver.rs's swap reason line changed: update `sink_swap_reasons`"
        );
        let names = [
            ("First", "first"),
            ("Error", "error"),
            ("ConnectTimeout", "connect_timeout"),
            ("EgressStalled", "egress_stalled"),
        ];
        for (variant, name) in names {
            assert!(
                src.contains(&format!("SwapReason::{variant} => \"{name}\"")),
                "driver.rs's `swap_name` no longer names {variant} \"{name}\""
            );
        }
        assert_eq!(
            src.matches("SwapReason::").count(),
            names.len(),
            "driver.rs names a SwapReason this test does not"
        );
        assert!(!src.contains("\"stall\""), "driver.rs names the retired reason");
        eprintln!("log lines checked: {}, reasons read: {}", f.lines, read.len());
    }

    /// The publish gate's line (gst_output.rs, Owed 84), as case 17's measure reads it: CI's lines
    /// (PR #65, dest-stall-rule), with near misses.
    /// Case 16's lines (Owed 82, PR #62's hook; `adapters::cef_graphics`): the arming line, each held
    /// teardown, the reaper's kill and its leak count, and each build's load, all by gen.
    #[test]
    fn the_held_teardown_lines_are_read_by_their_gen() {
        let mut log = String::new();
        let lines = [
            (0, "WARN test hook (Owed 82): the next 3 CEF teardowns never return"),
            (100, "gen=1: loading http://fake-control:7000"),
            (20_000, "WARN test hook (Owed 82) gen=1: this CEF teardown never returns (2 more to hold)"),
            (20_010, "gen=2: loading http://fake-control:7000"),
            (23_000, "WARN gen=1: teardown has not returned after 3000 ms; SIGKILL sent to renderer pids [412, 418] (of [412, 418, 420])"),
            (26_000, "WARN gen=1: teardown still has not returned 3000 ms after the kill: its thread, pipeline and renderer stay until the process restarts (1 leaked; Owed 55)"),
            (26_500, "WARN gen=2: teardown has not returned after 3000 ms; SIGKILL sent to renderer pids [] (of [])"),
            // Near misses: none is one of these facts.
            (27_000, "WARN test hook (Owed 82): the next +3 CEF teardowns never return"),
            (27_100, "WARN test hook (Owed 82): the next 3 CEF teardowns never return now"),
            (27_200, "WARN test hook (Owed 82) gen=x: this CEF teardown never returns (2 more to hold)"),
            (27_300, "WARN test hook (Owed 82) gen=4: this CEF teardown never returns (2 more)"),
            (27_400, "WARN gen=5: teardown has not returned after 3000 ms; SIGKILL sent to renderer pids [1,] (of [1])"),
            (27_500, "WARN gen=6: teardown has not returned after soon ms; SIGKILL sent to renderer pids [1] (of [1])"),
            (27_510, "WARN gen=10: teardown has not returned after 3000 ms; SIGKILL sent to renderer pids [1] (of [1]"),
            (27_520, "WARN gen=11: teardown has not returned after 3000 ms; SIGKILL sent to renderer pids [1] (of [x])"),
            (27_600, "WARN gen=7: teardown still has not returned 3000 ms after the kill: its thread, pipeline and renderer stay until the process restarts (one leaked; Owed 55)"),
            (27_700, "gen=8: page console: loading http://x"),
            (27_800, "gen=9: ready (retired or crashed: its frames stay off air)"),
        ];
        for (at, m) in lines {
            log.push_str(&line(at, m));
        }
        let f = log_facts(&log);
        assert_eq!(f.hold_armed, vec![Armed { at_ms: NOON_MS, n: 3 }]);
        assert_eq!(
            f.cef_loading,
            vec![
                GenAt {
                    at_ms: NOON_MS + 100,
                    ts_ms: NOON_MS,
                    gen: 1
                },
                GenAt {
                    at_ms: NOON_MS + 20_010,
                    ts_ms: NOON_MS,
                    gen: 2
                }
            ]
        );
        assert_eq!(
            f.teardowns_held,
            vec![HeldTeardown {
                at_ms: NOON_MS + 20_000,
                gen: 1,
                left: 2
            }]
        );
        assert_eq!(
            f.reaper_kills,
            vec![
                ReaperKill {
                    at_ms: NOON_MS + 23_000,
                    gen: 1,
                    after_ms: 3_000,
                    killed: vec![412, 418],
                    of: vec![412, 418, 420]
                },
                ReaperKill {
                    at_ms: NOON_MS + 26_500,
                    gen: 2,
                    after_ms: 3_000,
                    killed: vec![],
                    of: vec![]
                }
            ]
        );
        assert_eq!(
            f.teardown_leaks,
            vec![TeardownLeak {
                at_ms: NOON_MS + 26_000,
                gen: 1,
                leaked: 1
            }]
        );
        // The producers, each once, in the adapter's source: a reworded line reds here, not on CI.
        let src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../adapters/src/cef_graphics.rs"
        ))
        .expect("cef_graphics.rs");
        let mut pinned = 0;
        for producer in [
            // The producer's line, not the adapter's own test of it (`"))` there).
            "\"WARN test hook (Owed 82): the next {n} CEF teardowns never return\"\n",
            "\"WARN test hook (Owed 82) gen={gen}: this CEF teardown never returns ({left} more to hold)\"",
            "\"WARN gen={gen}: teardown has not returned after {} ms; SIGKILL sent to renderer pids \\\n         {killed:?} (of {pids:?})\"",
            "\"WARN gen={gen}: teardown still has not returned {TEARDOWN_LEAK_AFTER_MS} ms after the \\\n             kill: its thread, pipeline and renderer stay until the process restarts ({n} leaked; \\\n             Owed 55)\"",
            "\"gen={gen}: loading {}\"",
        ] {
            assert_eq!(
                src.matches(producer).count(),
                1,
                "cef_graphics.rs's line changed: update logfacts ({producer})"
            );
            pinned += 1;
        }
        eprintln!(
            "log lines checked: {}, producers pinned: {pinned}, facts read: {}",
            f.lines,
            f.hold_armed.len()
                + f.cef_loading.len()
                + f.teardowns_held.len()
                + f.reaper_kills.len()
                + f.teardown_leaks.len()
        );
        assert_eq!(pinned, 5);
    }

    #[test]
    fn a_publish_is_read_with_its_generation() {
        let mut log = String::new();
        let lines = [
            (0, "output gen=1: the sink can send after 11 ms; the 0 video frames queued before it are dropped, and it starts at the next keyframe"),
            (40_135, "output gen=3: the sink can send after 2649 ms; the 61 video frames queued before it are dropped, and it starts at the next keyframe"),
            // Near misses: none is a publish line.
            (41_000, "output gen=x: the sink can send after 5 ms; the 0 video frames queued"),
            (41_100, "output gen=4: the sink can send after soon ms; the 0 video frames queued"),
            (41_200, "output gen=5: the sink can send after 7 ms"),
            (41_300, "output gen=6 error: the sink can send after 7 ms; no"),
            (41_400, "output gen=7 the sink can send after 5 ms; the 0 video frames queued"),
        ];
        for (at, m) in lines {
            log.push_str(&line(at, m));
        }
        let f = log_facts(&log);
        let read: Vec<(i64, u64)> = f
            .sink_publishes
            .iter()
            .map(|p| (p.at_ms - NOON_MS, p.gen))
            .collect();
        assert_eq!(read, vec![(0, 1), (40_135, 3)]);
        assert!(
            f.output_errors.iter().any(|e| e.gen == 6),
            "an error line stays an error"
        );
        // And is no publish on its own (the parse loop reads errors first, which would hide it).
        assert_eq!(
            sink_publish("output gen=6 error: the sink can send after 7 ms; no"),
            None
        );
        let src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../adapters/src/gst_output.rs"
        ))
        .expect("gst_output.rs");
        assert_eq!(
            src.matches("\"output gen={}: the sink can send after {} ms; the {held} video frames queued \\")
                .count(),
            1,
            "gst_output.rs's publish line changed: update `sink_publishes`"
        );
        eprintln!("log lines checked: {}, publishes read: {}", f.lines, read.len());
    }

    #[test]
    fn an_empty_log_has_no_facts() {
        let f = log_facts("");
        assert_eq!(f, LogFacts::default());
        assert_eq!(f.lines, 0);
    }
}
