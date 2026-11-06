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
pub fn needs_audio(submitted: u64, elapsed_ms: u32, rate: u32) -> bool {
    (submitted * 1000 / u64::from(rate)).saturating_sub(u64::from(elapsed_ms)) < TARGET_BUFFER_MS
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
