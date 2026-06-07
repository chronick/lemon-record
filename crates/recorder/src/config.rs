//! File-backed, bidirectional recorder configuration.
//!
//! The config file at `~/.music-hub-data/sample-recorder-config.json` is the
//! *canonical interface*: the GUI populates its controls from it on launch and
//! writes changes straight back, and the same file is editable by hand, by the
//! CLI, and by the agent. One source of truth — no private in-memory state.
//!
//! Arming model: the **master** (the summed mix of all non-muted tracks) carries
//! the arm mode + threshold. When the master crosses the threshold the whole
//! session captures; it stops after `master_timeout_ms` below it. Individual
//! tracks are a basic mixer: each has a **mute** and a **volume** that scales its
//! contribution to the master. The master mix is always written; per-track stems
//! are written too whenever more than one track is active.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;

/// How the master decides whether the session is capturing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ArmMode {
    /// Never captures.
    Off,
    /// Always captures for the whole session.
    On,
    /// Captures only while the master is above `master_threshold_db`, stopping
    /// after `master_timeout_ms` of continuous sub-threshold signal. The default.
    #[default]
    Auto,
}

/// One track: a name, the input channel(s) feeding it, and its mixer state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackConfig {
    /// Filename stem for this track's WAV (sanitized at write time).
    pub name: String,
    /// Input channel indices feeding this track (0-based). One index = mono
    /// track, two = stereo pair. Indices beyond the open device are skipped.
    pub channels: Vec<u16>,
    /// Muted tracks are excluded from the master mix *and* not recorded.
    #[serde(default)]
    pub muted: bool,
    /// Linear gain applied to this track's contribution to the master mix.
    /// Stems are recorded raw (unity); volume is a non-destructive mix control.
    #[serde(default = "default_volume")]
    pub volume: f32,
}

impl TrackConfig {
    /// A mono track on a single input channel, named `track-NN`.
    pub fn mono(index: u16) -> Self {
        Self {
            name: format!("track-{:02}", index + 1),
            channels: vec![index],
            muted: false,
            volume: default_volume(),
        }
    }

    /// A stereo track on an adjacent input pair, named `stereo-NN`.
    pub fn stereo(left: u16) -> Self {
        Self {
            name: format!("stereo-{:02}", left / 2 + 1),
            channels: vec![left, left + 1],
            muted: false,
            volume: default_volume(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecorderConfig {
    pub sample_rate: u32,
    pub bit_depth: u16,
    pub output_dir: String,
    pub default_device: Option<String>,

    /// Base name for a recording. When blank, the recorder auto-generates a
    /// heroku-style name (e.g. `amber-cascade`) per session.
    #[serde(default, alias = "session_name")]
    pub recording_name: String,

    /// Master arm mode — drives whether the session is capturing.
    #[serde(default)]
    pub master_arm: ArmMode,

    /// Peak level (dBFS) above which `Auto` starts the master capturing.
    #[serde(default = "default_threshold_db", alias = "threshold_db")]
    pub master_threshold_db: f32,

    /// Continuous time (ms) the master can stay below threshold before `Auto`
    /// stops capturing. Defaults to 5 s.
    #[serde(
        default = "default_master_timeout_ms",
        alias = "silence_ms",
        alias = "arm_silence_ms"
    )]
    pub master_timeout_ms: u32,

    /// The track layout. When empty, the recorder derives a default mono track
    /// per device input channel at session start.
    #[serde(default)]
    pub tracks: Vec<TrackConfig>,
}

fn default_threshold_db() -> f32 {
    -40.0
}
fn default_master_timeout_ms() -> u32 {
    5000
}
fn default_volume() -> f32 {
    1.0
}

fn default_output_dir() -> String {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".music-hub-data")
        .join("recordings")
        .to_string_lossy()
        .to_string()
}

impl Default for RecorderConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            bit_depth: 24,
            output_dir: default_output_dir(),
            default_device: None,
            recording_name: String::new(),
            master_arm: ArmMode::default(),
            master_threshold_db: default_threshold_db(),
            master_timeout_ms: default_master_timeout_ms(),
            tracks: Vec::new(),
        }
    }
}

impl RecorderConfig {
    /// Build the default per-channel track layout for a device with
    /// `channel_count` inputs: one mono track per channel. `stereo` pairs
    /// adjacent channels instead.
    pub fn default_tracks(channel_count: u16, stereo: bool) -> Vec<TrackConfig> {
        if stereo {
            (0..channel_count)
                .step_by(2)
                .filter(|&l| l + 1 < channel_count)
                .map(TrackConfig::stereo)
                .collect()
        } else {
            (0..channel_count).map(TrackConfig::mono).collect()
        }
    }

    /// The effective track layout for a device: the configured tracks, or a
    /// derived mono-per-channel layout when none are configured.
    pub fn effective_tracks(&self, channel_count: u16) -> Vec<TrackConfig> {
        if self.tracks.is_empty() {
            Self::default_tracks(channel_count, false)
        } else {
            self.tracks.clone()
        }
    }
}

/// Thread-safe, file-backed config handle. Reads on construction, writes back
/// on every `set`/`update` — the file stays the source of truth.
pub struct RecorderConfigState {
    config: Mutex<RecorderConfig>,
    config_path: PathBuf,
}

impl RecorderConfigState {
    pub fn new() -> Self {
        Self::with_path(default_config_path())
    }

    pub fn with_path(config_path: PathBuf) -> Self {
        let config = if config_path.exists() {
            std::fs::read_to_string(&config_path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            RecorderConfig::default()
        };
        Self {
            config: Mutex::new(config),
            config_path,
        }
    }

    pub fn path(&self) -> &PathBuf {
        &self.config_path
    }

    fn save(&self, config: &RecorderConfig) -> Result<(), String> {
        if let Some(parent) = self.config_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(config).map_err(|e| e.to_string())?;
        std::fs::write(&self.config_path, json).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn get(&self) -> Result<RecorderConfig, String> {
        Ok(self.config.lock().map_err(|e| e.to_string())?.clone())
    }

    pub fn set(&self, config: RecorderConfig) -> Result<(), String> {
        self.save(&config)?;
        *self.config.lock().map_err(|e| e.to_string())? = config;
        Ok(())
    }

    pub fn update<F: FnOnce(&mut RecorderConfig)>(&self, f: F) -> Result<RecorderConfig, String> {
        let mut guard = self.config.lock().map_err(|e| e.to_string())?;
        f(&mut guard);
        self.save(&guard)?;
        Ok(guard.clone())
    }
}

impl Default for RecorderConfigState {
    fn default() -> Self {
        Self::new()
    }
}

fn default_config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".music-hub-data")
        .join("sample-recorder-config.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_auto_master() {
        let c = RecorderConfig::default();
        assert_eq!(c.sample_rate, 48000);
        assert_eq!(c.bit_depth, 24);
        assert_eq!(c.master_arm, ArmMode::Auto);
        assert_eq!(c.master_threshold_db, -40.0);
        assert_eq!(c.master_timeout_ms, 5000);
        assert!(c.recording_name.is_empty());
        assert!(c.tracks.is_empty());
    }

    #[test]
    fn default_tracks_are_unmuted_unity() {
        let tracks = RecorderConfig::default_tracks(4, false);
        assert_eq!(tracks.len(), 4);
        assert_eq!(tracks[0].channels, vec![0]);
        assert!(!tracks[0].muted);
        assert_eq!(tracks[0].volume, 1.0);
        assert_eq!(tracks[0].name, "track-01");
    }

    #[test]
    fn default_tracks_stereo_pairs_channels() {
        let tracks = RecorderConfig::default_tracks(12, true);
        assert_eq!(tracks.len(), 6);
        assert_eq!(tracks[0].channels, vec![0, 1]);
        assert_eq!(tracks[5].channels, vec![10, 11]);
    }

    #[test]
    fn effective_tracks_derives_when_empty() {
        assert_eq!(RecorderConfig::default().effective_tracks(4).len(), 4);
    }

    #[test]
    fn config_roundtrips_through_json() {
        let mut c = RecorderConfig::default();
        c.bit_depth = 16;
        c.recording_name = "live-jam".into();
        c.master_arm = ArmMode::On;
        c.master_threshold_db = -30.0;
        c.master_timeout_ms = 3000;
        c.tracks = vec![
            TrackConfig {
                name: "kick".into(),
                channels: vec![0],
                muted: true,
                volume: 0.5,
            },
            TrackConfig::stereo(2),
        ];
        let json = serde_json::to_string(&c).unwrap();
        let back: RecorderConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.recording_name, "live-jam");
        assert_eq!(back.master_arm, ArmMode::On);
        assert_eq!(back.master_threshold_db, -30.0);
        assert_eq!(back.master_timeout_ms, 3000);
        assert!(back.tracks[0].muted);
        assert_eq!(back.tracks[0].volume, 0.5);
        assert_eq!(back.tracks[1].channels, vec![2, 3]);
    }

    #[test]
    fn old_config_still_loads_with_defaults() {
        // Pre-master config: per-track arm/threshold + silence_ms are ignored;
        // session_name/arm_silence_ms alias into the new fields.
        let old = r#"{
            "sample_rate": 48000,
            "bit_depth": 24,
            "output_dir": "/tmp/rec",
            "default_device": null,
            "arm_silence_ms": 1500,
            "session_name": "old-set",
            "tracks": [{"name":"t","channels":[0],"arm":"on","threshold_db":-32.0}]
        }"#;
        let c: RecorderConfig = serde_json::from_str(old).unwrap();
        assert_eq!(c.master_timeout_ms, 1500); // aliased from arm_silence_ms
        assert_eq!(c.recording_name, "old-set");
        // Old per-track arm/threshold are dropped; mute/volume default in.
        assert_eq!(c.tracks.len(), 1);
        assert!(!c.tracks[0].muted);
        assert_eq!(c.tracks[0].volume, 1.0);
    }

    #[test]
    fn arm_mode_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&ArmMode::Auto).unwrap(), "\"auto\"");
        assert_eq!(serde_json::to_string(&ArmMode::Off).unwrap(), "\"off\"");
        assert_eq!(serde_json::to_string(&ArmMode::On).unwrap(), "\"on\"");
    }

    #[test]
    fn state_persists_and_reloads_from_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("cfg.json");
        let state = RecorderConfigState::with_path(path.clone());
        state
            .update(|c| {
                c.recording_name = "pinned".into();
                c.tracks = vec![TrackConfig::mono(3)];
            })
            .unwrap();
        assert!(path.exists());
        let reloaded = RecorderConfigState::with_path(path);
        let c = reloaded.get().unwrap();
        assert_eq!(c.recording_name, "pinned");
        assert_eq!(c.tracks[0].channels, vec![3]);
    }

    #[test]
    fn missing_file_yields_default() {
        let tmp = tempfile::tempdir().unwrap();
        let state = RecorderConfigState::with_path(tmp.path().join("nope.json"));
        assert_eq!(state.get().unwrap().sample_rate, 48000);
    }
}
