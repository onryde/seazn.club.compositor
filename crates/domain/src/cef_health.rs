//! CEF's health, as the graphics adapter reads it, and the one rule that turns it into an in-place
//! restart (C11b; owner ruling 2026-10-19 "B"; Owed 55, 56, 61). The adapter only counts; this module
//! decides, and `Supervisor` acts on the decision (the C10b / Owed 50 lesson: one authority, never a
//! second copy of the rule in the adapter).
use crate::config::Thresholds;

/// What the graphics adapter knows that the domain's per-page events cannot say: facts about CEF,
/// which is one per PROCESS (F-013), not about a page. C08 reads it once per tick and passes it in
/// as `Event::CefHealth`; `Default` is a healthy process.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CefHealth {
    /// `Some(gen)`: a CEF zygote is gone (Owed 56; `gen` is the newest build's generation when the
    /// adapter saw it, for the log only). The renderers' zygote: CEF can start no renderer again in
    /// this process. The GPU process's zygote: the next GPU-process death aborts the whole process
    /// (Chromium's FATAL "GPU process isn't usable. Goodbye.", SIGTRAP; F-013).
    pub unrecoverable: Option<u64>,
    /// Retired builds whose teardown has still not returned after the reaper's kill (Owed 55): a
    /// live count, each holding a thread, a pipeline and a renderer.
    pub leaked_teardowns: usize,
    /// GPU processes this process's CEF has lost (Owed 61): a count over the process's life, since
    /// Chromium aborts at its `CHROMIUM_GPU_DEATHS_ABORT`-th (F-013), not at a rate.
    pub gpu_process_deaths: usize,
}

/// Why the process restarts in place: the most urgent cause, in `restart_cause`'s order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartCause {
    ZygoteGone { gen: u64 },
    GpuProcessDeaths { count: usize },
    LeakedTeardowns { live: usize },
}

impl RestartCause {
    /// The final beat's `exitDetail` (at most 100 characters, the wire's cap; no page URL, no token:
    /// only counts and a generation).
    pub fn detail(self) -> String {
        match self {
            RestartCause::ZygoteGone { gen } => {
                format!("restart in place: a CEF zygote is gone (gen {gen})")
            }
            RestartCause::GpuProcessDeaths { count } => {
                format!("restart in place: {count} CEF GPU-process deaths")
            }
            RestartCause::LeakedTeardowns { live } => {
                format!("restart in place: {live} CEF teardowns leaked")
            }
        }
    }
}

impl CefHealth {
    /// The restart this health asks for, if any (owner ruling B; F-013). In urgency order:
    /// - a dead zygote, whatever generation it names: CEF is dead for every later page of the
    ///   process, and with the GPU zygote gone the next GPU-process death aborts the process, so the
    ///   fallback of staying on air without the overlay is not safe there (the final C11 review's G3);
    /// - `cef_gpu_deaths_to_restart` GPU-process deaths: each brings Chromium's abort closer;
    /// - `cef_leaks_to_restart` teardowns leaked at once: memory only, the least urgent.
    pub fn restart_cause(&self, t: &Thresholds) -> Option<RestartCause> {
        if let Some(gen) = self.unrecoverable {
            return Some(RestartCause::ZygoteGone { gen });
        }
        if self.gpu_process_deaths >= t.cef_gpu_deaths_to_restart {
            return Some(RestartCause::GpuProcessDeaths {
                count: self.gpu_process_deaths,
            });
        }
        if self.leaked_teardowns >= t.cef_leaks_to_restart {
            return Some(RestartCause::LeakedTeardowns {
                live: self.leaked_teardowns,
            });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The largest values each field can carry still fit the wire's 100 characters.
    #[test]
    fn every_detail_fits_the_wire() {
        for c in [
            RestartCause::ZygoteGone { gen: u64::MAX },
            RestartCause::GpuProcessDeaths { count: usize::MAX },
            RestartCause::LeakedTeardowns { live: usize::MAX },
        ] {
            let d = c.detail();
            assert!(d.chars().count() <= 100, "{d}");
            assert!(d.starts_with("restart in place: "), "{d}");
        }
    }
}
