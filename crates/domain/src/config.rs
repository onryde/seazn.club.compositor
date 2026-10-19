//! THE number authority. Every tunable lives here, each with the source that set it.
//! "PDF" = Live Sports Streaming Compositor — Production Design & Specification (2026-10-03).
//! "Spec" = `docs/specs/2026-10-14-r2-compositor-design.md`; "plan" = its Global Constraints.
//! "C02 brief default" = a number the C02 brief chose where no PDF, spec, plan or spike number exists.
//! A spike measurement (F-id in `_FINDINGS.md`) overrides a PDF value and must be cited.
use crate::time::Ms;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thresholds {
    /// PDF robustness state machine: "no frame 0.5 s" → Frozen.
    pub frozen_after_ms: Ms,
    /// PDF state machine "no frame 3 s" → Reconnecting slate; timing budget "slate ≤ 3.1 s".
    pub slate_after_ms: Ms,
    /// PDF: "a return to live requires 1 s of continuous frames".
    pub resume_after_ms: Ms,
    /// PDF failure table: input back-off 1 s → 10 s, retried forever. The first wait.
    pub input_backoff_base_ms: Ms,
    /// PDF failure table: input back-off 1 s → 10 s. The cap.
    pub input_backoff_cap_ms: Ms,
    /// Spec §4.2: a connect that neither succeeds nor fails within this is treated as lost. F-002 kept
    /// 5 s with a thin margin: the slowest healthy first decoded frame was 4.19 s (F-002) and the
    /// slowest first AU 4.4 s (F-004 rerun 1), so C10 times "connected" to the first decoded frame.
    pub input_connect_timeout_ms: Ms,
    /// Spec §6 "reconnect forever": a Running input that delivers no frame for this long is treated as
    /// a Stalled loss and rebuilt. Set after the 3 s slate so the viewer sees the slate first. F-002
    /// confirmed it is needed: CF holds a silent, encrypted playback socket open indefinitely after the
    /// phone stops publishing.
    pub input_stall_after_ms: Ms,
    /// C10: the SRT receive latency the input pipeline asks of CF Input A's playback (`srtsrc`
    /// `latency`, in its URI). The POC's `INPUT_LATENCY_MS` (1 000). F-002 measured every CF reader
    /// at 1 000 ms: 0 packets lost or retransmitted in 120 s, the first AU at 2.22–3.89 s, and an
    /// in-chain camera→overlay delay of 3 083–3 105 ms that includes it, so the overlay delay's
    /// default (F-002, ADR 0006) moves with this value.
    pub input_srt_latency_ms: Ms,
    /// PDF failure table: "alert if it repeats > 3 times in 5 min" (alert when count EXCEEDS this).
    pub kick_alert_count: usize,
    /// PDF failure table: the "5 min" of "> 3 times in 5 min".
    pub kick_window_ms: Ms,
    /// PDF chaos "destination accepts but stalls: detected within 5 s": a Connected output whose
    /// bytes-SENT total has not grown for this long is stalled, and its sink is swapped at once (C04
    /// `OutputLink::on_progress`). Not an ack stall: F-005 found CF Input B sends no RTMP
    /// acknowledgements (rtmp2sink `out-bytes-acked` stayed 0 in its 290 s pushes while
    /// `out-bytes-total` grew), so C09 feeds `out-bytes-total` (docs/_INDEX.md false premises).
    pub output_stall_after_ms: Ms,
    /// C02 brief default, sized to the PDF budget "output reconnect ≤ 5 s": back-off 0.5 s → 2 s keeps
    /// attempts inside it. The first wait.
    pub output_backoff_base_ms: Ms,
    /// C02 brief default, sized to the PDF budget "output reconnect ≤ 5 s": the 2 s cap.
    pub output_backoff_cap_ms: Ms,
    /// Spec §4.2: "a reconnect is expected to complete within 5 s". F-004 addendum: stall detection,
    /// one capped back-off and one whole connect must stay under YouTube's 15 s broadcast split.
    pub output_connect_timeout_ms: Ms,
    /// F-011 (Owed 38; the coordinator's ruling): a sink is Connected only once its bytes-sent total
    /// has kept growing for this long since its first growth, with no error and no stall between. The
    /// first nonzero `out-bytes-total` is the 476 B RTMP `connect` command (22–69 ms after the build,
    /// n = 10 on CF), and a REFUSED publish keeps counting until its refusal arrives: 262 334 B by
    /// 538 ms on CF (a made-up key) and 1 008 779 B over 2 033 ms on mediamtx (a denied path). No byte
    /// floor tells those apart; time does. 5 s is about 2.5× the slowest refusal measured. It is not
    /// longer because the first `playing` beat and the back-off's reset both wait on Connected.
    pub output_connected_after_ms: Ms,
    /// Spec §4.2: 3 CEF failures in 5 min → graphics Failed.
    pub cef_fail_count: usize,
    /// Spec §4.2: the "5 min" of "3 CEF failures in 5 min".
    pub cef_fail_window_ms: Ms,
    /// PDF page contract "loads in under 2 s"; ×5 margin before it counts as a failure. F-001 measured
    /// cefsrc start → first buffer at 2.781–2.804 s, well inside it.
    pub cef_load_timeout_ms: Ms,
    /// Spec §6: CEF recycled every 2 h with its last frame held.
    pub cef_recycle_every_ms: Ms,
    /// C02 brief default: spec §6 says "restart with backoff" and names no number, so CEF mirrors the
    /// input back-off, 1 s → 10 s. The first wait.
    pub cef_backoff_base_ms: Ms,
    /// C02 brief default: the cap of the CEF back-off (1 s → 10 s, mirroring the input).
    pub cef_backoff_cap_ms: Ms,
    /// R2005: the overlay page logs `seazn:alive` this often after ready, on F-001's console channel
    /// (`docs/_INDEX.md` Owed 18, owner 2026-10-18). The domain does not read it: it is the page's
    /// contract, kept here so its ratio to `cef_hang_after_ms` is pinned in one place.
    pub cef_alive_every_ms: Ms,
    /// C05b (Owed 18): a Ready page silent this long is hung, a counted failure that hides the overlay
    /// at once. A hung page paints nothing new and cefsrc emits only on paint (F-001), so frame silence
    /// is no signal; the beat is. Five beats, equal to the page's SCORE_STALE_AFTER_MS (spec §6);
    /// shorter turns CPU starvation (CEF children run at nice 10, so beats slip under load) into
    /// restarts whose load spikes feed the ladder.
    pub cef_hang_after_ms: Ms,
    /// Spec §6: CEF's memory is capped; above the cap C11 reports `CefCrashed` and the domain restarts
    /// the page. C11 reads it as the PSS sum of CEF's child processes (F-013: their RSS sum counts the
    /// zygote's shared pages once per process, 436 MB for one healthy page against 173 MB of PSS).
    pub cef_rss_cap_mb: u64,
    /// C02 brief default (no PDF number): overruns within `ladder_window_ms` needed to step down a rung.
    pub ladder_overruns_to_step: usize,
    /// C02 brief default (no PDF number): the window `ladder_overruns_to_step` is counted in.
    pub ladder_window_ms: Ms,
    /// R1 sends SIGINT and SIGKILLs after RUNNER_STOP_GRACE_SECONDS (seazn.club
    /// `apps/web/src/server/relay/config.ts`): 20 s since the owner's D15 ruling (2026-10-16), which
    /// fits the final hold, this drain and the headroom. The drain must finish with headroom for EOS to
    /// reach the sink and the process to exit: 3 s. C12's `final_hold_and_drain_fit_inside_r1_stop_grace`
    /// pins `final_hold_ms + drain_timeout_ms + 3 s` inside R1's grace.
    pub drain_timeout_ms: Ms,
    /// Overlay templates spec 2026-10-15 §4.2 (D4): FINAL_HOLD_MS, how long the Final scene holds before
    /// EOS. C12's `final_hold_and_drain_fit_inside_r1_stop_grace` pins final_hold + drain + 3 s inside
    /// R1's grace.
    pub final_hold_ms: Ms,
    /// Spec §5.3 default (`heartbeatIntervalMs`: 5000); the bootstrap's `heartbeatIntervalMs` overrides it.
    pub heartbeat_interval_ms: Ms,
    /// C02 brief default (no PDF number): bootstrap GET retry back-off while web is unreachable at boot.
    /// The first wait.
    pub bootstrap_backoff_base_ms: Ms,
    /// C02 brief default (no PDF number): the cap of the bootstrap GET retry back-off.
    pub bootstrap_backoff_cap_ms: Ms,
}

/// The floor for "the encoder keeps real time". A 1 s window at 30 fps holds 29–31 frames
/// (0.967–1.033), so 1.0 would fail a third of healthy windows. It is the C00 cost gate's "encoder
/// speed ≥ 0.95" (`_FINDINGS.md`, Scope and gate); F-005's worst 10 s window was 0.9963. A const,
/// not a `Thresholds` field, because `Thresholds` derives `Eq`. C09's band and C14 case 6 both read it.
pub const ENCODER_SPEED_REALTIME_MIN: f64 = 0.95;

/// F-004 (owner-observed; docs/_INDEX.md decision log, the C00 owner session): YouTube kept the
/// organiser's broadcast through 5 s and 15 s output gaps and started a new broadcast after a 30 s
/// one. So every recovery path the broadcast must survive finishes its output gap UNDER this. A const,
/// not a `Thresholds` field: it is YouTube's behaviour, not a setting of ours.
pub const OUTPUT_GAP_BUDGET_MS: Ms = 15_000;

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            frozen_after_ms: 500,
            slate_after_ms: 3_000,
            resume_after_ms: 1_000,
            input_backoff_base_ms: 1_000,
            input_backoff_cap_ms: 10_000,
            input_connect_timeout_ms: 5_000,
            input_stall_after_ms: 5_000,
            input_srt_latency_ms: 1_000,
            kick_alert_count: 3,
            kick_window_ms: 300_000,
            output_stall_after_ms: 5_000,
            output_backoff_base_ms: 500,
            output_backoff_cap_ms: 2_000,
            output_connect_timeout_ms: 5_000,
            output_connected_after_ms: 5_000,
            cef_fail_count: 3,
            cef_fail_window_ms: 300_000,
            cef_load_timeout_ms: 10_000,
            cef_recycle_every_ms: 7_200_000,
            cef_backoff_base_ms: 1_000,
            cef_backoff_cap_ms: 10_000,
            cef_alive_every_ms: 2_000,
            cef_hang_after_ms: 10_000,
            cef_rss_cap_mb: 600,
            ladder_overruns_to_step: 3,
            ladder_window_ms: 1_000,
            drain_timeout_ms: 7_000,
            final_hold_ms: 10_000,
            heartbeat_interval_ms: 5_000,
            bootstrap_backoff_base_ms: 1_000,
            bootstrap_backoff_cap_ms: 10_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputProfile {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub video_kbps: u32,
}

impl OutputProfile {
    /// ADR 0005 default. 4000 kbps = top of YouTube's 720p30 range; F-005 measured it real time at
    /// 0.83 of 4 cores.
    pub const P720: Self = Self {
        width: 1280,
        height: 720,
        fps: 30,
        video_kbps: 4_000,
    };
    /// ADR 0005 flag. 6000 kbps = the POC default; F-005 measured it real time at 1.11 of 4 cores.
    pub const P1080: Self = Self {
        width: 1920,
        height: 1080,
        fps: 30,
        video_kbps: 6_000,
    };

    pub fn frame_period_ms(&self) -> f64 {
        1_000.0 / f64::from(self.fps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // These pin the declarations to their SOURCES (PDF / spec / ADR / F-id), not to the code that reads
    // them: every later task takes its expected values from `Thresholds::default()`, so a wrong number
    // here would be frozen in as right everywhere else.

    const BOTH_MODES: [OutputProfile; 2] = [OutputProfile::P720, OutputProfile::P1080];

    #[test]
    fn programme_thresholds_match_the_pdf_state_machine() {
        let t = Thresholds::default();
        assert_eq!(
            (t.frozen_after_ms, t.slate_after_ms, t.resume_after_ms),
            (500, 3_000, 1_000)
        );
    }

    // In both modes (ADR 0005) a tick may land one frame late, and the slate is still on screen by 3.1 s.
    #[test]
    fn slate_lands_inside_the_3_1_s_budget_with_one_frame_of_tick_jitter() {
        let t = Thresholds::default();
        for p in BOTH_MODES {
            let worst = t.slate_after_ms as f64 + p.frame_period_ms();
            assert!(worst <= 3_100.0, "{p:?}: slate at {worst} ms");
        }
    }

    #[test]
    fn input_stall_rebuild_comes_after_the_slate() {
        let t = Thresholds::default();
        assert!(
            t.input_stall_after_ms > t.slate_after_ms,
            "viewers see the slate before the input is torn down"
        );
    }

    #[test]
    fn output_backoff_cap_fits_inside_the_5_s_reconnect_budget() {
        let t = Thresholds::default();
        assert!(t.output_backoff_cap_ms < 5_000);
    }

    // ADR 0005 and the plan: 1280×720 at 30 fps and 4000 kbps by default; 1920×1080 at 30 fps and
    // 6000 kbps behind the flag.
    #[test]
    fn profiles_are_adr_0005() {
        let p = OutputProfile::P720;
        assert_eq!((p.width, p.height, p.fps, p.video_kbps), (1280, 720, 30, 4_000));
        let p = OutputProfile::P1080;
        assert_eq!((p.width, p.height, p.fps, p.video_kbps), (1920, 1080, 30, 6_000));
    }

    // 30 fps in both modes (ADR 0005): 30 frame periods fill exactly one second.
    #[test]
    fn frame_period_is_a_thirtieth_of_a_second_in_both_modes() {
        for p in BOTH_MODES {
            let thirty = p.frame_period_ms() * 30.0;
            assert!(
                (thirty - 1_000.0).abs() < 1e-9,
                "{p:?}: 30 frames span {thirty} ms"
            );
        }
    }

    // Each number against its own source. The plan's Global Constraints: input back-off 1 s → 10 s; a
    // kick alert at MORE than 3 kicks in 5 min; output stall after 5 s (the plan says "no bytes acked";
    // F-005 makes it no bytes sent); a heartbeat every 5000 ms; CEF recycled every 2 h. Spec §4.2: CEF
    // Failed after 3 failures in 5 min. The C02 brief default: output back-off 0.5 s → 2 s, sized to
    // the PDF's 5 s reconnect budget.
    #[test]
    fn link_and_lifecycle_numbers_match_their_sources() {
        let t = Thresholds::default();
        assert_eq!(
            (t.input_backoff_base_ms, t.input_backoff_cap_ms),
            (1_000, 10_000),
            "input back-off"
        );
        assert_eq!((t.kick_alert_count, t.kick_window_ms), (3, 300_000), "kick alert");
        assert_eq!(t.output_stall_after_ms, 5_000, "output stall");
        assert_eq!(
            (t.output_backoff_base_ms, t.output_backoff_cap_ms),
            (500, 2_000),
            "output back-off"
        );
        assert_eq!(t.heartbeat_interval_ms, 5_000, "heartbeat");
        assert_eq!(
            (t.cef_fail_count, t.cef_fail_window_ms),
            (3, 300_000),
            "CEF failed"
        );
        assert_eq!(t.cef_recycle_every_ms, 7_200_000, "CEF recycle");
        assert_eq!(t.cef_rss_cap_mb, 600, "CEF memory cap (spec §6)");
    }

    // PDF "loads in under 2 s" with a ×5 margin; F-001 measured cefsrc start → first buffer at 2.804 s
    // at worst, which must not count as a failure.
    #[test]
    fn cef_load_timeout_is_five_pdf_loads_and_clears_the_f001_first_buffer() {
        let t = Thresholds::default();
        assert_eq!(t.cef_load_timeout_ms, 5 * 2_000);
        assert!(t.cef_load_timeout_ms > 2_804);
    }

    // Owed 18 (owner, 2026-10-18, "2 Ok"): the page beats every 2 s (R2005), and five missed beats
    // are a hang, the page's own SCORE_STALE_AFTER_MS (spec §6). Against the sources, not each other
    // alone: 5 × 2 000 = 10 000. A hang deadline at or past the recycle could never fire before it.
    #[test]
    fn the_hang_threshold_is_five_beats() {
        let t = Thresholds::default();
        assert_eq!(t.cef_alive_every_ms, 2_000, "R2005's beat");
        assert_eq!(t.cef_hang_after_ms, 5 * t.cef_alive_every_ms, "five beats");
        assert_eq!(t.cef_hang_after_ms, 10_000, "the page's SCORE_STALE_AFTER_MS");
        assert!(
            t.cef_hang_after_ms < t.cef_recycle_every_ms,
            "a hang is detected before the planned recycle"
        );
    }

    // F-002 and F-004 rerun 1: the slowest healthy connect on CF SRT playback was a first decoded
    // frame at 4.19 s and a first AU at 4.4 s. A timeout at or below that tears down a healthy input.
    #[test]
    fn input_connect_timeout_clears_the_slowest_measured_first_frame() {
        let t = Thresholds::default();
        assert!(
            t.input_connect_timeout_ms > 4_400,
            "{} ms",
            t.input_connect_timeout_ms
        );
    }

    // C10: the SRT latency is the one every F-002 reader used (1 000 ms), so the measured first-frame
    // times and the in-chain overlay delay (ADR 0006's default) hold for it. A different value owes a
    // re-measure and moves the overlay delay with it.
    #[test]
    fn input_srt_latency_is_the_one_f002_measured() {
        assert_eq!(Thresholds::default().input_srt_latency_ms, 1_000);
    }

    // F-004 addendum: YouTube split the organiser's broadcast after a 30 s output gap and kept it
    // through 15 s, so every recovery path must finish its gap under 15 s. The output's own worst
    // single-attempt recovery: the stall is detected, one capped back-off, then a connect that uses its
    // whole timeout.
    #[test]
    fn output_stall_and_one_reconnect_fit_inside_youtubes_15_s_split() {
        let t = Thresholds::default();
        let gap = t.output_stall_after_ms + t.output_backoff_cap_ms + t.output_connect_timeout_ms;
        assert!(gap < OUTPUT_GAP_BUDGET_MS, "output recovery gap {gap} ms");
    }

    // F-011 (Owed 38): a refused publish counted bytes until its refusal arrived, the last total at
    // 538 ms on CF (a made-up key) and the error at 2 033 ms on mediamtx (a denied path). Connected
    // must wait past both with at least twice their length to spare; the ruling set 5 s.
    #[test]
    fn output_connected_bound_clears_every_measured_refusal_twice_over() {
        let t = Thresholds::default();
        assert_eq!(
            t.output_connected_after_ms, 5_000,
            "the coordinator's ruling, from F-011"
        );
        for (leg, refused_by) in [("CF, a made-up key", 538), ("mediamtx, a denied path", 2_033)] {
            assert!(
                t.output_connected_after_ms >= 2 * refused_by,
                "{leg}: refused by {refused_by} ms, bound {} ms",
                t.output_connected_after_ms
            );
        }
    }

    // F-004: the same broadcast survived a 15 s gap, and a 30 s gap split it.
    #[test]
    fn output_gap_budget_is_the_15_s_youtube_kept_in_f004() {
        assert_eq!(OUTPUT_GAP_BUDGET_MS, 15_000);
    }

    // The C00 cost gate is "encoder speed ≥ 0.95"; F-005's worst 10 s window (0.9963) and a healthy
    // 29-frame second at 30 fps must both read as real time.
    #[test]
    fn encoder_realtime_floor_is_the_c00_gate_and_passes_a_29_frame_second() {
        let floor = ENCODER_SPEED_REALTIME_MIN;
        assert!((floor - 0.95).abs() < 1e-12, "C00 gate, got {floor}");
        assert!(29.0 / 30.0 >= floor, "a 29-frame second fails a {floor} floor");
        assert!(0.9963 >= floor, "F-005's worst window fails a {floor} floor");
    }
}
