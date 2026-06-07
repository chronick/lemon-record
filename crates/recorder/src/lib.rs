//! LEMON record — multi-track capture core.
//!
//! A standalone, multi-track audio capture library: it opens a multichannel
//! input device, meters every channel, and records master-gated *takes* into a
//! per-session folder, writing a `session.json` manifest that downstream tools
//! read. No daemon, no IPC — the GUI binary drives this library directly, and it
//! couples to anything downstream only through files.
//!
//! Module map:
//! - [`config`] — file-backed config: device, track layout, master arm/threshold.
//! - [`arm`] — the on/off/auto arm state machine driving the master gate (pure).
//! - [`session`] — auto-segmenting take writer + per-take WAVs + manifest (pure I/O).
//! - [`capture`] — the cpal stream that feeds `session` and computes meters.
//! - [`metering`] / [`visualization`] — level + spectral DSP helpers.
//! - [`naming`] — filesystem-safe naming helpers.

pub mod arm;
pub mod capture;
pub mod config;
pub mod metering;
pub mod naming;
pub mod session;
pub mod visualization;

pub use capture::{list_input_devices, AudioDevice, Capture, CaptureStatus};
pub use config::{ArmMode, RecorderConfig, RecorderConfigState, TrackConfig};
pub use metering::ChannelLevel;
pub use session::{SessionManifest, SessionNaming, SessionWriter, TakeSummary, TrackLayout};

/// Format a local datetime (`YYYYMMDD-HHMMSS`) for the session folder name —
/// human-sortable so folders line up chronologically.
pub fn session_timestamp() -> String {
    chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
}

/// A short (6-char) unique token, used in every filename so each WAV is unique
/// no matter where it's later dragged.
pub fn short_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..6].to_string()
}

/// A random heroku-style recording name (e.g. `amber-cascade`) — the default
/// when the user hasn't pinned a name, so recordings are always identifiable
/// without anyone having to think about naming.
pub fn auto_recording_name() -> String {
    naming::heroku_style_stem(&uuid::Uuid::new_v4().simple().to_string())
}

/// Resolve the recording base name: the user's pinned name if non-blank, else a
/// fresh auto name. Lives here so the GUI and any headless caller agree.
pub fn resolve_recording_name(configured: &str) -> String {
    if configured.trim().is_empty() {
        auto_recording_name()
    } else {
        configured.to_string()
    }
}
