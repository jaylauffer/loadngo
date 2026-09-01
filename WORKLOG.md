# Collaboration Worklog

| UTC time | Repo | Branch | Device | Claimed paths | Status |
|---|---|---|---|---|---|
| 2026-08-06T00:13:01Z | loadngo | dev | current Codex workspace | `host-desktop/src/audio.rs`, audio documentation | completed 2026-08-06T00:54:41Z |
| 2026-08-31T23:26:37Z | loadngo | dev | current Codex workspace | `docs/PROACTOR_ENGINE_ADOPTION.md`, `docs/PROACTOR_ARCHITECTURE.md`, `README.md` | completed 2026-09-01T07:12:33Z (closed out by a follow-up Claude Code session after Codex hit a usage limit; content verified against current code, unchanged) |
| 2026-09-01T11:50:27Z | loadngo | dev | macmini (Claude Code workspace) | `host-desktop/src/proactor_driver.rs` (new), `host-desktop/src/macos.rs`, `host-desktop/src/linux.rs`, `docs/PROACTOR_ENGINE_ADOPTION.md`, `docs/PROACTOR_ARCHITECTURE.md` | completed -- Linux fix (6ceb7ae9) verified directly on dolores: full loadngo-proactor test suite passed, io_uring_enter dropped from ~45k/sec to ~237/sec on a live sng-roguelite-game instance, real playtest confirmed smooth |
| 2026-09-01T20:15:00Z | loadngo | dev | macmini (Claude Code workspace) | `host-desktop/src/audio.rs`, `host-desktop/src/audio_mixer.rs` (new), `host-desktop/Cargo.toml`, `docs/AUDIO.md` | in_progress -- cross-platform AudioMixer/AudioPreferences (see /Users/jay/.claude/plans/streamed-tinkering-beaver.md), fixes the multi-OutputStream ALSA device race found on dolores (BGM silently failing while SFX played) |
