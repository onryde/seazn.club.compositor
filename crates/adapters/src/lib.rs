//! Live adapters: GStreamer, CEF, HTTP, /proc.
pub mod control_http;
pub mod frame_store;
pub mod gst_output;
pub mod gst_programme;
pub mod redact;
pub mod slate;
pub mod stats;

/// AGENTS.md §7: every URL or error text that may be logged goes through this.
pub use redact::redact;
