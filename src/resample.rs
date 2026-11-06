//! One continuous, band-limited converter per injection session.
use crate::stream::Pcm;
use anyhow::{Result, ensure};
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};

pub struct Converter {
    input_rate: u32,
    output_rate: u32,
    channels: usize,
    engine: Option<SincFixedIn<f32>>,
    buffered: Vec<Vec<f32>>,
}
impl Converter {
    pub fn new(input_rate: u32, output_rate: u32, channels: usize) -> Result<Self> {
        ensure!(
            input_rate > 0 && output_rate > 0 && (1..=2).contains(&channels),
            "invalid resampling format"
        );
        let engine = if input_rate == output_rate {
            None
        } else {
            Some(SincFixedIn::new(
                f64::from(output_rate) / f64::from(input_rate),
                1.0,
                SincInterpolationParameters {
                    sinc_len: 256,
                    f_cutoff: 0.95,
                    interpolation: SincInterpolationType::Cubic,
                    oversampling_factor: 256,
                    window: WindowFunction::BlackmanHarris2,
                },
                1024,
                channels,
            )?)
        };
        Ok(Self {
            input_rate,
            output_rate,
            channels,
            engine,
            buffered: vec![Vec::new(); channels],
        })
    }
    pub fn process(&mut self, pcm: Pcm) -> Result<Pcm> {
        ensure!(
            pcm.rate == self.input_rate && pcm.channels == self.channels,
            "resampler format changed"
        );
        ensure!(
            !pcm.samples.is_empty() && pcm.samples.len().is_multiple_of(self.channels),
            "invalid PCM frame"
        );
        let Some(engine) = &mut self.engine else {
            return Ok(pcm);
        };
        for frame in pcm.samples.chunks_exact(self.channels) {
            for (channel, sample) in self.buffered.iter_mut().zip(frame) {
                channel.push(f32::from(*sample) / 32768.0);
            }
        }
        let mut samples = Vec::new();
        let mut consumed = 0;
        // Reblock independently of MP3 packet sizes. Retain incomplete input for
        // the next call; zero-padding here would introduce periodic artifacts.
        while self.buffered[0].len() - consumed >= 1024 {
            let input: Vec<&[f32]> = self
                .buffered
                .iter()
                .map(|ch| &ch[consumed..consumed + 1024])
                .collect();
            let output = engine.process(&input, None)?;
            for frame in 0..output[0].len() {
                for channel in &output {
                    samples
                        .push((channel[frame] * 32768.0).round().clamp(-32768.0, 32767.0) as i16);
                }
            }
            consumed += 1024;
        }
        for channel in &mut self.buffered {
            channel.drain(..consumed);
        }
        Ok(Pcm {
            samples,
            rate: self.output_rate,
            channels: self.channels,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tone(rate: u32, frames: usize, frequency: f64) -> Vec<i16> {
        (0..frames)
            .map(|i| {
                (12000.0 * (std::f64::consts::TAU * frequency * i as f64 / rate as f64).sin())
                    .round() as i16
            })
            .collect()
    }
    fn convert(input: &[i16], from: u32, to: u32, chunks: &[usize]) -> Vec<i16> {
        let mut converter = Converter::new(from, to, 2).unwrap();
        let mut out = vec![];
        let mut pos = 0;
        let mut n = 0;
        while pos < input.len() {
            let end = (pos + chunks[n % chunks.len()]).min(input.len());
            let pcm = Pcm {
                samples: input[pos..end].iter().flat_map(|s| [*s, -*s]).collect(),
                rate: from,
                channels: 2,
            };
            let converted = converter.process(pcm).unwrap();
            assert_eq!(converted.rate, to);
            assert!(
                converted
                    .samples
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .all(|s| (i32::from(s[0]) + i32::from(s[1])).abs() <= 1)
            );
            out.extend(converted.samples.as_chunks::<2>().0.iter().map(|s| s[0]));
            pos = end;
            n += 1;
        }
        out
    }
    #[test]
    fn boundaries_preserve_waveform_and_fractional_timing() {
        for (from, to) in [(44100, 48000), (48000, 44100), (44100, 8000), (8000, 48000)] {
            let input = tone(from, from as usize * 3, 440.0);
            let a = convert(&input, from, to, &[1152]);
            let b = convert(&input, from, to, &[137, 2048, 576, 17]);
            assert_eq!(a.len(), b.len());
            assert!(
                a.iter()
                    .zip(&b)
                    .all(|(a, b)| (i32::from(*a) - i32::from(*b)).abs() <= 2)
            );
            // Filter latency is bounded and does not grow with decoder blocks.
            assert!(
                (a.len() as i64 - i64::from(to) * 3).abs()
                    < (1152 * u64::from(to)).div_ceil(u64::from(from)) as i64 + 2
            );
            let peak_step = a
                .iter()
                .skip(to as usize / 10)
                .zip(a.iter().skip(to as usize / 10 + 1))
                .map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs())
                .max()
                .unwrap();
            assert!(
                peak_step
                    < (12000.0 * std::f64::consts::TAU * 440.0 / f64::from(to)).ceil() as i32 + 20,
                "discontinuity: {from}->{to}, {peak_step}"
            );
        }
    }
    #[test]
    fn downsampling_rejects_aliases() {
        let input = tone(48000, 48000, 12000.0);
        let output = convert(&input, 48000, 16000, &[1152]);
        assert!(output.iter().skip(1000).all(|s| s.abs() < 30));
    }
    #[test]
    fn equal_rates_are_bit_exact() {
        let mut c = Converter::new(48000, 48000, 1).unwrap();
        let samples = vec![i16::MIN, -1, 0, 1, i16::MAX];
        let p = c
            .process(Pcm {
                samples: samples.clone(),
                rate: 48000,
                channels: 1,
            })
            .unwrap();
        assert_eq!(p.samples, samples);
    }
}
