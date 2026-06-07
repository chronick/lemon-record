use rustfft::num_complex::Complex;
use rustfft::FftPlanner;
use serde::Serialize;

/// Downsample waveform data to target length.
pub fn downsample_waveform(samples: &[f32], target_len: usize) -> Vec<f32> {
    if samples.len() <= target_len || target_len == 0 {
        return samples.to_vec();
    }
    let step = samples.len() as f64 / target_len as f64;
    (0..target_len)
        .map(|i| {
            let idx = (i as f64 * step) as usize;
            samples[idx.min(samples.len() - 1)]
        })
        .collect()
}

/// Calculate FFT spectrum from audio samples.
/// Returns magnitude bins (linear scale, not dB).
pub fn calculate_spectrum(samples: &[f32], num_bins: usize) -> Vec<f32> {
    if samples.is_empty() || num_bins == 0 {
        return vec![0.0; num_bins];
    }

    // Use power-of-2 FFT size
    let fft_size = samples.len().next_power_of_two();

    // Apply Hann window and convert to complex
    let mut buffer: Vec<Complex<f32>> = (0..fft_size)
        .map(|i| {
            if i < samples.len() {
                let window = 0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / (samples.len() - 1).max(1) as f32).cos());
                Complex::new(samples[i] * window, 0.0)
            } else {
                Complex::new(0.0, 0.0)
            }
        })
        .collect();

    // Perform FFT
    let mut planner = FftPlanner::new();
    let fft = planner.plan_fft_forward(fft_size);
    fft.process(&mut buffer);

    // Take first half (positive frequencies) and compute magnitudes
    let half = fft_size / 2;
    let magnitudes: Vec<f32> = buffer[..half]
        .iter()
        .map(|c| c.norm() / fft_size as f32)
        .collect();

    // Bin into num_bins using logarithmic frequency spacing
    if half <= num_bins {
        let mut result = magnitudes;
        result.resize(num_bins, 0.0);
        return result;
    }

    let mut bins = vec![0.0_f32; num_bins];
    for (bin_idx, bin) in bins.iter_mut().enumerate() {
        // Logarithmic mapping: low freqs get more bins
        let lo = ((bin_idx as f64 / num_bins as f64).powf(2.0) * half as f64) as usize;
        let hi = (((bin_idx + 1) as f64 / num_bins as f64).powf(2.0) * half as f64) as usize;
        let lo = lo.min(half - 1);
        let hi = hi.max(lo + 1).min(half);

        let mut max_val = 0.0_f32;
        for &m in &magnitudes[lo..hi] {
            max_val = max_val.max(m);
        }
        *bin = max_val;
    }

    bins
}

#[derive(Debug, Clone, Serialize)]
pub struct RecordingWaveformData {
    pub peaks: Vec<f32>,
    pub centroids: Vec<f32>,
    pub duration: f64,
}

/// Compute spectral centroid of a segment using FFT.
/// Returns frequency in Hz (weighted average frequency).
pub fn compute_spectral_centroid(segment: &[f32], sample_rate: u32) -> f32 {
    if segment.is_empty() {
        return 0.0;
    }

    let fft_size = segment.len().next_power_of_two();

    // Apply Hann window
    let mut buffer: Vec<Complex<f32>> = (0..fft_size)
        .map(|i| {
            if i < segment.len() {
                let window = 0.5
                    * (1.0
                        - (2.0 * std::f32::consts::PI * i as f32
                            / (segment.len() - 1).max(1) as f32)
                            .cos());
                Complex::new(segment[i] * window, 0.0)
            } else {
                Complex::new(0.0, 0.0)
            }
        })
        .collect();

    let mut planner = FftPlanner::new();
    let fft = planner.plan_fft_forward(fft_size);
    fft.process(&mut buffer);

    let half = fft_size / 2;
    let freq_resolution = sample_rate as f32 / fft_size as f32;

    let mut weighted_sum = 0.0_f32;
    let mut magnitude_sum = 0.0_f32;

    for i in 1..half {
        let magnitude = buffer[i].norm();
        let frequency = i as f32 * freq_resolution;
        weighted_sum += frequency * magnitude;
        magnitude_sum += magnitude;
    }

    if magnitude_sum > 1e-10 {
        weighted_sum / magnitude_sum
    } else {
        0.0
    }
}

/// Compute recording waveform data: peak amplitude + spectral centroid per bar.
///
/// Tested but not yet wired into the live recorder pipeline — kept for
/// the planned post-recording waveform render pass.
#[allow(dead_code)]
pub fn compute_recording_waveform(
    mono: &[f32],
    sample_rate: u32,
    num_bars: usize,
    duration: f64,
) -> RecordingWaveformData {
    if mono.is_empty() || num_bars == 0 {
        return RecordingWaveformData {
            peaks: vec![],
            centroids: vec![],
            duration,
        };
    }

    let samples_per_bar = mono.len() / num_bars;
    if samples_per_bar == 0 {
        // Fewer samples than bars — just return what we have
        let peaks: Vec<f32> = mono.iter().map(|s| s.abs()).collect();
        let centroids: Vec<f32> = mono
            .iter()
            .map(|_| 1000.0) // default centroid
            .collect();
        return RecordingWaveformData {
            peaks,
            centroids,
            duration,
        };
    }

    let mut peaks = Vec::with_capacity(num_bars);
    let mut centroids = Vec::with_capacity(num_bars);

    for bar in 0..num_bars {
        let start = bar * samples_per_bar;
        let end = if bar == num_bars - 1 {
            mono.len()
        } else {
            (bar + 1) * samples_per_bar
        };
        let segment = &mono[start..end];

        // Peak amplitude
        let peak = segment.iter().map(|s| s.abs()).fold(0.0_f32, f32::max);
        peaks.push(peak);

        // Spectral centroid (use up to 4096 samples for FFT)
        let centroid_segment = if segment.len() > 4096 {
            &segment[..4096]
        } else {
            segment
        };
        centroids.push(compute_spectral_centroid(centroid_segment, sample_rate));
    }

    RecordingWaveformData {
        peaks,
        centroids,
        duration,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_waveform_downsample() {
        let samples: Vec<f32> = (0..1000).map(|i| i as f32 / 1000.0).collect();
        let result = downsample_waveform(&samples, 100);
        assert_eq!(result.len(), 100);
    }

    #[test]
    fn test_waveform_downsample_passthrough() {
        let samples = vec![1.0, 2.0, 3.0];
        let result = downsample_waveform(&samples, 10);
        assert_eq!(result.len(), 3); // shorter than target, pass through
    }

    #[test]
    fn test_spectrum_bins() {
        let samples: Vec<f32> = (0..2048)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48000.0).sin())
            .collect();
        let result = calculate_spectrum(&samples, 128);
        assert_eq!(result.len(), 128);
        // At least some bin should be non-zero (440Hz tone)
        let max_val = result.iter().copied().fold(0.0_f32, f32::max);
        assert!(max_val > 0.0, "Spectrum should have non-zero values for a sine wave");
    }

    #[test]
    fn test_spectrum_empty() {
        let result = calculate_spectrum(&[], 64);
        assert_eq!(result.len(), 64);
        assert!(result.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn test_spectrum_hann_window() {
        // Verify windowing: a DC signal with Hann window should concentrate energy
        // in the low-frequency bins rather than being uniform
        let dc_signal = vec![1.0_f32; 1024];
        let spectrum = calculate_spectrum(&dc_signal, 64);
        // Low-frequency bins (first quarter) should have more energy than high-frequency bins (last quarter)
        let low_energy: f32 = spectrum[..16].iter().sum();
        let high_energy: f32 = spectrum[48..].iter().sum();
        assert!(low_energy > high_energy, "Low frequencies should dominate for DC signal");
    }

    #[test]
    fn test_spectral_centroid_440hz() {
        // Generate a 440Hz sine wave at 48kHz
        let sample_rate = 48000;
        let n = 4096;
        let samples: Vec<f32> = (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / sample_rate as f32).sin())
            .collect();
        let centroid = compute_spectral_centroid(&samples, sample_rate);
        // Centroid should be near 440Hz (allow some tolerance for windowing effects)
        assert!(
            (centroid - 440.0).abs() < 50.0,
            "Centroid of 440Hz tone should be near 440Hz, got {}",
            centroid
        );
    }

    #[test]
    fn test_spectral_centroid_high_freq() {
        // A 4000Hz tone should have a much higher centroid than a 200Hz tone
        let sample_rate = 48000;
        let n = 4096;
        let low_tone: Vec<f32> = (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * 200.0 * i as f32 / sample_rate as f32).sin())
            .collect();
        let high_tone: Vec<f32> = (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * 4000.0 * i as f32 / sample_rate as f32).sin())
            .collect();
        let low_centroid = compute_spectral_centroid(&low_tone, sample_rate);
        let high_centroid = compute_spectral_centroid(&high_tone, sample_rate);
        assert!(
            high_centroid > low_centroid * 2.0,
            "High tone centroid ({}) should be much larger than low tone centroid ({})",
            high_centroid,
            low_centroid
        );
    }

    #[test]
    fn test_spectral_centroid_silence() {
        let silence = vec![0.0_f32; 1024];
        let centroid = compute_spectral_centroid(&silence, 48000);
        assert_eq!(centroid, 0.0, "Silence should have 0 centroid");
    }

    #[test]
    fn test_recording_waveform_basic() {
        // Generate 1 second of 440Hz tone at 48kHz
        let sample_rate = 48000;
        let mono: Vec<f32> = (0..sample_rate)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / sample_rate as f32).sin())
            .collect();
        let result = compute_recording_waveform(&mono, sample_rate as u32, 100, 1.0);
        assert_eq!(result.peaks.len(), 100);
        assert_eq!(result.centroids.len(), 100);
        assert!((result.duration - 1.0).abs() < 0.001);
        // All peaks should be positive (sine wave)
        assert!(result.peaks.iter().all(|&p| p > 0.0));
        // All centroids should be near 440Hz
        for c in &result.centroids {
            assert!(*c > 300.0 && *c < 600.0, "Centroid {} should be near 440Hz", c);
        }
    }

    #[test]
    fn test_recording_waveform_empty() {
        let result = compute_recording_waveform(&[], 48000, 100, 0.0);
        assert!(result.peaks.is_empty());
        assert!(result.centroids.is_empty());
    }
}
