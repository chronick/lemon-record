//! Live audio capture: device monitoring + multi-track session recording.
//!
//! This is the only hardware-coupled module. It opens a cpal input stream at
//! the device's native channel count and, on each callback, (1) computes
//! per-channel meter levels and (2) — while recording — forwards the
//! interleaved block to a writer thread that drives a [`SessionWriter`]. All
//! the routing/arming/WAV logic lives in the pure `session`/`arm` modules; this
//! file is the thin shell that feeds them real samples.

use crate::config::RecorderConfig;
use crate::metering::{calculate_stereo_levels, ChannelLevel, LevelData};
use crate::session::{SessionManifest, SessionNaming, SessionWriter};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;
use crossbeam::channel::{self, Sender};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

// cpal::Stream is !Send on macOS (CoreAudio internals) but we only ever touch
// it from one thread behind a Mutex.
struct SendStream(#[allow(dead_code)] cpal::Stream);
unsafe impl Send for SendStream {}
unsafe impl Sync for SendStream {}

#[derive(Debug, Clone, Serialize)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
    pub max_channels: u16,
    pub default_sample_rate: u32,
}

/// Live snapshot the GUI polls each frame.
#[derive(Debug, Clone, Serialize, Default)]
pub struct CaptureStatus {
    pub monitoring: bool,
    pub recording: bool,
    pub device: Option<String>,
    pub channels: u16,
    pub sample_rate: u32,
    pub elapsed_secs: f64,
    pub session_dir: Option<String>,
}

pub struct Capture {
    stream: Mutex<Option<SendStream>>,
    device_name: Mutex<Option<String>>,
    is_monitoring: Arc<AtomicBool>,
    levels: Arc<Mutex<LevelData>>,
    smoothed_levels: Arc<Mutex<LevelData>>,
    stream_channels: Arc<AtomicU16>,
    stream_sample_rate: Arc<AtomicU32>,

    is_recording: Arc<AtomicBool>,
    writer_tx: Arc<Mutex<Option<Sender<Vec<f32>>>>>,
    writer_handle: Mutex<Option<JoinHandle<()>>>,
    recording_start: Mutex<Option<Instant>>,
    session_dir: Mutex<Option<String>>,
    finalized: Arc<AtomicBool>,
    last_manifest: Arc<Mutex<Option<SessionManifest>>>,
    /// Frames in the *current take* — resets to 0 when a take closes, so the UI
    /// clock shows per-take length. 0 while armed but waiting below threshold.
    take_frames: Arc<AtomicU64>,
    /// Whether a take is currently capturing (master gate open).
    capturing: Arc<AtomicBool>,
    /// Number of takes finalized in the current/last session.
    takes_done: Arc<AtomicU64>,
}

impl Default for Capture {
    fn default() -> Self {
        Self::new()
    }
}

impl Capture {
    pub fn new() -> Self {
        Self {
            stream: Mutex::new(None),
            device_name: Mutex::new(None),
            is_monitoring: Arc::new(AtomicBool::new(false)),
            levels: Arc::new(Mutex::new(LevelData::default())),
            smoothed_levels: Arc::new(Mutex::new(LevelData::default())),
            stream_channels: Arc::new(AtomicU16::new(2)),
            stream_sample_rate: Arc::new(AtomicU32::new(48000)),
            is_recording: Arc::new(AtomicBool::new(false)),
            writer_tx: Arc::new(Mutex::new(None)),
            writer_handle: Mutex::new(None),
            recording_start: Mutex::new(None),
            session_dir: Mutex::new(None),
            finalized: Arc::new(AtomicBool::new(true)),
            last_manifest: Arc::new(Mutex::new(None)),
            take_frames: Arc::new(AtomicU64::new(0)),
            capturing: Arc::new(AtomicBool::new(false)),
            takes_done: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn is_monitoring(&self) -> bool {
        self.is_monitoring.load(Ordering::Relaxed)
    }
    pub fn is_recording(&self) -> bool {
        self.is_recording.load(Ordering::Relaxed)
    }
    pub fn channels(&self) -> u16 {
        self.stream_channels.load(Ordering::Relaxed)
    }
    pub fn sample_rate(&self) -> u32 {
        self.stream_sample_rate.load(Ordering::Relaxed)
    }

    /// Smoothed per-channel levels for meters.
    pub fn levels(&self) -> Vec<ChannelLevel> {
        self.smoothed_levels
            .lock()
            .map(|l| l.channels.clone())
            .unwrap_or_default()
    }

    pub fn status(&self) -> CaptureStatus {
        let elapsed = self
            .recording_start
            .lock()
            .ok()
            .and_then(|g| g.map(|s| s.elapsed().as_secs_f64()))
            .unwrap_or(0.0);
        CaptureStatus {
            monitoring: self.is_monitoring(),
            recording: self.is_recording(),
            device: self.device_name.lock().ok().and_then(|g| g.clone()),
            channels: self.channels(),
            sample_rate: self.sample_rate(),
            elapsed_secs: elapsed,
            session_dir: self.session_dir.lock().ok().and_then(|g| g.clone()),
        }
    }

    /// Current take's duration, in seconds. Resets to 0 when a take closes
    /// (after the hold) and while armed-but-waiting — use for the transport
    /// clock so it tracks the take, not session uptime.
    pub fn take_secs(&self) -> f64 {
        let frames = self.take_frames.load(Ordering::Relaxed);
        let sr = self.sample_rate().max(1);
        frames as f64 / sr as f64
    }

    /// Whether a take is currently capturing (vs armed-and-waiting).
    pub fn is_capturing(&self) -> bool {
        self.capturing.load(Ordering::Relaxed)
    }

    /// Takes finalized in the current/last session.
    pub fn takes_done(&self) -> u64 {
        self.takes_done.load(Ordering::Relaxed)
    }

    /// The manifest of the most recently finalized session, if any.
    pub fn last_manifest(&self) -> Option<SessionManifest> {
        self.last_manifest.lock().ok().and_then(|g| g.clone())
    }

    /// Select an input device and start always-on monitoring. Opens the device
    /// at its native channel count so every input is available to the tracks.
    pub fn select_device(
        &self,
        device_id: &str,
        preferred_sample_rate: Option<u32>,
    ) -> Result<(), String> {
        self.stop_monitoring();

        let device = find_device_by_id(device_id)?;
        let default_config = device
            .default_input_config()
            .map_err(|e| format!("device config: {}", e))?;

        let supported_config = preferred_sample_rate
            .and_then(|target| {
                device.supported_input_configs().ok().and_then(|configs| {
                    configs
                        .filter_map(|range| {
                            let (min, max) =
                                (range.min_sample_rate().0, range.max_sample_rate().0);
                            (min <= target && target <= max)
                                .then(|| range.with_sample_rate(cpal::SampleRate(target)))
                        })
                        .next()
                })
            })
            .unwrap_or(default_config);

        let num_channels = supported_config.channels();
        let sample_rate = supported_config.sample_rate().0;
        let sample_format = supported_config.sample_format();
        let stream_config: cpal::StreamConfig = supported_config.into();

        self.stream_channels.store(num_channels, Ordering::Relaxed);
        self.stream_sample_rate.store(sample_rate, Ordering::Relaxed);

        let levels = Arc::clone(&self.levels);
        let smoothed = Arc::clone(&self.smoothed_levels);
        let is_recording = Arc::clone(&self.is_recording);
        let writer_tx = Arc::clone(&self.writer_tx);

        let err_fn = |err: cpal::StreamError| eprintln!("audio input error: {}", err);

        macro_rules! build {
            ($t:ty, $conv:expr) => {{
                let lv = Arc::clone(&levels);
                let sl = Arc::clone(&smoothed);
                let ir = Arc::clone(&is_recording);
                let wt = Arc::clone(&writer_tx);
                device
                    .build_input_stream(
                        &stream_config,
                        move |data: &[$t], _: &cpal::InputCallbackInfo| {
                            let f32_data: Vec<f32> = data.iter().map($conv).collect();
                            process_block(&f32_data, num_channels, &lv, &sl, &ir, &wt);
                        },
                        err_fn,
                        None,
                    )
                    .map_err(|e| format!("build input stream: {}", e))?
            }};
        }

        let stream = match sample_format {
            SampleFormat::F32 => build!(f32, |&s| s),
            SampleFormat::I16 => build!(i16, |&s| s as f32 / 32768.0),
            SampleFormat::I32 => build!(i32, |&s| s as f32 / 2147483648.0),
            other => return Err(format!("unsupported sample format: {:?}", other)),
        };

        stream.play().map_err(|e| format!("start stream: {}", e))?;
        *self.stream.lock().map_err(|e| e.to_string())? = Some(SendStream(stream));
        *self.device_name.lock().map_err(|e| e.to_string())? =
            Some(device.name().unwrap_or_else(|_| device_id.to_string()));
        self.is_monitoring.store(true, Ordering::Relaxed);
        Ok(())
    }

    fn stop_monitoring(&self) {
        if let Ok(mut s) = self.stream.lock() {
            *s = None;
        }
        self.is_monitoring.store(false, Ordering::Relaxed);
    }

    /// Begin a multi-track session. Spawns a writer thread that owns the
    /// [`SessionWriter`]; the audio callback forwards interleaved blocks to it.
    /// The recording name (auto when blank), a short unique id, and the folder
    /// datetime are all generated here so the caller doesn't manage naming.
    pub fn start_session(&self, config: &RecorderConfig) -> Result<(), String> {
        if self.is_recording() {
            return Err("already recording".into());
        }
        if !self.is_monitoring() {
            return Err("no device selected".into());
        }

        // The session captures at the live device channel count + sample rate,
        // overriding any stale value in the config so the WAVs match the stream.
        let mut cfg = config.clone();
        cfg.sample_rate = self.sample_rate();
        let device_channels = self.channels();
        let device = self.device_name.lock().ok().and_then(|g| g.clone());

        let naming = SessionNaming {
            recording: crate::resolve_recording_name(&config.recording_name),
            id: crate::short_id(),
            datetime: crate::session_timestamp(),
        };
        let session = SessionWriter::create(&cfg, device, device_channels, &naming)?;
        let dir = session.dir().to_string_lossy().to_string();

        let (tx, rx) = channel::unbounded::<Vec<f32>>();
        let finalized = Arc::clone(&self.finalized);
        let last_manifest = Arc::clone(&self.last_manifest);
        let take_frames = Arc::clone(&self.take_frames);
        let capturing = Arc::clone(&self.capturing);
        let takes_done = Arc::clone(&self.takes_done);
        take_frames.store(0, Ordering::Relaxed);
        capturing.store(false, Ordering::Relaxed);
        takes_done.store(0, Ordering::Relaxed);

        let handle = std::thread::spawn(move || {
            let mut session = session;
            for block in rx {
                if let Err(e) = session.push_frames(&block) {
                    eprintln!("session write error: {}", e);
                    break;
                }
                // Publish per-take state so the UI clock + notification track
                // the take, resetting between takes while staying armed.
                take_frames.store(session.current_take_frames(), Ordering::Relaxed);
                capturing.store(session.is_capturing(), Ordering::Relaxed);
                takes_done.store(session.takes_completed() as u64, Ordering::Relaxed);
            }
            match session.finalize() {
                Ok(m) => {
                    takes_done.store(m.take_count() as u64, Ordering::Relaxed);
                    if let Ok(mut g) = last_manifest.lock() {
                        *g = Some(m);
                    }
                }
                Err(e) => eprintln!("session finalize error: {}", e),
            }
            take_frames.store(0, Ordering::Relaxed);
            capturing.store(false, Ordering::Relaxed);
            finalized.store(true, Ordering::Release);
        });

        *self.writer_tx.lock().map_err(|e| e.to_string())? = Some(tx);
        *self.writer_handle.lock().map_err(|e| e.to_string())? = Some(handle);
        *self.recording_start.lock().map_err(|e| e.to_string())? = Some(Instant::now());
        *self.session_dir.lock().map_err(|e| e.to_string())? = Some(dir);
        self.finalized.store(false, Ordering::Release);
        self.is_recording.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Stop the session. Closes the writer channel; the writer thread finalizes
    /// the WAVs and manifest in the background. Poll [`Capture::is_finalized`]
    /// or call [`Capture::wait_for_finalize`] before reading the files.
    pub fn stop_session(&self) -> Result<(), String> {
        if !self.is_recording() {
            return Err("not recording".into());
        }
        self.is_recording.store(false, Ordering::Relaxed);
        *self.writer_tx.lock().map_err(|e| e.to_string())? = None; // signal EOF

        let finalized = Arc::clone(&self.finalized);
        let handle = self
            .writer_handle
            .lock()
            .map_err(|e| e.to_string())?
            .take();
        if let Some(h) = handle {
            std::thread::spawn(move || {
                let _ = h.join();
                finalized.store(true, Ordering::Release);
            });
        } else {
            self.finalized.store(true, Ordering::Release);
        }
        *self.recording_start.lock().map_err(|e| e.to_string())? = None;
        Ok(())
    }

    pub fn is_finalized(&self) -> bool {
        self.finalized.load(Ordering::Acquire)
    }

    pub fn wait_for_finalize(&self, timeout_ms: u64) -> Result<(), String> {
        let start = Instant::now();
        while !self.is_finalized() {
            if start.elapsed().as_millis() as u64 > timeout_ms {
                return Err("timeout finalizing session".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        Ok(())
    }
}

/// Apply exponential smoothing: fast attack, slow decay.
fn smooth_levels(raw: &LevelData, prev: &LevelData) -> LevelData {
    let mut channels = Vec::with_capacity(raw.channels.len());
    for (i, raw_ch) in raw.channels.iter().enumerate() {
        let (prev_rms, prev_peak) = prev
            .channels
            .get(i)
            .map(|p| (p.rms_db, p.peak_db))
            .unwrap_or((-96.0, -96.0));
        let rms_alpha = if raw_ch.rms_db > prev_rms { 0.4 } else { 0.15 };
        let peak_alpha = if raw_ch.peak_db > prev_peak { 0.5 } else { 0.1 };
        channels.push(ChannelLevel {
            rms_db: rms_alpha * raw_ch.rms_db + (1.0 - rms_alpha) * prev_rms,
            peak_db: peak_alpha * raw_ch.peak_db + (1.0 - peak_alpha) * prev_peak,
        });
    }
    LevelData { channels }
}

/// Audio-thread callback body: meters always, forward to writer while recording.
/// Visualization locks are `try_lock` so the realtime thread never blocks; the
/// writer send uses a regular lock because dropping audio is not acceptable.
fn process_block(
    data: &[f32],
    num_channels: u16,
    levels: &Arc<Mutex<LevelData>>,
    smoothed_levels: &Arc<Mutex<LevelData>>,
    is_recording: &Arc<AtomicBool>,
    writer_tx: &Arc<Mutex<Option<Sender<Vec<f32>>>>>,
) {
    let raw = calculate_stereo_levels(data, num_channels);
    if let Ok(mut lvl) = levels.try_lock() {
        *lvl = raw.clone();
    }
    if let Ok(mut smooth) = smoothed_levels.try_lock() {
        *smooth = smooth_levels(&raw, &smooth);
    }
    if is_recording.load(Ordering::Relaxed) {
        if let Ok(tx_guard) = writer_tx.lock() {
            if let Some(tx) = tx_guard.as_ref() {
                let _ = tx.send(data.to_vec());
            }
        }
    }
}

pub fn list_input_devices() -> Result<Vec<AudioDevice>, String> {
    let host = cpal::default_host();
    let default_name = host.default_input_device().and_then(|d| d.name().ok());
    let mut devices = Vec::new();
    for device in host
        .input_devices()
        .map_err(|e| format!("enumerate devices: {}", e))?
    {
        let name = device.name().unwrap_or_else(|_| "Unknown".to_string());
        let is_default = default_name.as_deref() == Some(&name);
        let (max_channels, default_sample_rate) = device
            .default_input_config()
            .map(|c| (c.channels(), c.sample_rate().0))
            .unwrap_or((2, 48000));
        devices.push(AudioDevice {
            id: name.clone(),
            name,
            is_default,
            max_channels,
            default_sample_rate,
        });
    }
    Ok(devices)
}

fn find_device_by_id(device_id: &str) -> Result<cpal::Device, String> {
    let host = cpal::default_host();
    for device in host
        .input_devices()
        .map_err(|e| format!("enumerate devices: {}", e))?
    {
        if device.name().ok().as_deref() == Some(device_id) {
            return Ok(device);
        }
    }
    Err(format!("device not found: {}", device_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_capture_is_idle() {
        let c = Capture::new();
        assert!(!c.is_monitoring());
        assert!(!c.is_recording());
        assert!(c.is_finalized());
        assert!(c.last_manifest().is_none());
    }

    #[test]
    fn start_session_requires_monitoring() {
        let c = Capture::new();
        let cfg = RecorderConfig::default();
        let err = c.start_session(&cfg).unwrap_err();
        assert!(err.contains("no device"));
    }

    #[test]
    fn stop_without_recording_errors() {
        let c = Capture::new();
        assert!(c.stop_session().is_err());
    }

    #[test]
    fn smoothing_tracks_toward_raw() {
        let raw = LevelData {
            channels: vec![ChannelLevel { rms_db: -10.0, peak_db: -6.0 }],
        };
        let prev = LevelData {
            channels: vec![ChannelLevel { rms_db: -40.0, peak_db: -40.0 }],
        };
        let out = smooth_levels(&raw, &prev);
        // Attack pulls the smoothed value upward toward the louder raw value.
        assert!(out.channels[0].rms_db > prev.channels[0].rms_db);
        assert!(out.channels[0].rms_db < raw.channels[0].rms_db);
    }

    #[test]
    fn status_reflects_idle_state() {
        let s = Capture::new().status();
        assert!(!s.monitoring && !s.recording);
        assert!(s.device.is_none());
    }
}
