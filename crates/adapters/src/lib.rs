//! Live adapters: GStreamer, CEF, HTTP, /proc.
pub mod control_http;
pub mod redact;

/// AGENTS.md §7: every URL or error text that may be logged goes through this.
pub use redact::redact;
