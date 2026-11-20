use std::collections::VecDeque;
use symphonia_core::dsp::{complex::Complex, fft::Fft};

pub const BANDS: usize = 80;
const WINDOW: usize = 2048;

pub(crate) struct Analyzer {
    fft: Fft,
    window: Vec<f32>,
    input: Vec<Complex>,
    power: Vec<f32>,
    history: Vec<VecDeque<f32>>,
    sums: Vec<f32>,
    frames: usize,
    rate: u32,
    channels: u16,
}

impl Default for Analyzer {
    fn default() -> Self {
        Self {
            fft: Fft::new(WINDOW),
            window: (0..WINDOW)
                .map(|n| 0.5 - 0.5 * (std::f32::consts::TAU * n as f32 / (WINDOW - 1) as f32).cos())
                .collect(),
            input: vec![Complex::default(); WINDOW],
            power: vec![0.0; WINDOW / 2],
            history: Vec::new(),
            sums: Vec::new(),
            frames: 0,
            rate: 0,
            channels: 0,
        }
    }
}

impl Analyzer {
    pub fn reset(&mut self) {
        for channel in &mut self.history {
            channel.clear();
        }
        self.sums.fill(0.0);
        self.frames = 0;
    }

    pub fn analyze(&mut self, samples: &[f32], channels: u16, rate: u32) -> [f32; BANDS] {
        if channels == 0 || rate == 0 {
            return [0.0; BANDS];
        }
        if self.rate != rate || self.channels != channels {
            self.rate = rate;
            self.channels = channels;
            self.history = (0..channels)
                .map(|_| VecDeque::with_capacity(WINDOW))
                .collect();
            self.sums = vec![0.0; usize::from(channels)];
            self.frames = 0;
        }
        let step = (rate / 48_000).max(1) as usize;
        for frame in samples.chunks_exact(usize::from(channels)) {
            for (sum, sample) in self.sums.iter_mut().zip(frame) {
                *sum += if sample.is_finite() { *sample } else { 0.0 };
            }
            self.frames += 1;
            if self.frames == step {
                for (channel, sum) in self.history.iter_mut().zip(&mut self.sums) {
                    if channel.len() == WINDOW {
                        channel.pop_front();
                    }
                    channel.push_back(*sum / step as f32);
                    *sum = 0.0;
                }
                self.frames = 0;
            }
        }
        self.power.fill(0.0);
        let normalization = (2.0 / self.window.iter().sum::<f32>()).powi(2) / f32::from(channels);
        for channel in &self.history {
            self.input.fill(Complex::default());
            let mean = channel.iter().sum::<f32>() / channel.len().max(1) as f32;
            let offset = WINDOW - channel.len();
            for (index, sample) in channel.iter().enumerate() {
                self.input[offset + index].re = (sample - mean) * self.window[offset + index];
            }
            self.fft.fft_inplace(&mut self.input);
            for (power, bin) in self.power.iter_mut().zip(&self.input) {
                *power += (bin.re * bin.re + bin.im * bin.im) * normalization;
            }
        }
        let effective_rate = rate as f32 / step as f32;
        let upper = (effective_rate * 0.5).min(20_000.0).max(21.0);
        std::array::from_fn(|index| {
            let low = 20.0 * (upper / 20.0).powf(index as f32 / BANDS as f32);
            let high = 20.0 * (upper / 20.0).powf((index + 1) as f32 / BANDS as f32);
            let first =
                ((low * WINDOW as f32 / effective_rate).floor() as usize).clamp(1, WINDOW / 2 - 1);
            let last = ((high * WINDOW as f32 / effective_rate).ceil() as usize)
                .clamp(first + 1, WINDOW / 2);
            let power = self.power[first..last].iter().copied().fold(0.0, f32::max);
            ((10.0 * power.max(1e-12).log10() + 72.0) / 72.0).clamp(0.0, 1.0)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fft_tracks_real_frequencies_and_preserves_opposite_phase_stereo() {
        for rate in [44_100, 48_000, 96_000, 192_000] {
            for frequency in [200.0, 1_000.0, 8_000.0] {
                let samples: Vec<_> = (0..rate / 10)
                    .flat_map(|n| {
                        let sample = (std::f32::consts::TAU * frequency * n as f32 / rate as f32)
                            .sin()
                            * 0.8;
                        [sample, -sample]
                    })
                    .collect();
                let levels = Analyzer::default().analyze(&samples, 2, rate);
                let peak = levels
                    .iter()
                    .enumerate()
                    .max_by(|a, b| a.1.total_cmp(b.1))
                    .unwrap()
                    .0;
                let expected = (frequency / 20.0).ln() / (20_000.0_f32 / 20.0).ln() * BANDS as f32;
                assert!(
                    (peak as f32 - expected).abs() < 3.0,
                    "rate={rate}, frequency={frequency}, peak={peak}"
                );
            }
        }
    }

    #[test]
    fn silence_dc_and_reset_have_no_spectrum() {
        let mut analyzer = Analyzer::default();
        for samples in [vec![0.0; 4800], vec![0.5; 4800], vec![f32::NAN; 4800]] {
            analyzer.reset();
            assert!(
                analyzer
                    .analyze(&samples, 1, 48_000)
                    .iter()
                    .all(|value| *value == 0.0)
            );
        }
    }
}
