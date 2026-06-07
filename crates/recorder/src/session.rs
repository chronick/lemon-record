//! Multi-take session writer — a set-and-forget auto-segmenting recorder.
//!
//! A *session* is one armed period. Capture is gated by the **master** (the
//! summed mix of all non-muted tracks) running the arm state machine. Each time
//! the master goes live a new **take** starts recording; when it falls below the
//! threshold for the hold time the take is finalized and saved, the clock
//! resets, and the session stays armed for the next take. So you arm once and
//! every phrase you play becomes its own saved take.
//!
//! Output layout (one folder per armed session, one set of files per take):
//!
//! ```text
//! <output_dir>/<recording>-<datetime>/
//! ├── <recording>-<id>-t01-master.wav     # take 1 mix (always)
//! ├── <recording>-<id>-t01-<track>.wav    # take 1 stems (only when >1 track)
//! ├── <recording>-<id>-t02-master.wav     # take 2 …
//! └── session.json                        # manifest: every take, rewritten as they close
//! ```
//!
//! Per-track controls are a basic mixer: **mute** (excludes from master + stems)
//! and **volume** (scales the track's contribution to the master; stems are raw).
//! `On` mode captures one continuous take until stop; `Auto` segments takes by
//! the threshold; `Off` captures nothing.
//!
//! Frame-routing and WAV I/O are free of audio hardware, so the module is
//! exercised by feeding synthetic buffers and reading the WAVs back.

use crate::arm::ArmState;
use crate::config::{ArmMode, RecorderConfig, TrackConfig};
use crate::metering::linear_to_db;
use crate::naming::sanitize_stem;
use hound::{SampleFormat, WavSpec, WavWriter};
use serde::Serialize;
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

type Writer = WavWriter<BufWriter<File>>;

/// Resolved naming for a session (the capture/GUI layer generates these so this
/// module stays pure and deterministic).
#[derive(Debug, Clone)]
pub struct SessionNaming {
    pub recording: String,
    pub id: String,
    pub datetime: String,
}

/// The static track layout, recorded in the manifest.
#[derive(Debug, Clone, Serialize)]
pub struct TrackLayout {
    pub name: String,
    pub channels: Vec<u16>,
    pub muted: bool,
    pub volume: f32,
}

/// One captured stem within a take.
#[derive(Debug, Clone, Serialize)]
pub struct TakeStem {
    pub name: String,
    pub file: String,
}

/// One finalized take.
#[derive(Debug, Clone, Serialize)]
pub struct TakeSummary {
    pub index: u32,
    pub master: String,
    pub stems: Vec<TakeStem>,
    pub frames: u64,
}

impl TakeSummary {
    pub fn duration_secs(&self, sample_rate: u32) -> f64 {
        if sample_rate == 0 {
            0.0
        } else {
            self.frames as f64 / sample_rate as f64
        }
    }
}

/// Written to `session.json` — the durable description the CLI pipeline reads.
#[derive(Debug, Clone, Serialize)]
pub struct SessionManifest {
    pub recording: String,
    pub id: String,
    pub datetime: String,
    pub sample_rate: u32,
    pub bit_depth: u16,
    pub device: Option<String>,
    pub device_channels: u16,
    pub master_arm: ArmMode,
    pub tracks: Vec<TrackLayout>,
    pub takes: Vec<TakeSummary>,
}

impl SessionManifest {
    pub fn take_count(&self) -> usize {
        self.takes.len()
    }
    /// Total captured duration across all takes.
    pub fn duration_secs(&self) -> f64 {
        if self.sample_rate == 0 {
            return 0.0;
        }
        self.takes.iter().map(|t| t.frames).sum::<u64>() as f64 / self.sample_rate as f64
    }
}

struct TrackSlot {
    name: String,
    channels: Vec<u16>,
    muted: bool,
    volume: f32,
    spec: WavSpec,
    writer: Option<Writer>,
    frames: u64,
}

fn spec_for(channels: u16, sample_rate: u32, bit_depth: u16) -> WavSpec {
    WavSpec {
        channels,
        sample_rate,
        bits_per_sample: bit_depth,
        sample_format: if bit_depth == 32 {
            SampleFormat::Float
        } else {
            SampleFormat::Int
        },
    }
}

fn write_sample(w: &mut Writer, sample: f32, bit_depth: u16) -> Result<(), String> {
    let r = match bit_depth {
        16 => w.write_sample((sample * 32767.0).clamp(-32768.0, 32767.0) as i16),
        24 => w.write_sample((sample * 8388607.0).clamp(-8388608.0, 8388607.0) as i32),
        _ => w.write_sample(sample),
    };
    r.map_err(|e| format!("write sample: {}", e))
}

pub struct SessionWriter {
    recording: String,
    id: String,
    datetime: String,
    dir: PathBuf,
    sample_rate: u32,
    bit_depth: u16,
    device: Option<String>,
    device_channels: u16,
    master_arm: ArmState,
    master_mode: ArmMode,
    tracks: Vec<TrackSlot>,
    write_stems: bool,

    // --- current take state ---
    take_index: u32,
    was_live: bool,
    master_writer: Option<Writer>,
    master_frames: u64,

    // --- completed takes ---
    takes: Vec<TakeSummary>,
}

impl SessionWriter {
    pub fn create(
        config: &RecorderConfig,
        device: Option<String>,
        device_channels: u16,
        naming: &SessionNaming,
    ) -> Result<Self, String> {
        let recording = sanitize_stem(&naming.recording);
        let id = sanitize_stem(&naming.id);
        let folder = format!("{}-{}", recording, naming.datetime);
        let dir = Path::new(&config.output_dir).join(&folder);
        std::fs::create_dir_all(&dir).map_err(|e| format!("create session dir: {}", e))?;

        let layout = config.effective_tracks(device_channels);
        let tracks: Vec<TrackSlot> = layout
            .iter()
            .map(|t| Self::build_slot(t, config, device_channels))
            .collect();
        let active = tracks.iter().filter(|t| !t.muted && !t.channels.is_empty()).count();
        let write_stems = active > 1;

        Ok(Self {
            recording,
            id,
            datetime: naming.datetime.clone(),
            dir,
            sample_rate: config.sample_rate,
            bit_depth: config.bit_depth,
            device,
            device_channels,
            master_arm: ArmState::new(
                config.master_arm,
                config.master_threshold_db,
                config.master_timeout_ms,
            ),
            master_mode: config.master_arm,
            tracks,
            write_stems,
            take_index: 0,
            was_live: false,
            master_writer: None,
            master_frames: 0,
            takes: Vec::new(),
        })
    }

    fn build_slot(t: &TrackConfig, config: &RecorderConfig, device_channels: u16) -> TrackSlot {
        let channels: Vec<u16> = t
            .channels
            .iter()
            .copied()
            .filter(|&c| c < device_channels)
            .collect();
        let nch = channels.len().max(1) as u16;
        TrackSlot {
            name: t.name.clone(),
            channels,
            muted: t.muted,
            volume: t.volume,
            spec: spec_for(nch, config.sample_rate, config.bit_depth),
            writer: None,
            frames: 0,
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Frames written in the current take — resets to 0 when a take closes, so
    /// the UI clock shows per-take length.
    pub fn current_take_frames(&self) -> u64 {
        self.master_frames
    }

    /// Whether the master is currently capturing (a take is in progress).
    pub fn is_capturing(&self) -> bool {
        self.master_arm.is_live()
    }

    /// Number of takes finalized so far.
    pub fn takes_completed(&self) -> usize {
        self.takes.len()
    }

    fn take_file(&self, suffix: &str) -> String {
        format!("{}-{}-t{:02}-{}.wav", self.recording, self.id, self.take_index, sanitize_stem(suffix))
    }

    /// Feed one block of interleaved device audio. Opens/continues/closes takes
    /// based on the master arm gate.
    pub fn push_frames(&mut self, interleaved: &[f32]) -> Result<(), String> {
        let dc = self.device_channels.max(1) as usize;
        if interleaved.is_empty() || interleaved.len() < dc {
            return Ok(());
        }
        let nframes = interleaved.len() / dc;
        let dt_ms = (((nframes as u64 * 1000) / self.sample_rate.max(1) as u64) as u32).max(1);
        let bit_depth = self.bit_depth;

        // Build the master mix (interleaved stereo) and find its peak.
        let mut master = Vec::with_capacity(nframes * 2);
        let mut peak = 0.0_f32;
        for f in 0..nframes {
            let base = f * dc;
            let (mut l, mut r) = (0.0_f32, 0.0_f32);
            for t in &self.tracks {
                if t.muted || t.channels.is_empty() {
                    continue;
                }
                let left = interleaved[base + t.channels[0] as usize] * t.volume;
                let right = t
                    .channels
                    .get(1)
                    .map(|&c| interleaved[base + c as usize] * t.volume)
                    .unwrap_or(left);
                l += left;
                r += right;
            }
            peak = peak.max(l.abs()).max(r.abs());
            master.push(l);
            master.push(r);
        }

        let live = self.master_arm.update(linear_to_db(peak), dt_ms);

        // Gate opened → start a new take.
        if live && !self.was_live {
            self.take_index += 1;
        }

        if live {
            // Master mix (always).
            if self.master_writer.is_none() {
                let spec = spec_for(2, self.sample_rate, bit_depth);
                let path = self.dir.join(self.take_file("master"));
                self.master_writer = Some(
                    WavWriter::create(&path, spec).map_err(|e| format!("create master: {}", e))?,
                );
            }
            {
                let w = self.master_writer.as_mut().unwrap();
                for &s in &master {
                    write_sample(w, s, bit_depth)?;
                }
            }
            self.master_frames += nframes as u64;

            // Stems (only when >1 active track).
            if self.write_stems {
                // Build paths up front to avoid borrowing self in the loop.
                for idx in 0..self.tracks.len() {
                    if self.tracks[idx].muted || self.tracks[idx].channels.is_empty() {
                        continue;
                    }
                    if self.tracks[idx].writer.is_none() {
                        let path = self.dir.join(self.take_file(&self.tracks[idx].name.clone()));
                        let spec = self.tracks[idx].spec;
                        self.tracks[idx].writer = Some(
                            WavWriter::create(&path, spec).map_err(|e| format!("create stem: {}", e))?,
                        );
                    }
                    let channels = self.tracks[idx].channels.clone();
                    let w = self.tracks[idx].writer.as_mut().unwrap();
                    for f in 0..nframes {
                        let base = f * dc;
                        for &ch in &channels {
                            write_sample(w, interleaved[base + ch as usize], bit_depth)?;
                        }
                    }
                    self.tracks[idx].frames += nframes as u64;
                }
            }
        }

        // Gate closed → finalize the take.
        if !live && self.was_live {
            self.close_take()?;
        }

        self.was_live = live;
        Ok(())
    }

    /// Finalize the current take's writers, record it, and reset for the next.
    fn close_take(&mut self) -> Result<(), String> {
        let master_file = self.take_file("master");
        if let Some(w) = self.master_writer.take() {
            w.finalize().map_err(|e| format!("finalize master: {}", e))?;
        } else {
            // No master writer means nothing was captured this run — nothing to do.
            self.master_frames = 0;
            return Ok(());
        }

        let mut stems = Vec::new();
        for t in &mut self.tracks {
            if let Some(w) = t.writer.take() {
                w.finalize().map_err(|e| format!("finalize {}: {}", t.name, e))?;
                let file = format!(
                    "{}-{}-t{:02}-{}.wav",
                    self.recording,
                    self.id,
                    self.take_index,
                    sanitize_stem(&t.name)
                );
                stems.push(TakeStem { name: t.name.clone(), file });
            }
            t.frames = 0;
        }

        self.takes.push(TakeSummary {
            index: self.take_index,
            master: master_file,
            stems,
            frames: self.master_frames,
        });
        self.master_frames = 0;

        // Keep session.json current so closed takes are immediately usable.
        self.write_manifest()?;
        Ok(())
    }

    fn manifest(&self) -> SessionManifest {
        SessionManifest {
            recording: self.recording.clone(),
            id: self.id.clone(),
            datetime: self.datetime.clone(),
            sample_rate: self.sample_rate,
            bit_depth: self.bit_depth,
            device: self.device.clone(),
            device_channels: self.device_channels,
            master_arm: self.master_mode,
            tracks: self
                .tracks
                .iter()
                .map(|t| TrackLayout {
                    name: t.name.clone(),
                    channels: t.channels.clone(),
                    muted: t.muted,
                    volume: t.volume,
                })
                .collect(),
            takes: self.takes.clone(),
        }
    }

    fn write_manifest(&self) -> Result<(), String> {
        let json = serde_json::to_string_pretty(&self.manifest())
            .map_err(|e| format!("serialize manifest: {}", e))?;
        std::fs::write(self.dir.join("session.json"), json)
            .map_err(|e| format!("write manifest: {}", e))
    }

    /// Close any in-progress take and write the final manifest.
    pub fn finalize(mut self) -> Result<SessionManifest, String> {
        if self.was_live {
            self.close_take()?;
        }
        self.write_manifest()?;
        Ok(self.manifest())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naming(recording: &str) -> SessionNaming {
        SessionNaming {
            recording: recording.to_string(),
            id: "a1b2c3".to_string(),
            datetime: "20260607-120000".to_string(),
        }
    }

    fn track(name: &str, channels: Vec<u16>, muted: bool, volume: f32) -> TrackConfig {
        TrackConfig { name: name.into(), channels, muted, volume }
    }

    /// `nframes` of `amp` on the listed channels, rest silent.
    fn block(device_channels: u16, nframes: usize, hot: &[u16], amp: f32) -> Vec<f32> {
        let dc = device_channels as usize;
        let mut out = vec![0.0f32; dc * nframes];
        for f in 0..nframes {
            for &c in hot {
                out[f * dc + c as usize] = amp;
            }
        }
        out
    }

    fn config_with(arm: ArmMode, timeout_ms: u32, tracks: Vec<TrackConfig>, dir: &Path) -> RecorderConfig {
        let mut c = RecorderConfig::default();
        c.output_dir = dir.to_string_lossy().to_string();
        c.sample_rate = 48000;
        c.bit_depth = 16;
        c.master_arm = arm;
        c.master_threshold_db = -40.0;
        c.master_timeout_ms = timeout_ms;
        c.tracks = tracks;
        c
    }

    fn read_wav(path: &Path) -> (u16, usize) {
        let r = hound::WavReader::open(path).unwrap();
        let spec = r.spec();
        (spec.channels, r.len() as usize / spec.channels as usize)
    }

    #[test]
    fn on_mode_is_one_continuous_take() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = config_with(ArmMode::On, 2000, vec![track("kick", vec![0], false, 1.0)], tmp.path());
        let mut s = SessionWriter::create(&cfg, None, 2, &naming("rec")).unwrap();
        let dir = s.dir().to_path_buf();
        s.push_frames(&block(2, 500, &[0], 0.0)).unwrap(); // ON writes even silence
        s.push_frames(&block(2, 500, &[0], 0.5)).unwrap();
        let m = s.finalize().unwrap();
        assert_eq!(m.take_count(), 1);
        assert_eq!(m.takes[0].index, 1);
        assert_eq!(m.takes[0].master, "rec-a1b2c3-t01-master.wav");
        let (ch, frames) = read_wav(&dir.join(&m.takes[0].master));
        assert_eq!(ch, 2);
        assert_eq!(frames, 1000);
    }

    #[test]
    fn auto_segments_each_phrase_into_its_own_take() {
        let tmp = tempfile::tempdir().unwrap();
        // hold = 100ms; blocks of 4800 frames = 100ms each at 48k.
        let cfg = config_with(ArmMode::Auto, 100, vec![track("t", vec![0], false, 1.0)], tmp.path());
        let mut s = SessionWriter::create(&cfg, None, 1, &naming("rec")).unwrap();
        let dir = s.dir().to_path_buf();

        s.push_frames(&block(1, 4800, &[0], 0.5)).unwrap(); // phrase 1 (take opens)
        s.push_frames(&block(1, 4800, &[], 0.0)).unwrap(); // silence → hold elapses → take 1 closes
        s.push_frames(&block(1, 4800, &[0], 0.5)).unwrap(); // phrase 2 (take opens)
        let m = s.finalize().unwrap(); // closes take 2

        assert_eq!(m.take_count(), 2, "two phrases → two takes");
        assert_eq!(m.takes[0].index, 1);
        assert_eq!(m.takes[1].index, 2);
        assert_eq!(m.takes[0].master, "rec-a1b2c3-t01-master.wav");
        assert_eq!(m.takes[1].master, "rec-a1b2c3-t02-master.wav");
        assert!(dir.join("rec-a1b2c3-t01-master.wav").exists());
        assert!(dir.join("rec-a1b2c3-t02-master.wav").exists());
    }

    #[test]
    fn auto_below_threshold_records_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = config_with(ArmMode::Auto, 2000, vec![track("t", vec![0], false, 1.0)], tmp.path());
        let mut s = SessionWriter::create(&cfg, None, 1, &naming("rec")).unwrap();
        s.push_frames(&block(1, 5000, &[0], 0.0005)).unwrap(); // ~-66 dB
        let m = s.finalize().unwrap();
        assert_eq!(m.take_count(), 0);
    }

    #[test]
    fn multi_track_take_has_stems_plus_master() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = config_with(
            ArmMode::On,
            2000,
            vec![track("kick", vec![0], false, 1.0), track("snare", vec![1], false, 1.0)],
            tmp.path(),
        );
        let mut s = SessionWriter::create(&cfg, None, 2, &naming("jam")).unwrap();
        let dir = s.dir().to_path_buf();
        s.push_frames(&block(2, 200, &[0, 1], 0.3)).unwrap();
        let m = s.finalize().unwrap();
        let take = &m.takes[0];
        assert_eq!(take.master, "jam-a1b2c3-t01-master.wav");
        assert_eq!(take.stems.len(), 2);
        assert!(dir.join("jam-a1b2c3-t01-kick.wav").exists());
        assert!(dir.join("jam-a1b2c3-t01-snare.wav").exists());
    }

    #[test]
    fn single_track_take_is_master_only() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = config_with(ArmMode::On, 2000, vec![track("solo", vec![0], false, 1.0)], tmp.path());
        let mut s = SessionWriter::create(&cfg, None, 1, &naming("rec")).unwrap();
        s.push_frames(&block(1, 100, &[0], 0.5)).unwrap();
        let m = s.finalize().unwrap();
        assert!(m.takes[0].stems.is_empty(), "single active track → master only");
    }

    #[test]
    fn muted_track_excluded() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = config_with(
            ArmMode::On,
            2000,
            vec![track("kick", vec![0], false, 1.0), track("noise", vec![1], true, 1.0)],
            tmp.path(),
        );
        let mut s = SessionWriter::create(&cfg, None, 2, &naming("rec")).unwrap();
        let dir = s.dir().to_path_buf();
        s.push_frames(&block(2, 50, &[0, 1], 0.5)).unwrap();
        let _ = s.finalize().unwrap();
        // Only one active track → master only, muted noise wrote nothing.
        assert!(!dir.join("rec-a1b2c3-t01-noise.wav").exists());
    }

    #[test]
    fn take_clock_resets_between_takes() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = config_with(ArmMode::Auto, 100, vec![track("t", vec![0], false, 1.0)], tmp.path());
        let mut s = SessionWriter::create(&cfg, None, 1, &naming("rec")).unwrap();
        s.push_frames(&block(1, 4800, &[0], 0.5)).unwrap();
        assert_eq!(s.current_take_frames(), 4800);
        assert!(s.is_capturing());
        s.push_frames(&block(1, 4800, &[], 0.0)).unwrap(); // hold elapses, take closes
        assert_eq!(s.current_take_frames(), 0, "clock resets after take closes");
        assert!(!s.is_capturing());
        assert_eq!(s.takes_completed(), 1);
    }

    #[test]
    fn manifest_written_with_takes_and_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = config_with(ArmMode::On, 2000, vec![track("a", vec![0], false, 1.0)], tmp.path());
        let mut s = SessionWriter::create(&cfg, Some("Zoom".into()), 2, &naming("set")).unwrap();
        let dir = s.dir().to_path_buf();
        s.push_frames(&block(2, 100, &[0], 0.5)).unwrap();
        let m = s.finalize().unwrap();
        assert_eq!(m.master_arm, ArmMode::On);
        let raw = std::fs::read_to_string(dir.join("session.json")).unwrap();
        assert!(raw.contains("\"takes\""));
        assert!(raw.contains("t01-master.wav"));
        assert!(raw.contains("\"master_arm\": \"on\""));
    }
}
