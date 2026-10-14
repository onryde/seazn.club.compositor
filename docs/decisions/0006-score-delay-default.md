# 0006. Score delay: measured default plus per-fixture override; SEI probe in the spike

- **Status:** accepted (owner, 2026-10-14)

## Context

Through Cloudflare, video reaches the compositor about 2–4 s after the event, while score events
arrive in real time. With a delay of 0, the bug spoils each point before viewers see it.

The capture app's `deliveredLagS` cannot calibrate this, for two reasons:

- It is relative lag growth (`lagMs − minLag` on the HLS manifest, see
  `seazn.club.capture/.../DeliveryWatch.kt`), not absolute delay.
- It is measured on a different path.

## Decision

- **Default delay:** the camera→compositor delay measured in spike T00.2 with a clock in shot.
- **Override:** a new nullable per-fixture `fixture_stream_settings.overlay_delay_ms`, editable from
  the organiser UI. It is copied onto the session row at create time. See spec §7: the existing
  session column is never written.
- **Calibration UX:** moves to R4.
- **Spike T00.3:** test whether an H.264 SEI wall-clock stamp survives CF Input A to SRT playback.
  - If it survives, an R3 capture change stamps NTP time into keyframes. The compositor then measures
    the delay per frame automatically, and the R4 calibration UX is dropped.
  - If CF strips it, this ADR stands permanently.

## Rejected

- **Auto-tune from SRT stats.** There is no ground truth: those stats miss the phone→CF leg, which is
  often the largest part of the delay.
- **Default 0.** Every uncalibrated match would spoil points.
