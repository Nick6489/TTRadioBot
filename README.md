# TTRadioBot

A small Linux Rust bot that relays a live HTTP/HTTPS MP3 stream into TeamTalk.
Symphonia decodes MP3 to interleaved signed 16-bit PCM; volume is applied before
`TT_InsertAudioBlock`. Stream requests identify the bot with
`User-Agent: TTRadioBot/<version>`. No playback subprocess, sound device, audio server,
loopback, or local media library is used.

## Build and SDK installation

Install Rust stable (edition 2024), a C/C++ build toolchain, pkg-config, CMake and
libclang. Debian build packages: `build-essential pkg-config cmake libclang-dev`.
Build dependencies generate bindings; the native SDK is loaded at runtime.

```
cargo build --release --locked
install -Dm755 target/release/ttradiobot ~/.local/bin/ttradiobot
```

Download and extract the [TeamTalk SDK](https://bearware.dk/?page_id=419) for your
architecture. Install its `Library/TeamTalk_DLL/libTeamTalk5.so` under
`~/.local/lib/teamtalk/`, and set `sdk_library` to its absolute path. Follow the
SDK license terms; vendor trial packages require a license for continuing use.
TTRadioBot does not download executable SDK files at startup.

Use `ldd /absolute/path/libTeamTalk5.so` to check dependencies. The Ubuntu 22
v5.19a SDK is the baseline used here. ALSA/PulseAudio **client shared libraries**
can be required by the SDK loader even though the bot opens no sound device and
needs no PulseAudio daemon. See [SDK details](docs/sdk.md) for exact inspection
results, TLS limitations, queue semantics and musl deployment.

## Configuration and first run

```
install -dm700 ~/.config/ttradiobot ~/.local/state/ttradiobot
install -m600 config.example.toml ~/.config/ttradiobot/config.toml
# Edit config.toml: server, credentials, controller accounts, absolute paths.
~/.local/bin/ttradiobot ~/.config/ttradiobot/config.toml
```

Configuration is TOML with unknown fields rejected. Set `no_gender = true` to
display the neutral/no-gender icon; omission preserves the existing gender.
 TLS/plain mode is explicit.
`insecure_tls = true` disables TeamTalk certificate verification only when
explicitly requested; HTTP HTTPS verification remains enabled. Read the
[SDK TLS caveat](docs/sdk.md#tls) before choosing trusted CAs.

On first run there is no saved URL and playback is disabled. `default_volume`
(default 100) applies only when no state file exists. A separate protected JSON
file stores the single saved URL, volume, and enabled setting. State changes use
mode 0600, file synchronization, atomic rename, and directory synchronization.
Malformed state causes startup to fail; fix it or deliberately remove it to
start fresh. The process never quietly overwrites invalid state. Paths are used
literally and relative paths resolve from the working directory; use absolute
paths in service configurations. Protect configuration and state backups too.

## Commands and authorization

Send private TeamTalk text messages to the bot:

| Command | Result |
| --- | --- |
| `u URL` | Save URL, enable playback, switch stream |
| `s` | Stop and disable playback |
| `p` | Enable the saved URL; error if none exists |
| `v NUMBER` | Save volume 0–100; 100 is unity gain, 0 silence |
| `i` | URL host, title, volume, desired setting, actual state |
| `h` | Return home; report already home, success, or the server error |
| `?` | Command help |

Only exact authenticated usernames in `controllers` can control the bot,
including `h`. Sender identities are resolved from current SDK user information.
Nicknames, numeric IDs and administrator privileges grant no authority. Do not
allowlist shared guest accounts. Empty identities are never authorized.
Everyone may request help/info. Replies are private. Information hides URL
userinfo, path, query and fragment, because any of these can contain tokens.
Use the protected state file to inspect the full saved URL locally.

`channel_commands = true` also accepts commands in the bot's actual channel.
Keep it false (default) when stream URLs contain credentials: posting a URL to a
channel exposes it to other channel members regardless of the bot's replies.
Use one text message per command. A failed save rejects the change; only a
successful save is acknowledged as applied.

## Playback behavior

Desired state and actual state are independent. Actual states are Stopped,
Waiting, Connecting, Playing, and Retrying. Ordinary channel users count as
listeners; the bot and accounts in `excluded_listeners` do not. Empty-channel
suspension waits `grace_seconds` (default 5); returning before it expires cancels
the suspension. Suspension cancels network/decoder work, retries, and queued
injection, but retains the saved URL and enabled setting. Returning listeners
reconnect at the live position. `s` persists disabled playback, so joins cannot
restart it. Shutdown preserves user intention as well.

The TeamTalk event loop stays responsive while asynchronous HTTP feeds a
separate blocking decoder through bounded channels. Switching destroys the old
worker's receivers, so obsolete audio and title updates cannot reach the new
session. Redirects stop after five hops. Read/connect deadlines and capped
exponential backoff bound failures; sustained playback resets radio backoff.
Format changes start a fresh injection session; decoder reset errors reconnect.
ICY length bytes and metadata are removed before decoding across arbitrary read
boundaries. Missing/empty titles use the station name or playback state. Status
updates are deduplicated and limited to 511 UTF-8 bytes without splitting a
character. Metadata is interpreted as UTF-8 with replacement for invalid bytes.

Volume changes use the current setting when submitting PCM, without reopening
the stream. A small amount already submitted (about 300 ms plus a decoder frame
and channel codec buffering) retains the previous gain. No amplification or
speech processing is enabled. TeamTalk account permissions, channel codec,
channel transmission policy, and server flood limits still apply.

## systemd user service

Create the state directory first, then:

```
install -Dm644 systemd/ttradiobot.service ~/.config/systemd/user/ttradiobot.service
systemctl --user daemon-reload
systemctl --user enable --now ttradiobot
systemctl --user status ttradiobot
journalctl --user -u ttradiobot -f
systemctl --user stop ttradiobot
```

No root is required for runtime. To keep a user manager running after logout,
optional `loginctl enable-linger "$USER"` may require administrator authorization
according to the host policy. The unit allows state writes only under
`~/.local/state/ttradiobot`; adjust `ReadWritePaths` if moving the state file.
SIGINT/SIGTERM cancels streaming, ends injection, disconnects, and exits without
changing persisted enabled state. Logs omit URLs, credentials and message text.

## Validation

```
cargo fmt --check
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
cargo build --release --locked
cargo test hkc_live -- --ignored --nocapture
```

Offline tests cover command authorization/parsing, occupancy and grace periods,
manual stop, persistence/restart, Unicode/privacy, ICY fragmentation/truncation,
PCM gain, live HTTP decoding, stalled reads and switching/canceling workers.
The last command performs an optional real network decode smoke test; it is
separate from offline tests. See [manual integration checks](docs/validation.md)
for exact Linux/TeamTalk playback verification and recorded validation limits.
