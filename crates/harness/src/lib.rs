#![allow(clippy::print_stdout)] // harness tools report on stdout by design (C13)
//! The harness (C13, `docs/tasks/C13-harness.md`): the real relay image runs against fakes of
//! everything around it under `harness/compose.yml`, and the reference checker turns what was
//! recorded into facts. C14's chaos cases and C15's soak are scripts over it.
//!
//! - [`strip`]: the camera's burned-in counter, the checker's ground truth;
//! - [`score`]: the overlay page's score strip;
//! - [`audio`]: the camera's tone bursts, and the scan that finds them in the programme;
//! - [`camera`]: fake-camera's controls and schedule;
//! - [`control`]: fake-control's routes (the session, the heartbeats, the score feed, the page);
//! - [`inputs`] and [`checker`]: the checker's inputs and its analysis;
//! - [`media`] (feature `media`): everything that runs GStreamer.
//!
//! The harness reads `domain::config::Thresholds` (the one authority for the programme's numbers)
//! and nothing else of the domain.
pub mod audio;
pub mod camera;
pub mod checker;
pub mod clock;
pub mod control;
pub mod inputs;
#[cfg(feature = "media")]
pub mod media;
pub mod score;
pub mod strip;
