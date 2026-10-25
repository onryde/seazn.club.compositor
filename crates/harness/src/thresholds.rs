//! The chaos suite's numbers (C14, `docs/tasks/C14-chaos-suite.md`): `Thresholds::default()`, the
//! domain's named consts, the spec §6 budgets and the few adapter constants a case bound names,
//! serialised to `harness/thresholds.json` by `dump-thresholds`. Every case verdict reads that file as
//! `$t` (run-case.sh's `require`), so **no number is typed into a verdict**: when the spike re-pins a
//! threshold, the committed file moves (the drift tests below), and the cases move with it.
//!
//! **Where the budgets live** (a deviation from the brief, recorded in `docs/_INDEX.md`): the brief
//! adds them to `crates/domain/src/config.rs`; the C14 dispatch makes the domain read-only for this
//! task. They are here, each with its source, and their consistency tests read the domain's own
//! `Thresholds::default()`, so a spike-driven threshold change that breaks a budget still reds at
//! `cargo test`, not in a two-minute chaos run.
use domain::config::{
    OutputProfile, Thresholds, CHROMIUM_GPU_DEATHS_ABORT, ENCODER_SPEED_REALTIME_MIN, OUTPUT_GAP_BUDGET_MS,
    OUTPUT_PROGRESS_POLL_MS, OUTPUT_SINK_TIMEOUT_MS, TICK_WIDEST_MS,
};
use serde_json::{json, Map, Value};

/// Spec §6 "Budgets": "slate after camera loss ≤3.1 s".
pub const BUDGET_SLATE_AFTER_LOSS_MS: u64 = 3_100;
/// Spec §6 "Budgets": "return to live ≤1.2 s" (the failure table: "Live 1.2 s after frames return").
pub const BUDGET_RETURN_TO_LIVE_MS: u64 = 1_200;
/// Spec §6 "Budgets": "output reconnect ≤5 s".
pub const BUDGET_OUTPUT_RECONNECT_MS: u64 = 5_000;
/// Spec §6, the failure table's output disconnect / stall row: "≤2 s of programme lost".
pub const BUDGET_OUTPUT_LOST_MAX_MS: u64 = 2_000;
/// Spec §6, the failure table's encoder overrun row: "Real time within 10 s".
pub const BUDGET_LADDER_REALTIME_MS: u64 = 10_000;
/// Spec §6, the failure table's process crash row: "Output back ≤15 s". The same number as the
/// domain's `OUTPUT_GAP_BUDGET_MS` (F-004: YouTube's broadcast split; owner, 2026-10-20, Owed 69).
pub const BUDGET_PROCESS_RESTART_OUTPUT_MS: u64 = OUTPUT_GAP_BUDGET_MS;
/// Spec §6 "Budgets": "input frame→encoder ≤150 ms".
pub const BUDGET_FRAME_TO_ENCODER_MS: u64 = 150;
/// The C14 brief's budget table: the PDF's A/V budget, which spec §6 adopts as written ("The PDF's
/// failure table, timing budgets and resource bounds are adopted as written"); §6's own text names
/// no A/V number.
pub const BUDGET_AV_OFFSET_MS: u64 = 40;
/// Spec §6, the failure table's score feed row: the page "signals `scoreStale` after 10 s" (the
/// page's `SCORE_STALE_AFTER_MS`, R2005). The harness page's own stale rule reads this value from
/// fake-control (`control::PageControl`), so the page and case 7's bound share one number.
pub const BUDGET_SCORE_STALE_AFTER_MS: u64 = 10_000;
/// The C14 brief, cases 6 and 10: "`fps_mean` ≥ 29.5 (the clock holds)".
pub const BUDGET_FPS_MEAN_MIN: f64 = 29.5;
/// The C14 brief, case 13: the zygote verdict "within 2 s of the kill (the 1 s child poll, a tick,
/// slack)".
pub const BUDGET_ZYGOTE_VERDICT_MS: u64 = 2_000;
/// Owed 70 (a), the C14 brief's case 15: the restart threshold leaves two survivable GPU-process
/// deaths before Chromium's abort; one is budgeted to deaths the 1 s child poll has not read yet...
pub const BUDGET_GPU_DEATHS_UNREAD: u64 = 1;
/// ...and one to deaths during the exit (the final beat's flush, while CEF still runs).
pub const BUDGET_GPU_DEATHS_AT_EXIT: u64 = 1;
/// The harness page's poll period for `/score.json` and `/page-control.json`
/// (`harness/pages/overlay-test.html`, which reads it from fake-control). Case 7's recovery bound:
/// the first poll after the feed returns.
pub const HARNESS_PAGE_POLL_MS: u64 = 1_000;

/// The adapter constants a case bound names. They come from `adapters` (a GStreamer crate), so the
/// `media` build passes them in (`dump-thresholds`); the host drift test passes the committed
/// values back and checks everything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdapterConsts {
    /// C11's `CHILD_POLL_MS`: the CEF child poll; case 13's verdict window, case 15's lag.
    pub cef_child_poll_ms: u64,
    /// C09's `ENCODED_RING_SPAN` (the 2 s leaky output queue, spec §6), in ms.
    pub encoded_ring_span_ms: u64,
    /// C11's `REAPER_KILL_AFTER_MS`: a retired build's teardown not back by then has its renderers
    /// SIGKILLed (case 16: each held teardown's reaper line, and the room a leg leaves).
    pub reaper_kill_after_ms: u64,
    /// C11's `TEARDOWN_LEAK_AFTER_MS`: still not back this long after the kill, it counts leaked
    /// (case 16: the leak count, about `reaper_kill_after_ms` + this after the retirement).
    pub teardown_leak_after_ms: u64,
}

/// `harness/thresholds.json`, as `dump-thresholds` writes it.
pub fn dump(adapters: AdapterConsts) -> Value {
    // An exhaustive destructure: a field the domain adds is a compile error here, never a number
    // the cases silently lack.
    let Thresholds {
        frozen_after_ms,
        slate_after_ms,
        resume_after_ms,
        input_backoff_base_ms,
        input_backoff_cap_ms,
        input_connect_timeout_ms,
        input_stall_after_ms,
        input_srt_latency_ms,
        kick_alert_count,
        kick_window_ms,
        output_backoff_base_ms,
        output_backoff_cap_ms,
        output_connect_timeout_ms,
        output_connected_after_ms,
        output_egress_flat_ms,
        cef_fail_count,
        cef_fail_window_ms,
        cef_load_timeout_ms,
        cef_recycle_every_ms,
        cef_backoff_base_ms,
        cef_backoff_cap_ms,
        cef_alive_every_ms,
        cef_hang_after_ms,
        cef_rss_cap_mb,
        cef_leaks_to_restart,
        cef_gpu_deaths_to_restart,
        ladder_overruns_to_step,
        ladder_window_ms,
        drain_timeout_ms,
        final_hold_ms,
        heartbeat_interval_ms,
        bootstrap_backoff_base_ms,
        bootstrap_backoff_cap_ms,
    } = Thresholds::default();
    let AdapterConsts {
        cef_child_poll_ms,
        encoded_ring_span_ms,
        reaper_kill_after_ms,
        teardown_leak_after_ms,
    } = adapters;
    let mut m = Map::new();
    let mut put = |k: &str, v: Value| {
        m.insert(k.to_owned(), v);
    };
    put(
        "source",
        json!("cargo run -p harness --bin dump-thresholds > harness/thresholds.json (C14); never edited by hand"),
    );
    // Thresholds::default(), every field.
    put("frozen_after_ms", json!(frozen_after_ms));
    put("slate_after_ms", json!(slate_after_ms));
    put("resume_after_ms", json!(resume_after_ms));
    put("input_backoff_base_ms", json!(input_backoff_base_ms));
    put("input_backoff_cap_ms", json!(input_backoff_cap_ms));
    put("input_connect_timeout_ms", json!(input_connect_timeout_ms));
    put("input_stall_after_ms", json!(input_stall_after_ms));
    put("input_srt_latency_ms", json!(input_srt_latency_ms));
    put("kick_alert_count", json!(kick_alert_count));
    put("kick_window_ms", json!(kick_window_ms));
    put("output_backoff_base_ms", json!(output_backoff_base_ms));
    put("output_backoff_cap_ms", json!(output_backoff_cap_ms));
    put("output_connect_timeout_ms", json!(output_connect_timeout_ms));
    put("output_connected_after_ms", json!(output_connected_after_ms));
    put("output_egress_flat_ms", json!(output_egress_flat_ms));
    put("cef_fail_count", json!(cef_fail_count));
    put("cef_fail_window_ms", json!(cef_fail_window_ms));
    put("cef_load_timeout_ms", json!(cef_load_timeout_ms));
    put("cef_recycle_every_ms", json!(cef_recycle_every_ms));
    put("cef_backoff_base_ms", json!(cef_backoff_base_ms));
    put("cef_backoff_cap_ms", json!(cef_backoff_cap_ms));
    put("cef_alive_every_ms", json!(cef_alive_every_ms));
    put("cef_hang_after_ms", json!(cef_hang_after_ms));
    put("cef_rss_cap_mb", json!(cef_rss_cap_mb));
    put("cef_leaks_to_restart", json!(cef_leaks_to_restart));
    put("cef_gpu_deaths_to_restart", json!(cef_gpu_deaths_to_restart));
    put("ladder_overruns_to_step", json!(ladder_overruns_to_step));
    put("ladder_window_ms", json!(ladder_window_ms));
    put("drain_timeout_ms", json!(drain_timeout_ms));
    put("final_hold_ms", json!(final_hold_ms));
    put("heartbeat_interval_ms", json!(heartbeat_interval_ms));
    put("bootstrap_backoff_base_ms", json!(bootstrap_backoff_base_ms));
    put("bootstrap_backoff_cap_ms", json!(bootstrap_backoff_cap_ms));
    // The domain's named consts, and the harness session's frame period (720p30, the fixture's).
    put("encoder_speed_realtime_min", json!(ENCODER_SPEED_REALTIME_MIN));
    put("output_gap_budget_ms", json!(OUTPUT_GAP_BUDGET_MS));
    // rtmp2sink's `timeout`, which C09 sets: the only detector of a destination stall (Owed 87).
    put("output_sink_timeout_ms", json!(OUTPUT_SINK_TIMEOUT_MS));
    // C09's poll of the sink's total, and the widest gap between ticks: the egress backstop's terms
    // (Owed 94), which the output-hung case's bounds read.
    put("output_progress_poll_ms", json!(OUTPUT_PROGRESS_POLL_MS));
    put("tick_widest_ms", json!(TICK_WIDEST_MS));
    put("chromium_gpu_deaths_abort", json!(CHROMIUM_GPU_DEATHS_ABORT));
    put("frame_period_ms", json!(OutputProfile::P720.frame_period_ms()));
    // Owed 86's starvation alarm, for the same profile: raised by a beat below the floor, cleared by
    // one at or above the recovered level (case 6, the honest beat).
    put("output_fps_floor", json!(OutputProfile::P720.fps_floor()));
    put("output_fps_recovered", json!(OutputProfile::P720.fps_recovered()));
    // C08's final-beat flush bound (an in-place restart's decision-to-exit part).
    put("final_beat_timeout_ms", json!(app::ports::FINAL_BEAT_TIMEOUT_MS));
    // The adapters'.
    put("cef_child_poll_ms", json!(cef_child_poll_ms));
    put("encoded_ring_span_ms", json!(encoded_ring_span_ms));
    put("reaper_kill_after_ms", json!(reaper_kill_after_ms));
    put("teardown_leak_after_ms", json!(teardown_leak_after_ms));
    // The budgets.
    put("budget_slate_after_loss_ms", json!(BUDGET_SLATE_AFTER_LOSS_MS));
    put("budget_return_to_live_ms", json!(BUDGET_RETURN_TO_LIVE_MS));
    put("budget_output_reconnect_ms", json!(BUDGET_OUTPUT_RECONNECT_MS));
    put("budget_output_lost_max_ms", json!(BUDGET_OUTPUT_LOST_MAX_MS));
    put("budget_ladder_realtime_ms", json!(BUDGET_LADDER_REALTIME_MS));
    put(
        "budget_process_restart_output_ms",
        json!(BUDGET_PROCESS_RESTART_OUTPUT_MS),
    );
    put("budget_frame_to_encoder_ms", json!(BUDGET_FRAME_TO_ENCODER_MS));
    put("budget_av_offset_ms", json!(BUDGET_AV_OFFSET_MS));
    put("budget_score_stale_after_ms", json!(BUDGET_SCORE_STALE_AFTER_MS));
    put("budget_fps_mean_min", json!(BUDGET_FPS_MEAN_MIN));
    put("budget_zygote_verdict_ms", json!(BUDGET_ZYGOTE_VERDICT_MS));
    put("budget_gpu_deaths_unread", json!(BUDGET_GPU_DEATHS_UNREAD));
    put("budget_gpu_deaths_at_exit", json!(BUDGET_GPU_DEATHS_AT_EXIT));
    put("harness_page_poll_ms", json!(HARNESS_PAGE_POLL_MS));
    Value::Object(m)
}

/// The file's text: pretty JSON and a final newline (what the drift tests compare byte for byte).
pub fn render(adapters: AdapterConsts) -> String {
    let v = dump(adapters);
    serde_json::to_string_pretty(&v).unwrap_or_default() + "\n"
}

/// The committed file's path, from this crate.
pub const COMMITTED: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../harness/thresholds.json");

/// The committed file's adapter constants, read back (the host drift test's half).
pub fn committed_adapters(v: &Value) -> Option<AdapterConsts> {
    let n = |k: &str| v.get(k).and_then(Value::as_u64);
    Some(AdapterConsts {
        cef_child_poll_ms: n("cef_child_poll_ms")?,
        encoded_ring_span_ms: n("encoded_ring_span_ms")?,
        reaper_kill_after_ms: n("reaper_kill_after_ms")?,
        teardown_leak_after_ms: n("teardown_leak_after_ms")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOTH_MODES: [OutputProfile; 2] = [OutputProfile::P720, OutputProfile::P1080];

    fn committed() -> (String, Value) {
        let text = std::fs::read_to_string(COMMITTED).expect("harness/thresholds.json is committed");
        let v: Value = serde_json::from_str(&text).expect("harness/thresholds.json is JSON");
        (text, v)
    }

    // The drift gate's host half: everything but the adapters' four constants (the media test,
    // tests/thresholds.rs, checks those against the adapters crate itself).
    #[test]
    fn the_committed_thresholds_are_what_the_domain_and_the_budgets_say() {
        let (text, v) = committed();
        let a = committed_adapters(&v).expect("the committed file names the adapter constants");
        assert_eq!(
            text,
            render(a),
            "harness/thresholds.json is stale: run `cargo run -p harness --bin dump-thresholds > harness/thresholds.json`"
        );
        let keys = v.as_object().map_or(0, Map::len);
        eprintln!("thresholds.json keys checked: {keys}");
        assert!(keys > 40, "{keys} keys");
    }

    // Every Thresholds field reaches the file: the destructure in `dump` is exhaustive, and each
    // value is the default's (a field read into the wrong key would show here).
    #[test]
    fn each_threshold_is_dumped_under_its_own_name() {
        let t = Thresholds::default();
        let v = dump(AdapterConsts {
            cef_child_poll_ms: 2,
            encoded_ring_span_ms: 3,
            reaper_kill_after_ms: 4,
            teardown_leak_after_ms: 5,
        });
        let pairs: [(&str, u64); 10] = [
            ("slate_after_ms", t.slate_after_ms),
            ("resume_after_ms", t.resume_after_ms),
            ("output_connect_timeout_ms", t.output_connect_timeout_ms),
            ("output_sink_timeout_ms", OUTPUT_SINK_TIMEOUT_MS),
            ("output_egress_flat_ms", t.output_egress_flat_ms),
            ("output_progress_poll_ms", OUTPUT_PROGRESS_POLL_MS),
            ("tick_widest_ms", TICK_WIDEST_MS),
            ("cef_hang_after_ms", t.cef_hang_after_ms),
            ("heartbeat_interval_ms", t.heartbeat_interval_ms),
            ("cef_gpu_deaths_to_restart", t.cef_gpu_deaths_to_restart as u64),
        ];
        for (k, want) in pairs {
            assert_eq!(v[k].as_u64(), Some(want), "{k}");
        }
        // The adapters' are carried, not invented.
        assert_eq!(v["cef_child_poll_ms"].as_u64(), Some(2));
        assert_eq!(v["encoded_ring_span_ms"].as_u64(), Some(3));
        assert_eq!(v["reaper_kill_after_ms"].as_u64(), Some(4));
        assert_eq!(v["teardown_leak_after_ms"].as_u64(), Some(5));
        // Owed 87 retired the stall rule, and with it the threshold only its bound read (the poll it
        // also dropped is back for Owed 94's backstop, above).
        let retired = ["output_stall_after_ms"];
        for k in retired {
            assert!(v.get(k).is_none(), "{k} is still dumped");
        }
        // Owed 86's honest beat: the starvation alarm's two levels for the harness session's profile
        // (720p30, the fixture's), read from the profile's own declarations (case 6).
        let levels = [
            ("output_fps_floor", OutputProfile::P720.fps_floor()),
            ("output_fps_recovered", OutputProfile::P720.fps_recovered()),
        ];
        for (k, want) in levels {
            assert_eq!(v[k].as_f64(), Some(want), "{k}");
        }
        eprintln!(
            "named fields checked: {}",
            pairs.len() + 4 + retired.len() + levels.len()
        );
    }

    // The brief: "Add a unit test that each budget is consistent with its threshold". A spike-driven
    // threshold change that breaks a budget reds here.
    #[test]
    fn the_slate_and_the_return_fit_their_budgets_in_both_modes() {
        let t = Thresholds::default();
        let mut checked = 0;
        for p in BOTH_MODES {
            let f = p.frame_period_ms();
            assert!(
                t.slate_after_ms as f64 + f <= BUDGET_SLATE_AFTER_LOSS_MS as f64,
                "{p:?} slate"
            );
            assert!(
                t.resume_after_ms as f64 + f <= BUDGET_RETURN_TO_LIVE_MS as f64,
                "{p:?} return"
            );
            assert!(
                f <= BUDGET_FRAME_TO_ENCODER_MS as f64,
                "{p:?}: a tick fits the encoder budget"
            );
            assert!(
                f <= BUDGET_AV_OFFSET_MS as f64,
                "{p:?}: one frame resolves the A/V budget"
            );
            checked += 4;
        }
        eprintln!("mode budgets checked: {checked}");
        assert!(checked > 0);
    }

    #[test]
    fn the_output_budgets_hold_the_output_thresholds() {
        let t = Thresholds::default();
        // One capped back-off then a connect attempt starts inside the reconnect budget.
        assert!(t.output_backoff_cap_ms < BUDGET_OUTPUT_RECONNECT_MS);
        // The restart budget is YouTube's split (F-004) and no looser.
        assert_eq!(BUDGET_PROCESS_RESTART_OUTPUT_MS, 15_000);
    }

    // Constant against constant: checked by the compiler (clippy refuses a runtime assert on
    // constants), so a change that breaks one fails the build. (The restart budget IS the domain's
    // OUTPUT_GAP_BUDGET_MS, by definition above: no assert can add to that; the PR #52 review's m7.)
    // The final beat's flush fits inside the restart budget with room for a boot.
    const _: () = assert!(app::ports::FINAL_BEAT_TIMEOUT_MS < BUDGET_PROCESS_RESTART_OUTPUT_MS);
    // The page polls more often than its stale rule fires.
    const _: () = assert!(HARNESS_PAGE_POLL_MS < BUDGET_SCORE_STALE_AFTER_MS);

    // The 2026-10-18 amendment: the ladder's one step lands within one window of its overruns.
    #[test]
    fn the_ladder_window_fits_the_realtime_budget() {
        let t = Thresholds::default();
        assert!(t.ladder_window_ms <= BUDGET_LADDER_REALTIME_MS);
    }

    // config.rs: `cef_hang_after_ms` is "equal to the page's SCORE_STALE_AFTER_MS (spec §6)".
    #[test]
    fn the_page_stale_rule_is_the_hang_rule() {
        assert_eq!(
            Thresholds::default().cef_hang_after_ms,
            BUDGET_SCORE_STALE_AFTER_MS
        );
    }

    // Owed 70 (a): restart at N, abort at the 6th; the deaths between are the two budgets.
    #[test]
    fn the_gpu_death_budgets_are_the_survivable_deaths() {
        let t = Thresholds::default();
        let survivable = CHROMIUM_GPU_DEATHS_ABORT - 1 - t.cef_gpu_deaths_to_restart;
        assert_eq!(
            (BUDGET_GPU_DEATHS_UNREAD + BUDGET_GPU_DEATHS_AT_EXIT) as usize,
            survivable
        );
    }

    #[test]
    fn the_fps_floor_is_inside_one_frame_of_thirty() {
        // 29.5 fps over a 10 s window is 295 frames against 300: at most five missing.
        let f = f64::from(OutputProfile::P720.fps);
        assert!(BUDGET_FPS_MEAN_MIN < f && BUDGET_FPS_MEAN_MIN > f * ENCODER_SPEED_REALTIME_MIN);
    }

    /// The numbers a budget's source states: every occurrence in `text` of `pre`, a number, then
    /// `post`, times `unit`.
    fn stated(text: &str, pre: &str, post: &str, unit: f64) -> Vec<f64> {
        let mut found = Vec::new();
        let mut from = 0;
        while let Some(i) = text[from..].find(pre) {
            let at = from + i + pre.len();
            let n: String = text[at..]
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            if !n.is_empty() && text[at + n.len()..].starts_with(post) {
                found.push(n.parse::<f64>().expect("a number") * unit);
            }
            from = at;
        }
        found
    }

    // The PR #52 review's m7: a budget edited and then regenerated passed every drift test, because
    // only the restart budget was pinned (to the domain's OUTPUT_GAP_BUDGET_MS). The domain declares
    // no const for the others (Owed 83), so each is pinned to the sentence it was taken from: the
    // spec §6 text, or the C14 brief's where the spec has none. A budget changed without its source
    // reds here, and so does a source changed without its budget.
    #[test]
    fn every_budget_is_the_number_its_source_states() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let read = |p: &str| std::fs::read_to_string(root.join(p)).unwrap_or_else(|e| panic!("{p}: {e}"));
        let spec = read("docs/specs/2026-10-14-r2-compositor-design.md");
        let brief = read("docs/tasks/C14-chaos-suite.md");
        let ms = |v: u64| v as f64;
        let rows: [(&str, &str, &str, f64, f64); 11] = [
            (
                &spec,
                "input frame→encoder ≤",
                " ms",
                1.0,
                ms(BUDGET_FRAME_TO_ENCODER_MS),
            ),
            (
                &spec,
                "slate after camera loss ≤",
                " s",
                1000.0,
                ms(BUDGET_SLATE_AFTER_LOSS_MS),
            ),
            (
                &spec,
                "return to live ≤",
                " s",
                1000.0,
                ms(BUDGET_RETURN_TO_LIVE_MS),
            ),
            (
                &spec,
                "output reconnect ≤",
                " s",
                1000.0,
                ms(BUDGET_OUTPUT_RECONNECT_MS),
            ),
            (
                &spec,
                "≤",
                " s of programme lost",
                1000.0,
                ms(BUDGET_OUTPUT_LOST_MAX_MS),
            ),
            (
                &spec,
                "Real time within ",
                " s",
                1000.0,
                ms(BUDGET_LADDER_REALTIME_MS),
            ),
            (
                &spec,
                "Output back ≤",
                " s",
                1000.0,
                ms(BUDGET_PROCESS_RESTART_OUTPUT_MS),
            ),
            (&spec, "A/V within ±", " ms", 1.0, ms(BUDGET_AV_OFFSET_MS)),
            (
                &spec,
                "signals `scoreStale` after ",
                " s",
                1000.0,
                ms(BUDGET_SCORE_STALE_AFTER_MS),
            ),
            (
                &brief,
                "`fps_mean` over the window stays ≥ ",
                " (the clock holds)",
                1.0,
                BUDGET_FPS_MEAN_MIN,
            ),
            (
                &brief,
                "`cef health zygote_gone=<g>` within ",
                " s of the kill",
                1000.0,
                ms(BUDGET_ZYGOTE_VERDICT_MS),
            ),
        ];
        let mut checked = 0;
        for (text, pre, post, unit, budget) in rows {
            let found = stated(text, pre, post, unit);
            assert!(!found.is_empty(), "no `{pre}<n>{post}` in its source");
            for n in &found {
                assert!(
                    (n - budget).abs() < 1e-9,
                    "`{pre}<n>{post}`: the source says {n}, the budget is {budget} (both in the budget's unit)"
                );
                checked += 1;
            }
        }
        eprintln!("budget statements checked: {checked}");
        assert!(checked >= rows.len());
    }

    #[test]
    fn a_stated_number_is_read_with_its_unit_and_only_between_its_words() {
        let t = "a ≤1.2 s; b ≤3 ms; c ≤x s; ≤2 s of programme lost";
        assert_eq!(stated(t, "a ≤", " s", 1000.0), [1200.0]);
        assert_eq!(stated(t, "b ≤", " s", 1000.0), Vec::<f64>::new(), "ms is not s");
        assert_eq!(stated(t, "c ≤", " s", 1000.0), Vec::<f64>::new());
        assert_eq!(stated(t, "≤", " s of programme lost", 1000.0), [2000.0]);
    }
}
