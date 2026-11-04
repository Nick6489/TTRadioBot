//! The only application FFI boundary. See docs/sdk.md for verified native semantics.
use crate::stream::Pcm;
use teamtalk::Client;
use teamtalk_sys as ffi;
pub fn insert(client: &Client, pcm: &Pcm, id: i32) -> bool {
    if pcm.channels == 0
        || !pcm.samples.len().is_multiple_of(pcm.channels)
        || pcm.samples.is_empty()
    {
        return false;
    }
    let block = ffi::AudioBlock {
        nStreamID: id,
        nSampleRate: pcm.rate as i32,
        nChannels: pcm.channels as i32,
        nSamples: (pcm.samples.len() / pcm.channels) as i32,
        lpRawAudio: pcm.samples.as_ptr().cast_mut().cast(),
        ..Default::default()
    };
    // SDK copies PCM synchronously into its queue; this buffer remains alive for the call.
    client.insert_audio_block(&block)
}
pub fn end(client: &Client, reset_id: i32) {
    // NULL queues end-of-input, it DOES NOT discard queued audio. A new stream ID
    // destroys the previous input streamer first. Queue one silent stereo frame
    // under a fresh ID, then terminate that replacement session.
    let _ = insert(
        client,
        &Pcm {
            samples: vec![0, 0],
            rate: 48000,
            channels: 2,
        },
        reset_id,
    );
    // SAFETY: raw_ptr belongs to this live client, used only on its event thread.
    // Null is explicitly allowed by TT_InsertAudioBlock and is missing in wrapper.
    unsafe {
        let _ = ffi::api().TT_InsertAudioBlock(client.raw_ptr(), std::ptr::null());
    }
}
