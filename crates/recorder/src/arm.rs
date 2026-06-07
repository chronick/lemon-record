//! Per-track arm state machine.
//!
//! Each track decides, frame-block by frame-block, whether it is *live* (should
//! be writing audio). The decision is pure given the track's peak level and the
//! elapsed time — no I/O — so it's exhaustively unit-testable without hardware.
//!
//! - [`ArmMode::Off`] → never live (silent/unused inputs write no dead WAVs).
//! - [`ArmMode::On`] → always live for the session.
//! - [`ArmMode::Auto`] → opens when peak ≥ threshold, closes after `silence_ms`
//!   of continuous sub-threshold signal. This is what makes auto-arming skip
//!   silent channels: a track that never crosses threshold never goes live.

use crate::config::ArmMode;

/// Runtime arming state for one track. Construct from the track's config, then
/// feed it level updates as audio arrives.
#[derive(Debug, Clone)]
pub struct ArmState {
    mode: ArmMode,
    threshold_db: f32,
    silence_ms: u32,
    /// Whether the track is currently capturing.
    live: bool,
    /// Whether this track has *ever* gone live this session — used to decide if
    /// a WAV file should exist at all (no dead files for never-triggered Auto).
    ever_live: bool,
    /// Accumulated continuous sub-threshold time, in ms.
    ms_below: u32,
}

impl ArmState {
    pub fn new(mode: ArmMode, threshold_db: f32, silence_ms: u32) -> Self {
        Self {
            mode,
            threshold_db,
            silence_ms,
            // `On` starts live immediately; `Auto`/`Off` start closed.
            live: mode == ArmMode::On,
            ever_live: mode == ArmMode::On,
            ms_below: 0,
        }
    }

    pub fn is_live(&self) -> bool {
        self.live
    }

    /// True if this track has produced (or will produce) audio this session.
    /// `Off` tracks and `Auto` tracks that never crossed threshold report
    /// false, so the session can skip creating their WAV files.
    pub fn ever_live(&self) -> bool {
        self.ever_live
    }

    pub fn mode(&self) -> ArmMode {
        self.mode
    }

    /// Update with the peak level (dBFS) of a block spanning `dt_ms` and return
    /// whether the track is live *after* this block. `On`/`Off` ignore the
    /// level; `Auto` runs the open/close hysteresis.
    pub fn update(&mut self, peak_db: f32, dt_ms: u32) -> bool {
        match self.mode {
            ArmMode::Off => {
                self.live = false;
            }
            ArmMode::On => {
                self.live = true;
                self.ever_live = true;
            }
            ArmMode::Auto => {
                if peak_db >= self.threshold_db {
                    self.live = true;
                    self.ever_live = true;
                    self.ms_below = 0;
                } else if self.live {
                    self.ms_below = self.ms_below.saturating_add(dt_ms);
                    if self.ms_below >= self.silence_ms {
                        self.live = false;
                    }
                }
            }
        }
        self.live
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SILENCE_MS: u32 = 2000;
    const THRESH: f32 = -40.0;

    #[test]
    fn off_is_never_live() {
        let mut s = ArmState::new(ArmMode::Off, THRESH, SILENCE_MS);
        assert!(!s.is_live());
        assert!(!s.update(0.0, 100)); // even a hot signal stays off
        assert!(!s.update(-3.0, 100));
        assert!(!s.ever_live());
    }

    #[test]
    fn on_is_always_live() {
        let mut s = ArmState::new(ArmMode::On, THRESH, SILENCE_MS);
        assert!(s.is_live());
        assert!(s.update(-96.0, 100)); // stays live through silence
        assert!(s.ever_live());
    }

    #[test]
    fn auto_starts_closed() {
        let s = ArmState::new(ArmMode::Auto, THRESH, SILENCE_MS);
        assert!(!s.is_live());
        assert!(!s.ever_live());
    }

    #[test]
    fn auto_opens_above_threshold() {
        let mut s = ArmState::new(ArmMode::Auto, THRESH, SILENCE_MS);
        assert!(s.update(-30.0, 100)); // -30 > -40 → open
        assert!(s.is_live());
        assert!(s.ever_live());
    }

    #[test]
    fn auto_stays_closed_for_silent_channel() {
        // The silent-channel case: a track that never crosses threshold never
        // goes live and never reports ever_live — so no dead WAV is written.
        let mut s = ArmState::new(ArmMode::Auto, THRESH, SILENCE_MS);
        for _ in 0..1000 {
            assert!(!s.update(-60.0, 100));
        }
        assert!(!s.ever_live());
    }

    #[test]
    fn auto_holds_open_through_brief_dips() {
        let mut s = ArmState::new(ArmMode::Auto, THRESH, SILENCE_MS);
        s.update(-10.0, 100); // open
                              // 1.5s of silence — less than the 2s window
        for _ in 0..15 {
            assert!(s.update(-60.0, 100), "should hold open during brief dip");
        }
    }

    #[test]
    fn auto_closes_after_silence_window() {
        let mut s = ArmState::new(ArmMode::Auto, THRESH, SILENCE_MS);
        s.update(-10.0, 100); // open
                              // exactly 2s of continuous silence → close
        let mut last = true;
        for _ in 0..20 {
            last = s.update(-60.0, 100);
        }
        assert!(!last, "should close once silence_ms is reached");
        assert!(!s.is_live());
        assert!(s.ever_live(), "ever_live stays true after closing");
    }

    #[test]
    fn auto_silence_counter_resets_on_resurgence() {
        let mut s = ArmState::new(ArmMode::Auto, THRESH, SILENCE_MS);
        s.update(-10.0, 100); // open
        for _ in 0..15 {
            s.update(-60.0, 100); // 1.5s silence (not yet closed)
        }
        s.update(-10.0, 100); // signal returns → counter resets
        for _ in 0..15 {
            assert!(s.update(-60.0, 100), "1.5s after reset should still be open");
        }
    }

    #[test]
    fn auto_threshold_boundary_is_inclusive() {
        let mut s = ArmState::new(ArmMode::Auto, THRESH, SILENCE_MS);
        assert!(s.update(THRESH, 100), "peak == threshold should open");
    }

    #[test]
    fn auto_reopens_after_closing() {
        let mut s = ArmState::new(ArmMode::Auto, THRESH, SILENCE_MS);
        s.update(-10.0, 100);
        for _ in 0..25 {
            s.update(-60.0, 100); // close
        }
        assert!(!s.is_live());
        assert!(s.update(-5.0, 100), "new signal reopens the track");
    }
}
