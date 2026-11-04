# Validation and manual checks

## Reproduce automated checks

Install the build dependencies from README.md, then run:

```
cargo fmt --check
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
cargo build --release --locked
cargo test hkc_live -- --ignored --nocapture
```

The offline suite needs no SDK library or TeamTalk server. The HKC test contacts
`https://listen.hkcradio.com/hkc.mp3`, requests ICY data, and asserts that nonempty
mono/stereo PCM is decoded. It does not prove TeamTalk playback.

For the opt-in TeamTalk integration test, extract the Ubuntu 22 v5.19a Standard
SDK and run its server on loopback in a separate terminal. Use an isolated copy
of the fixture (the server rewrites its configuration):

```
mkdir -p /tmp/ttradiobot-test-server
cp tests/fixtures/teamtalk-server.xml /tmp/ttradiobot-test-server/server.xml
/absolute/path/to/SDK/Server/tt5srv -nd -c /tmp/ttradiobot-test-server/server.xml
```

This fixture has deliberately public **test-only** credentials, bound to
127.0.0.1 TCP/UDP port 19333. Never use these accounts on a public server. In
another terminal:

```
TT_SDK_LIBRARY=/absolute/path/to/SDK/Library/TeamTalk_DLL/libTeamTalk5.so \
  cargo test --test teamtalk -- --ignored --nocapture
```

The test starts/stops its own bot process and synthetic ICY MP3 HTTP server.
Only the receiving test client opens the SDK's virtual output device to inspect
remote decoded samples; the bot initializes no input/output device. No audio
daemon is needed. SDK initialization may print ALSA probing diagnostics on a
host without a sound card; those originate in the SDK, not a playback route.

## Recorded environment

Development validation uses Debian 13 x86_64, Rust 1.99.1, TeamTalkRust revision
1f4eb1578b14084becf491285e3482bfbc8c583f plus the documented initialization patch,
and the vendor Ubuntu 22 v5.19a Standard SDK (server 5.19.0.5170).
No PulseAudio or PipeWire process was running during the native tests.

`ldd` on this SDK's libTeamTalk5.so found direct dependencies on libssl.so.3,
libcrypto.so.3, libasound.so.2, libpulse.so.0, libm.so.6 and libc.so.6, plus the
glibc loader. libpulse transitively loads libpulsecommon and libraries including
D-Bus, libsndfile, X11/XCB, zlib and zstd on this host. Install distribution
packages for the direct libraries and let its package manager resolve these
transitive dependencies. Do not remove libpulse0 merely because its daemon is
not used. `ldd` must show no missing libraries for your actual SDK build.

See the repository completion report for actual command results. Production
server TLS interoperability (trusted custom CA, expired certificates, invalid
chains), remote administration moves/kicks, long-duration playback, and native
musl SDK runtime must also be checked on the deployment host. The container
recipe is supplied but requires Podman/Docker to verify.

## Manual playback, recovery, and occupancy checks

1. On an isolated Linux host, verify `pgrep -x pulseaudio` and
   `pgrep -x pipewire` return no running daemons. Do not stop services on a shared
   desktop just for this test; a separate server/container is preferable. Do not
   expose `/dev/snd` to a bot container.
2. Create an ordinary authenticated bot account allowed to join the home
   channel, transmit voice, and send private replies. Its status must **not** be
   locked. Configure a music-appropriate Opus channel with stereo if desired.
   Configure a distinct controller username in `controllers`, plus a distinct
   ordinary listener that is not a controller.
3. Start the bot with an absent state file. Send `i`: no URL, stopped, default
   volume. From an unlisted account, try `s`, `p`, `u URL`, `v 20`, and `h`:
   each must be rejected. Change that user's display nickname to a controller
   name and retry; it must remain unauthorized. `?` and `i` should work.
4. Keep the bot's channel empty. From a controller elsewhere send
   `u https://listen.hkcradio.com/hkc.mp3`. Confirm `i` reports enabled/Waiting
   and there is no voice transmission. Join as the ordinary listener. Verify
   live music, status titles changing, and `i` reporting Playing. Confirm that
   initial membership synchronization on bot restart does not suspend a channel
   already containing listeners.
5. Send `v 0`, `v 50`, `v 100` while listening. Check silence, reduced level and
   original level, respectively, without an HTTP reconnect. `v -1`, `v 101`,
   missing URL and extra arguments should receive concise errors. If testing
   credential URLs, use private messages; `i`, logs and status must not expose
   the credentials.
6. Leave the channel. Return before five seconds: playback should continue.
   Leave longer: status becomes Waiting, HTTP closes and transmission ends.
   Return: music resumes at the live position. Add an excluded bot account:
   its presence alone must not keep playback running.
7. Send `s`, leave, and rejoin. It must stay stopped with no stale title. Send
   `p`: it resumes. Switch with `u` to another valid MP3 stream while the old
   stream is active/stalled: old audio/title must not return. Try an invalid
   stream: it should retry at capped intervals while commands remain responsive.
8. Temporarily interrupt the radio source/network or use a local HTTP fixture
   that stalls mid-frame. Check bounded retry logs, responsive `i`/`s`, and
   fresh audio after recovery. Temporarily stop/restart the TeamTalk server:
   login and join should complete before occupancy-driven playback starts.
9. Have an administrator move the bot to an empty/occupied channel, move
   listeners between channels, and kick the bot. Occupancy must follow the
   bot's actual channel. Send `h` from a controller to return home; check the
   server permits the join.
10. While enabled, `systemctl --user restart ttradiobot`: enabled state must
    survive, playing only with listeners. Stop manually using `s`, restart the
    service: it must remain disabled. Make the state directory unwritable and
    try changing volume: the command must report a save failure and leave the
    active desired state unchanged. Restore permissions afterward.
11. Send SIGTERM and SIGINT during active playback and during a stalled HTTP
    connection. Check prompt exit, ended transmission, no old worker left
    running, and an unchanged persisted enabled setting.
12. For TLS test with a trusted CA/self-signed PEM, then a wrong CA. The latter
    must fail and retry without insecure fallback. Check an explicit
    `insecure_tls=true` configuration logs its choice. Remember that the SDK
    chain verification does not authenticate DNS names (docs/sdk.md).

## Rootless container on a musl-based host

Install rootless Podman using the host's package manager. Build:

```
podman build -f Containerfile -t localhost/ttradiobot .
```

Prepare config with `sdk_library = "/sdk/libTeamTalk5.so"` and
`state_file = "/state/state.json"`, then run as your unprivileged account:

```
podman run --rm --name ttradiobot --network host --userns keep-id --user "$(id -u):$(id -g)" \
  -v "$HOME/.config/ttradiobot:/config:ro,Z" \
  -v "$HOME/.local/state/ttradiobot:/state:Z" \
  -v "$HOME/.local/lib/teamtalk:/sdk:ro,Z" \
  localhost/ttradiobot
```

The SDK and all its dependencies execute inside the glibc container. No audio
socket/device is mounted. On systems without SELinux omit `,Z`. If a systemd
user manager is available, replace ExecStart in the supplied unit with this
Podman command using absolute paths and add
`ExecStop=/usr/bin/podman stop -t 5 ttradiobot`; relax the unit's filesystem
sandbox as required for rootless Podman's storage. Alpine itself commonly uses
OpenRC instead of systemd; its supervisor can run the same command as a normal
user. Native musl builds require a matching native SDK, as documented in sdk.md;
the vendor Ubuntu binary cannot substitute for one.
