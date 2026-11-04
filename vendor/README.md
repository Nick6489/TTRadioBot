# TeamTalkRust patch

`teamtalk/` is the MIT-licensed `teamtalk` crate source from TeamTalkRust revision
1f4eb1578b14084becf491285e3482bfbc8c583f (6.0.0). Only its source, manifest, README
and license are included; upstream tests/examples are not needed to build it.

Local changes:

- src/client/channels.rs: allow joining from Joined as well as LoggedIn so
  channel changes (including returning home after an administrator move) reach
  the native SDK. Concurrent joins remain blocked.

- Cargo.toml: spell out optional teamtalk-macros version instead of inheriting a
  workspace dependency outside this repository.
- src/lib.rs: a process-local OnceLock records successful SDK loading. `init()`
  returns immediately if it was already loaded, including by `init_with_path()`.
  Upstream Client::new and set_license unconditionally call init, which otherwise
  contacts the SDK download site and ignores the explicitly configured path.
  All application initialization is sequential before creating clients.

The raw null-input operation remains in the application's documented adapter.
No SDK binaries or proprietary SDK source are vendored. When updating the pin,
reapply/review this patch and rerun the native startup/integration test.
