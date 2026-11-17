//! Consumption accounting counts actual frames after application resampling.
use teamtalk::types::AudioCodec;

// Allow short network and scheduler delays without starving the SDK input.
const TARGET_BUFFER_MS: u64 = 300;
pub fn output_rate(codec: &AudioCodec) -> Option<u32> {
    match codec {
        AudioCodec::Opus(o) => u32::try_from(o.sample_rate).ok().filter(|r| *r > 0),
        AudioCodec::Speex(s) => speex_rate(s.bandmode),
        AudioCodec::SpeexVBR(s) => speex_rate(s.bandmode),
        _ => None,
    }
}
fn speex_rate(band: i32) -> Option<u32> {
    match band {
        0 => Some(8000),
        1 => Some(16000),
        2 => Some(32000),
        _ => None,
    }
}
pub fn queued_ms(submitted: u64, elapsed_ms: u32, rate: u32) -> u64 {
    (submitted * 1000 / u64::from(rate)).saturating_sub(u64::from(elapsed_ms))
}
pub fn needs_audio(submitted: u64, elapsed_ms: u32, rate: u32) -> bool {
    queued_ms(submitted, elapsed_ms, rate) < TARGET_BUFFER_MS
}
/// A duration-bounded reserve before the continuous converter and SDK queue.
/// Decoder blocks are at most 20 ms; the last block may cross the capacity.
pub struct Reserve {
    blocks: std::collections::VecDeque<crate::stream::Pcm>,
    duration: std::time::Duration,
    target_ms: u64,
    buffering: bool,
}
impl Default for Reserve {
    fn default() -> Self {
        Self {
            blocks: Default::default(),
            duration: std::time::Duration::ZERO,
            target_ms: 1000,
            buffering: true,
        }
    }
}
impl Reserve {
    pub fn full(&self) -> bool {
        self.duration >= std::time::Duration::from_secs(3)
    }
    pub fn push(&mut self, pcm: crate::stream::Pcm) {
        self.duration += Self::duration(&pcm);
        self.blocks.push_back(pcm);
    }
    pub fn pop(&mut self) -> Option<crate::stream::Pcm> {
        let pcm = self.blocks.pop_front()?;
        self.duration = self.duration.saturating_sub(Self::duration(&pcm));
        Some(pcm)
    }
    fn duration(pcm: &crate::stream::Pcm) -> std::time::Duration {
        std::time::Duration::from_secs_f64(
            (pcm.samples.len() / pcm.channels) as f64 / f64::from(pcm.rate),
        )
    }
    pub fn is_buffering(&self) -> bool {
        self.buffering
    }
    pub fn ready(&mut self) -> bool {
        if self.buffering && self.ms() >= self.target_ms {
            self.buffering = false;
            tracing::info!(
                reserve_ms = self.ms(),
                target_ms = self.target_ms,
                "Audio reserve ready"
            );
        }
        !self.buffering
    }
    pub fn underrun(&mut self) -> bool {
        if self.buffering {
            return false;
        }
        self.buffering = true;
        self.target_ms = (self.target_ms + 1000).min(3000);
        tracing::warn!(
            reserve_ms = self.ms(),
            refill_ms = self.target_ms,
            "Audio input underrun; refilling reserve"
        );
        true
    }
    pub fn ms(&self) -> u64 {
        self.duration.as_millis() as u64
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replenishes_below_three_hundred_ms() {
        for rate in [8000, 16000, 32000, 48000] {
            let consumed = u64::from(rate);
            let target = u64::from(rate) * TARGET_BUFFER_MS / 1000;
            assert!(!needs_audio(consumed + target, 1000, rate));
            assert!(needs_audio(consumed + target - 1, 1000, rate));
            assert!(needs_audio(consumed - 1, 1000, rate));
        }
    }
}

#[cfg(test)]
mod reserve_tests {
    use super::*;
    fn block(rate: u32, channels: usize) -> crate::stream::Pcm {
        crate::stream::Pcm {
            samples: vec![0; rate as usize / 50 * channels],
            rate,
            channels,
        }
    }
    #[test]
    fn startup_and_quarter_second_delivery_pause() {
        for (rate, channels) in [(8000, 1), (44100, 2), (48000, 2)] {
            let mut reserve = Reserve::default();
            for _ in 0..49 {
                reserve.push(block(rate, channels));
            }
            assert!(!reserve.ready());
            reserve.push(block(rate, channels));
            assert!(reserve.ready());
            // No incoming data during 260 ms of consumption.
            for _ in 0..13 {
                assert!(reserve.pop().is_some());
            }
            assert_eq!(reserve.ms(), 740);
            assert!(reserve.ready());
        }
    }
    #[test]
    fn repeated_starvation_requires_fuller_refills_and_reset_forgets_them() {
        let mut reserve = Reserve::default();
        for refill in [1000, 2000, 3000, 3000] {
            for _ in 0..refill / 20 - 1 {
                reserve.push(block(48000, 2));
            }
            assert!(!reserve.ready());
            reserve.push(block(48000, 2));
            assert!(reserve.ready());
            while reserve.pop().is_some() {}
            assert!(reserve.underrun());
            assert!(!reserve.underrun());
        }
        reserve = Reserve::default();
        while !reserve.full() {
            reserve.push(block(44100, 2));
        }
        assert_eq!(reserve.ms(), 3000);
        assert!(reserve.ready());
    }
}
