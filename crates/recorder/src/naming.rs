//! Filesystem-safe naming helpers for recordings.
//!
//! These are the deterministic, dependency-free pieces lifted from the
//! original recorder: stem sanitization, timestamp-suffix preservation, and a
//! heroku-style name generator used as a fallback session name. The CLAP /
//! LLM auto-naming that used to live here belongs to downstream tooling, not
//! the recorder — the recorder only needs to produce safe, unique filenames.

use std::path::PathBuf;

/// Short, evocative word lists for the fallback name generator. Used when the
/// user hasn't named a session — better than a bare timestamp.
const ADJECTIVES: &[&str] = &[
    "autumn", "silver", "amber", "crimson", "ember", "frosted", "hollow", "wild", "quiet",
    "violet", "midnight", "restless", "drifting", "muted", "soft", "brittle", "molten", "still",
    "dusty", "warm", "distant", "neon", "salted", "faded", "gilded", "rusted", "hushed",
    "winter", "summer", "coastal",
];

const NOUNS: &[&str] = &[
    "waterfall", "meadow", "engine", "cove", "shadow", "ember", "signal", "ridge", "lantern",
    "harbor", "valley", "thicket", "cipher", "beacon", "mosaic", "atlas", "cascade", "nocturne",
    "drift", "echo", "glacier", "grove", "tundra", "lagoon", "canyon", "eddy", "lattice",
    "archive", "mirror", "prism",
];

/// Deterministic adjective-noun pair derived from `seed`. Uses a simple
/// FNV-1a hash to avoid pulling in a hashing dependency for this one use.
pub fn heroku_style_stem(seed: &str) -> String {
    let hash = fnv1a(seed.as_bytes());
    let adj = ADJECTIVES[(hash as usize) % ADJECTIVES.len()];
    let noun = NOUNS[((hash >> 16) as usize) % NOUNS.len()];
    format!("{}-{}", adj, noun)
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Rename `path` to use `new_stem` (extension is preserved). Returns the new
/// path. If renaming fails or the target exists, returns the original path
/// unchanged — callers can treat the returned path as canonical either way.
pub fn rename_with_stem(path: &str, new_stem: &str) -> String {
    let original = PathBuf::from(path);
    let parent = match original.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let ext = original.extension().and_then(|e| e.to_str()).unwrap_or("wav");

    let timestamp_suffix = original
        .file_stem()
        .and_then(|s| s.to_str())
        .and_then(extract_timestamp_suffix)
        .unwrap_or_default();

    let new_name = if timestamp_suffix.is_empty() {
        format!("{}.{}", new_stem, ext)
    } else {
        format!("{}_{}.{}", new_stem, timestamp_suffix, ext)
    };
    let new_path = parent.join(&new_name);

    if new_path == original {
        return path.to_string();
    }
    if new_path.exists() {
        eprintln!(
            "Auto-name: target {} already exists; keeping original",
            new_path.display()
        );
        return path.to_string();
    }

    match std::fs::rename(&original, &new_path) {
        Ok(()) => new_path.to_string_lossy().to_string(),
        Err(e) => {
            eprintln!("Auto-name: rename failed ({}); keeping original", e);
            path.to_string()
        }
    }
}

/// Extract a `YYYYMMDD_HHMMSS` suffix from a filename stem, if present.
pub fn extract_timestamp_suffix(stem: &str) -> Option<String> {
    let bytes = stem.as_bytes();
    if bytes.len() < 16 {
        return None;
    }
    let n = bytes.len();
    let tail = &bytes[n.saturating_sub(16)..];
    if tail[0] != b'_' || tail[9] != b'_' {
        return None;
    }
    for (i, &b) in tail.iter().enumerate() {
        match i {
            0 | 9 => {
                if b != b'_' {
                    return None;
                }
            }
            _ => {
                if !b.is_ascii_digit() {
                    return None;
                }
            }
        }
    }
    Some(String::from_utf8_lossy(&tail[1..]).to_string())
}

/// Sanitize a stem so it's safe as a filename on macOS/Linux/Windows.
/// Keeps `[a-zA-Z0-9_-]`, collapses runs of other characters to `-`.
pub fn sanitize_stem(stem: &str) -> String {
    let mut out = String::with_capacity(stem.len());
    let mut last_was_sep = false;
    for c in stem.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            out.push(c);
            last_was_sep = false;
        } else if !last_was_sep {
            out.push('-');
            last_was_sep = true;
        }
    }
    let trimmed = out.trim_matches(|c: char| c == '-' || c == '_').to_string();
    if trimmed.is_empty() {
        "recording".to_string()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heroku_style_is_deterministic() {
        let a = heroku_style_stem("session_20260415_183012");
        let b = heroku_style_stem("session_20260415_183012");
        assert_eq!(a, b);
        assert!(a.contains('-'));
    }

    #[test]
    fn heroku_style_varies_across_seeds() {
        let a = heroku_style_stem("session_20260415_183012");
        let b = heroku_style_stem("session_20260415_183030");
        assert_ne!(a, b);
    }

    #[test]
    fn heroku_style_output_is_safe_filename() {
        let stem = heroku_style_stem("any-seed");
        for c in stem.chars() {
            assert!(
                c.is_ascii_alphanumeric() || c == '-' || c == '_',
                "unexpected char: {}",
                c
            );
        }
    }

    #[test]
    fn extract_timestamp_suffix_recognizes_recorder_format() {
        assert_eq!(
            extract_timestamp_suffix("session_20260415_183012"),
            Some("20260415_183012".to_string())
        );
        assert_eq!(
            extract_timestamp_suffix("kick_20260415_183012"),
            Some("20260415_183012".to_string())
        );
    }

    #[test]
    fn extract_timestamp_suffix_returns_none_for_mismatches() {
        assert_eq!(extract_timestamp_suffix("no-timestamp"), None);
        assert_eq!(extract_timestamp_suffix("session_2026_abcdef"), None);
        assert_eq!(extract_timestamp_suffix("short"), None);
    }

    #[test]
    fn sanitize_stem_strips_unsafe_characters() {
        assert_eq!(sanitize_stem("kick/drum!hat"), "kick-drum-hat");
        assert_eq!(sanitize_stem("__valid-name__"), "valid-name");
        assert_eq!(sanitize_stem("///"), "recording");
        assert_eq!(sanitize_stem("dark-kick"), "dark-kick");
        assert_eq!(sanitize_stem("a kick drum"), "a-kick-drum");
    }

    #[test]
    fn rename_preserves_timestamp_suffix() {
        let tmp = tempfile::tempdir().unwrap();
        let original = tmp.path().join("session_20260415_183012.wav");
        std::fs::write(&original, b"fake wav").unwrap();
        let new_path = rename_with_stem(original.to_str().unwrap(), "dark-kick");
        assert!(new_path.ends_with("dark-kick_20260415_183012.wav"));
        assert!(std::path::Path::new(&new_path).exists());
        assert!(!original.exists());
    }

    #[test]
    fn rename_keeps_original_path_if_target_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let original = tmp.path().join("session_20260415_183012.wav");
        std::fs::write(&original, b"fake wav").unwrap();
        let conflict = tmp.path().join("dark-kick_20260415_183012.wav");
        std::fs::write(&conflict, b"other").unwrap();

        let new_path = rename_with_stem(original.to_str().unwrap(), "dark-kick");
        assert_eq!(new_path, original.to_str().unwrap());
        assert!(original.exists(), "original should not have been renamed");
    }
}
