# SDK boundary and deployment

TeamTalkRust is pinned to the Git revision in Cargo.toml; Cargo.lock pins all
transitive dependencies. Its `teamtalk` crate owns connectivity and event
processing. `teamtalk-sys` supplies the matching runtime-loaded API. The only
application unsafe call is the null-input adapter in `src/native.rs`.

## Verified source semantics

Reviewed upstream TeamTalkRust audio blocks, encryption, channel/user APIs and
SDK v5 source. Primary references:

- [TeamTalkRust source](https://github.com/BlindMaster24/TeamTalkRust/tree/1f4eb1578b14084becf491285e3482bfbc8c583f)
- [SDK insertion and native conversion](https://github.com/BearWare/TeamTalk5/blob/7153c2ce4c6e09268f54f83304671d8e33103914/Library/TeamTalkLib/bin/dll/TeamTalk.cpp)
- [ClientNode queue and audio callbacks](https://github.com/BearWare/TeamTalk5/blob/7153c2ce4c6e09268f54f83304671d8e33103914/Library/TeamTalkLib/teamtalk/client/ClientNode.cpp)
- [AudioInputStreamer](https://github.com/BearWare/TeamTalk5/blob/7153c2ce4c6e09268f54f83304671d8e33103914/Library/TeamTalkLib/avstream/AudioInputStreamer.cpp)
- [PCM buffer copy](https://github.com/BearWare/TeamTalk5/blob/7153c2ce4c6e09268f54f83304671d8e33103914/Library/TeamTalkLib/codec/MediaUtil.cpp)
- [C API documentation](https://bearware.dk/teamtalksdk/v5.19a/docs/C-API/)

Injection replaces voice input, without initializing any sound device. Do not
call `enable_voice_transmission(true)` or voice activation: they conflict with
injection. PCM is native-endian signed 16-bit, interleaved for stereo, with
`nSamples = scalar_samples / channels`. Mono and stereo are accepted; other
channel layouts are rejected. `uSampleIndex` is ignored. `nStreamID` identifies
the injection session and progress events. AudioInputStreamer copies submitted
frames synchronously into message blocks; the caller may release PCM after the
call returns. It resamples and converts channels to the channel codec format,
and rejects an input format change within the same session. The bot starts a
new session on format changes. `queue.rs` mirrors native CalcSamples rounding,
so duration credits remain aligned over long sessions.

The SDK queue is finite and insertion can fail. The bot normally keeps about
100 ms plus one MP3 frame outstanding, tracking cumulative submitted duration
against `AudioInputProgress.elapsed_ms`, accounting for each frame
rounded into the channel sample rate exactly as the native resampler does. It replenishes using consumption events,
not wall-clock audio pacing. Failure preserves one pending PCM frame and waits
for consumption; a stalled-input deadline reconnects instead of looping forever.
Compressed input is bounded to two 1024-byte chunks, with four decoded frames
plus one pending frame; Symphonia and the HTTP client also have finite internal
buffers. An output/backpressure timeout prevents prolonged stale connections.

**NULL does not clear the queue.** SDK `QueueAudioInput` calls `Flush`, appending
an end marker. A different nonzero stream ID destroys the old input streamer,
discarding its queued samples. Stop/switch uses a new ID with one silent stereo
sample frame, then NULL to end that replacement session. A packet already handed
to the encoder/network cannot be recalled. This bounds any audible tail to
encoder/network buffering, rather than draining the old radio queue.

The pinned wrapper accepts only `&AudioBlock`, so cannot express NULL. The small
adapter calls `TT_InsertAudioBlock(client.raw_ptr(), null())` on the event thread,
with a live owned Client. The adapter adds no wider unsafe surface. A separate minimal initialization
patch is vendored because upstream Client::new re-runs SDK discovery after an
explicit library load; see vendor/README.md.

Injected PCM is delivered through `AudioInputCallback -> QueueVoiceFrame ->
AudioThread`, so voice preprocessing settings matter. The bot explicitly selects
`AudioPreprocessor::None`, retaining unity gain and disabling Speex/WebRTC AGC,
noise suppression and echo cancellation. Never initialize a capture device.

## TLS

`EncryptionContext` exposes a CA file/directory, peer verification and depth.
The bot enables chain verification by default for TLS, using the configured PEM
`ca_file` or `/etc/ssl/certs/ca-certificates.crt`. A missing CA bundle fails startup.
A dedicated PEM containing a self-signed server certificate or its issuing CA
can be configured. No automatic insecure fallback exists.

**Limitation:** the SDK exposes certificate chain validation, but no hostname/SAN
validation option. Reviewed native conversion uses ACE/OpenSSL verification and
does not set an expected DNS hostname. Thus ordinary chain verification is
available, but HTTPS-style hostname authentication is not supplied by this SDK
API. Prefer a dedicated server CA/certificate, and consider a verified transport
tunnel if DNS identity validation is required. `insecure_tls = true` explicitly
disables chain verification and logs that choice. HTTP streaming uses rustls with
normal certificate and hostname verification independently of TeamTalk settings.

## Native libraries and musl

The vendor provides Ubuntu/glibc libraries; do not dlopen a glibc SDK into a
native musl process or claim that a static Rust executable removes this dependency.
A native musl build requires a matching musl-built TeamTalk SDK and its native
dependencies from your SDK supplier/build. Rust must use dynamic musl CRT support
(`-C target-feature=-crt-static`) because the binding loads the SDK with dlopen.
The binding build also needs libclang and the target C headers. Native musl SDK
availability/runtime has not been verified here.

For a concrete deployment on musl-based hosts without mixing libc ABIs, run the
glibc bot and SDK together in an unprivileged glibc container (Podman/rootless
Docker), bind-mount protected config and state, and enable host networking for
TeamTalk UDP. The included Containerfile provides the build/runtime environment.
The host need not run PulseAudio or expose `/dev/snd`. The systemd user unit can
instead invoke the rootless container command documented in validation.md.

Inspect each supplied library with `ldd`/`readelf -d`; runtime dependencies vary
by SDK build. Library dependencies do not imply the corresponding audio daemons
must run. See validation.md for the exact library tested in this workspace.
