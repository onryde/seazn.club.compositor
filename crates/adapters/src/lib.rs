//! Live adapters: GStreamer, CEF, HTTP, /proc.
pub mod cef_graphics;
pub mod control_http;
pub mod frame_store;
pub mod gst_input;
pub mod gst_output;
pub mod gst_programme;
mod input_threads;
pub mod metrics_proc;
pub mod phase_http;
pub mod redact;
pub mod slate;
pub mod stats;

/// AGENTS.md §7: every URL or error text that may be logged goes through this.
pub use redact::redact;
