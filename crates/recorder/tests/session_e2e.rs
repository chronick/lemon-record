//! End-to-end session test: drive a full session with synthetic audio through
//! the public API exactly as the capture writer thread does, then read the
//! resulting WAVs + manifest back off disk and assert their contents.
//!
//! Exercises the real `SessionWriter` — folder creation, master-gated
//! auto-segmenting takes, per-take stems, mute exclusion, the naming
//! convention, and the manifest.

use recorder::config::{ArmMode, RecorderConfig, TrackConfig};
use recorder::session::{SessionNaming, SessionWriter};
use std::path::Path;
use std::process::Command;

fn naming(recording: &str, id: &str, datetime: &str) -> SessionNaming {
    SessionNaming {
        recording: recording.to_string(),
        id: id.to_string(),
        datetime: datetime.to_string(),
    }
}

fn track(name: &str, channels: Vec<u16>, muted: bool) -> TrackConfig {
    TrackConfig { name: name.into(), channels, muted, volume: 1.0 }
}

/// Interleaved frames: listed channels carry a 440 Hz sine at `amp`, rest silent.
fn sine_block(device_channels: u16, nframes: usize, hot: &[u16], amp: f32, sr: u32) -> Vec<f32> {
    let dc = device_channels as usize;
    let mut out = vec![0.0f32; dc * nframes];
    for f in 0..nframes {
        let s = (2.0 * std::f32::consts::PI * 440.0 * f as f32 / sr as f32).sin() * amp;
        for &ch in hot {
            out[f * dc + ch as usize] = s;
        }
    }
    out
}

fn read_frames(path: &Path) -> (u16, u32, usize) {
    let r = hound::WavReader::open(path).expect("open wav");
    let spec = r.spec();
    let frames = r.len() as usize / spec.channels as usize;
    (spec.channels, spec.sample_rate, frames)
}

#[test]
fn auto_session_segments_phrases_into_saved_takes() {
    let tmp = tempfile::tempdir().unwrap();
    let sr = 48000;

    // 4-input layout: a stereo pair + a mono kick (both active), plus a muted
    // spare. Master AUTO with a 100 ms hold; blocks of 4800 frames = 100 ms.
    let mut cfg = RecorderConfig::default();
    cfg.output_dir = tmp.path().to_string_lossy().to_string();
    cfg.sample_rate = sr;
    cfg.bit_depth = 24;
    cfg.recording_name = "field rec".into();
    cfg.master_arm = ArmMode::Auto;
    cfg.master_threshold_db = -40.0;
    cfg.master_timeout_ms = 100;
    cfg.tracks = vec![
        track("stereo bus", vec![0, 1], false),
        track("kick", vec![2], false),
        track("spare", vec![3], true), // muted → excluded
    ];

    let mut s = SessionWriter::create(
        &cfg,
        Some("ZoomL6Max".into()),
        4,
        &naming("field rec", "z9x8c7", "20260607-133700"),
    )
    .unwrap();
    let dir = s.dir().to_path_buf();

    // Phrase 1 (200 ms), silence (closes take 1), phrase 2 (100 ms).
    s.push_frames(&sine_block(4, 4800, &[0, 1, 2], 0.5, sr)).unwrap();
    s.push_frames(&sine_block(4, 4800, &[0, 1, 2], 0.5, sr)).unwrap();
    s.push_frames(&sine_block(4, 4800, &[], 0.0, sr)).unwrap(); // hold elapses → take 1 closes
    s.push_frames(&sine_block(4, 4800, &[0, 1, 2], 0.5, sr)).unwrap();
    let m = s.finalize().unwrap(); // closes take 2

    assert_eq!(m.recording, "field-rec");
    assert_eq!(m.master_arm, ArmMode::Auto);
    assert_eq!(m.device.as_deref(), Some("ZoomL6Max"));
    assert_eq!(m.take_count(), 2, "two phrases → two saved takes");
    assert!(dir.ends_with("field-rec-20260607-133700"));

    // Take 1: ~200 ms master + two stems (muted spare excluded).
    let t1 = &m.takes[0];
    assert_eq!(t1.index, 1);
    assert_eq!(t1.master, "field-rec-z9x8c7-t01-master.wav");
    assert_eq!(t1.stems.len(), 2);
    let (mch, mrate, mframes) = read_frames(&dir.join(&t1.master));
    assert_eq!(mch, 2);
    assert_eq!(mrate, sr);
    assert_eq!(mframes, 9600); // 200 ms
    assert!(dir.join("field-rec-z9x8c7-t01-stereo-bus.wav").exists());
    assert!(dir.join("field-rec-z9x8c7-t01-kick.wav").exists());
    assert!(!dir.join("field-rec-z9x8c7-t01-spare.wav").exists());

    // Take 2.
    let t2 = &m.takes[1];
    assert_eq!(t2.index, 2);
    assert_eq!(t2.master, "field-rec-z9x8c7-t02-master.wav");
    assert!(dir.join(&t2.master).exists());

    // Manifest lists both takes.
    let raw = std::fs::read_to_string(dir.join("session.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["takes"].as_array().unwrap().len(), 2);
    assert_eq!(v["recording"], "field-rec");
}

#[test]
fn on_session_is_one_continuous_take() {
    let tmp = tempfile::tempdir().unwrap();
    let sr = 48000;
    let mut cfg = RecorderConfig::default();
    cfg.output_dir = tmp.path().to_string_lossy().to_string();
    cfg.sample_rate = sr;
    cfg.bit_depth = 16;
    cfg.master_arm = ArmMode::On;
    cfg.tracks = vec![track("solo", vec![0], false)];

    let mut s = SessionWriter::create(&cfg, None, 1, &naming("rec", "id0001", "20260607-140000")).unwrap();
    let dir = s.dir().to_path_buf();
    s.push_frames(&sine_block(1, 24000, &[0], 0.5, sr)).unwrap();
    let m = s.finalize().unwrap();
    assert_eq!(m.take_count(), 1);
    let (_, _, frames) = read_frames(&dir.join(&m.takes[0].master));
    assert_eq!(frames, 24000);
}

/// Belt-and-suspenders: if `ffprobe` is on PATH, confirm a take's master WAV is
/// a real decodable file. Skips when absent.
#[test]
fn take_master_is_valid_to_ffprobe_if_available() {
    if Command::new("ffprobe").arg("-version").output().is_err() {
        eprintln!("ffprobe not found; skipping external-validation check");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let sr = 44100;
    let mut cfg = RecorderConfig::default();
    cfg.output_dir = tmp.path().to_string_lossy().to_string();
    cfg.sample_rate = sr;
    cfg.bit_depth = 16;
    cfg.master_arm = ArmMode::On;
    cfg.tracks = vec![track("probe", vec![0], false)];
    let mut s = SessionWriter::create(&cfg, None, 1, &naming("rec", "id0002", "20260607-150000")).unwrap();
    let dir = s.dir().to_path_buf();
    s.push_frames(&sine_block(1, 22050, &[0], 0.5, sr)).unwrap();
    let m = s.finalize().unwrap();
    let path = dir.join(&m.takes[0].master);

    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "stream=sample_rate,channels", "-of", "default=nw=1", path.to_str().unwrap()])
        .output()
        .expect("run ffprobe");
    assert!(out.status.success(), "ffprobe failed: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("sample_rate=44100"), "ffprobe saw: {stdout}");
    assert!(stdout.contains("channels=2"), "ffprobe saw: {stdout}");
}
