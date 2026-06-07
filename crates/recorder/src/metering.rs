use serde::Serialize;

#[derive(Debug, Clone, Serialize, Default)]
pub struct ChannelLevel {
    pub rms_db: f32,
    pub peak_db: f32,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct LevelData {
    pub channels: Vec<ChannelLevel>,
}

pub fn calculate_rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

pub fn calculate_peak(samples: &[f32]) -> f32 {
    samples.iter().map(|s| s.abs()).fold(0.0_f32, f32::max)
}

pub fn linear_to_db(value: f32) -> f32 {
    if value <= 0.0 {
        -96.0
    } else {
        20.0 * value.log10()
    }
}

pub fn calculate_stereo_levels(interleaved: &[f32], num_channels: u16) -> LevelData {
    let nc = num_channels as usize;
    if nc == 0 || interleaved.is_empty() {
        return LevelData::default();
    }

    let mut channels = Vec::with_capacity(nc);
    for ch in 0..nc {
        let channel_samples: Vec<f32> = interleaved.iter().skip(ch).step_by(nc).copied().collect();
        let rms = calculate_rms(&channel_samples);
        let peak = calculate_peak(&channel_samples);
        channels.push(ChannelLevel {
            rms_db: linear_to_db(rms),
            peak_db: linear_to_db(peak),
        });
    }

    LevelData { channels }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rms_silence() {
        let silence = vec![0.0_f32; 1024];
        let rms = calculate_rms(&silence);
        assert_eq!(rms, 0.0);
        assert_eq!(linear_to_db(rms), -96.0);
    }

    #[test]
    fn test_rms_full_scale() {
        // Full-scale sine wave RMS is ~0.707, so dB is ~-3.01
        let n = 48000;
        let samples: Vec<f32> = (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48000.0).sin())
            .collect();
        let rms = calculate_rms(&samples);
        let db = linear_to_db(rms);
        assert!((db - (-3.01)).abs() < 0.1, "RMS of full-scale sine should be ~-3dB, got {}", db);
    }

    #[test]
    fn test_peak_detection() {
        let samples = vec![0.1, -0.5, 0.3, -0.9, 0.2];
        let peak = calculate_peak(&samples);
        assert!((peak - 0.9).abs() < 1e-6);
    }

    #[test]
    fn test_stereo_deinterleave() {
        // Interleaved stereo: L=0.5, R=0.25, L=0.5, R=0.25
        let interleaved = vec![0.5, 0.25, 0.5, 0.25, 0.5, 0.25, 0.5, 0.25];
        let levels = calculate_stereo_levels(&interleaved, 2);
        assert_eq!(levels.channels.len(), 2);
        // Left channel is all 0.5, right is all 0.25
        let left_rms = calculate_rms(&[0.5, 0.5, 0.5, 0.5]);
        let right_rms = calculate_rms(&[0.25, 0.25, 0.25, 0.25]);
        assert!((levels.channels[0].rms_db - linear_to_db(left_rms)).abs() < 0.01);
        assert!((levels.channels[1].rms_db - linear_to_db(right_rms)).abs() < 0.01);
    }
}
