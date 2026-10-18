//! Process lifecycle and the exit-code contract (plan Global Constraints; R1 runner contract).
use crate::time::Ms;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExitReason {
    Ended,
    Deadline,
    Signalled,
    SessionGone,
    Refused,
    Fatal,
}

impl ExitReason {
    /// Every reason once, in the order the brief declares them (C07's wire maps each one).
    pub const ALL: [ExitReason; 6] = [
        ExitReason::Ended,
        ExitReason::Deadline,
        ExitReason::Signalled,
        ExitReason::SessionGone,
        ExitReason::Refused,
        ExitReason::Fatal,
    ];

    /// 0 = clean (the host does not restart it; ADR 0007 item 5). 78 = EX_CONFIG. 70 = EX_SOFTWARE.
    pub fn code(self) -> i32 {
        match self {
            ExitReason::Ended | ExitReason::Deadline | ExitReason::Signalled | ExitReason::SessionGone => 0,
            ExitReason::Refused => 78,
            ExitReason::Fatal => 70,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
    /// `retry_at: None` ⇒ a fetch is in flight.
    Booting {
        attempt: u32,
        retry_at: Option<Ms>,
    },
    Running,
    /// The final hold: the Final scene is on air for `final_hold_ms`, then the drain starts with
    /// `reason` (overlay templates spec 2026-10-15 §4.2, D4).
    Holding {
        since: Ms,
        reason: ExitReason,
    },
    Draining {
        since: Ms,
        reason: ExitReason,
    },
    Exited(ExitReason),
}

#[cfg(test)]
mod tests {
    use super::*;

    // The plan's Global Constraints: 0 for ended, deadline, signal or session gone (410); 78 for
    // unauthorized or not found (401/404) and a wrong mode; 70 for fatal.
    #[test]
    fn exit_codes_match_the_contract() {
        let table = [
            (ExitReason::Ended, 0),
            (ExitReason::Deadline, 0),
            (ExitReason::Signalled, 0),
            (ExitReason::SessionGone, 0),
            (ExitReason::Refused, 78),
            (ExitReason::Fatal, 70),
        ];
        for (r, c) in table {
            assert_eq!(r.code(), c, "{r:?}");
        }
        assert_eq!(table.len(), ExitReason::ALL.len(), "every reason, once");
        for (i, r) in ExitReason::ALL.into_iter().enumerate() {
            assert_eq!(table[i].0, r, "the table follows ALL");
        }
    }
}
