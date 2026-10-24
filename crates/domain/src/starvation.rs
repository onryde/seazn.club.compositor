//! The output's starvation alarm (Owed 86; the owner's "Ok", 2026-10-24, in chat, relayed by the
//! coordinator, to the coordinator's recommendation that the beat report the real output frame rate
//! and raise an alarm when it falls below a floor, with no ladder change).
//!
//! Fed once a beat with the programme's real output rate: the video frames that left the encoder over
//! the beat window, per second (the adapters' `BeatRate`), never the profile's nominal `fps`. It is
//! raised by a beat below `OutputProfile::fps_floor` and, once raised, cleared by a beat at or above
//! `OutputProfile::fps_recovered`: a band, so a rate hovering at the floor does not raise and clear on
//! alternate beats. Each change is reported once. It decides nothing else: the ladder's input (the
//! encoder queue's overruns, C09) and its rule are untouched, and the driver only logs the change.
use crate::config::OutputProfile;

/// A change of the alarm's state, reported on the beat that made it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alarm {
    Raised,
    Cleared,
}

/// Down until a starved beat; see the module doc.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StarvationAlarm {
    raised: bool,
}

impl StarvationAlarm {
    pub fn is_raised(&self) -> bool {
        self.raised
    }

    /// One beat's real output rate against `profile`'s levels. A rate that is not a measurement (not
    /// finite, or negative: the contract's `minimum: 0`) changes nothing; a measured 0 is starved.
    pub fn on_reading(&mut self, fps: f64, profile: &OutputProfile) -> Option<Alarm> {
        if !fps.is_finite() || fps < 0.0 {
            return None;
        }
        if !self.raised && fps < profile.fps_floor() {
            self.raised = true;
            return Some(Alarm::Raised);
        }
        if self.raised && fps >= profile.fps_recovered() {
            self.raised = false;
            return Some(Alarm::Cleared);
        }
        None
    }
}
