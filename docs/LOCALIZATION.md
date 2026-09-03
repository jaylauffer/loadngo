# Localization Roadmap

## Status and purpose

Status: **Phase 1 implemented** (2026-09-03). This is `loadngo`-scoped,
not any one game's, because the problem is shared across every game
built on the engine — `sng-roguelite`, `sng-rusty`, and `sng-zhoenus` all
have the exact same gap today. Raised while designing reward-card icons
for `sng-roguelite` (adding a visual layer to cards made it obvious that
text layout — and therefore localization readiness — needed a real
answer, not an assumption of fixed-length English strings forever).

## Current state before this work

- **No i18n/l10n infrastructure existed anywhere** — not in `loadngo`,
  not in any of the three games. Every player-facing string was a
  hardcoded literal directly in each game's UI code (`sng-roguelite`'s
  `game-app/src/lib.rs` alone had roughly 290 string-literal sites).
- **A font *fallback chain* mechanism already existed** in
  `loadngo/renderer` (`FontCatalogManifest.fallback_fonts`,
  `FontFaceManifest::candidate_paths`) — real infrastructure, but never
  wired up or exercised by any game. It resolves a list of *candidate
  font file paths* to try; it does not yet do anything about which fonts
  are actually bundled.
- **Every bundled font in `loadngo/assets/fonts/`** (mechanical-font,
  rocketfuel-font, borkboerk-font, and the rest) **is a stylized,
  almost-certainly Latin-only display font.** Even with the fallback
  chain wired up, there is currently nothing in the asset set with broad
  glyph coverage (CJK, Cyrillic, Arabic, etc.) to fall back *to*.

So this is two genuinely independent problems: **what text to show**
(string catalog + lookup) and **whether it can render at all** (font
glyph coverage). Phase 1 below is the first problem only.

## Decision: locale-keyed RON catalogs, as shared `loadngo` infrastructure

Adopted 2026-09-03, with the user's explicit direction to use the
locale-keyed `.ron` approach rather than pulling in an existing i18n
crate (`fluent`, `gettext`, etc.) — this matches the project's existing
content-as-data convention (`items.ron`, `encounters.ron`, and every
other `sng-roguelite` catalog already work this way), so localized
strings are just another catalog, not a new kind of thing to learn.

Solved once in `loadngo` and adopted per-game, the same shape as the
`AudioMixer` work (`[[loadngo_audiomixer_device_race]]`) — not
reinvented three times.

## Phase 1 — done, then revised same-day (2026-09-03): catalog format, lookup API, and OS locale detection

New crate: `loadngo/localization` (package `loadngo-localization`),
mirroring `sng-roguelite/crates/game-data`'s already-proven
content-catalog pattern (`parse_*_ron` + `validate_*`,
`schema_version`/`revision` header fields, a diagnostics-collecting
validator that reports every problem found, not just the first) rather
than inventing a new convention for content that happens to be strings.

- `LocaleCatalogDefinition` — one file per locale (e.g.
  `assets/localization/de.ron`), `{ schema_version, locale, revision,
  strings: HashMap<String, String> }`. Keys are opaque lookup strings
  (`"title.press_to_start"`, or `"item.<id>.description"` for catalog
  content — see below), not the English text itself.
- `parse_locale_catalog_ron` / `validate_locale_catalog` — same
  two-function shape as `game-data`'s item/reward/encounter catalogs.
- **`Localizer::t(key, default) -> &str`** — revised from the original
  `t(key) -> String` design after the user raised a real problem with
  it: `sng-roguelite`'s `items.ron` contains item descriptions inline,
  and forcing that content through an opaque-key-only catalog would mean
  either duplicating it into a committed English locale file (a second,
  redundant source of truth) or making `items.ron` itself keys-only and
  much less readable for content design. Resolved by making English
  *always* the caller-supplied `default` — for UI chrome, the literal
  being migrated; for catalog content, the field already authored in
  RON. **There is deliberately no committed English catalog anywhere,
  for either game.** A locale catalog only ever needs to contain actual
  translations; an untranslated key falls straight through to the
  always-correct default, so a partial translation is never broken, just
  incomplete. Revised before any real adoption existed (confirmed zero
  callers at the time), so this was a clean swap, not a breaking migration.
- `stable_key_from_text(context, text) -> String` — an FNV-1a 64-bit
  content-hash key helper, deliberately the same algorithm as
  `sng-rusty`'s `stable_line_id` (`src/bin/export_lines.rs`), lifted in
  directly per the user's explicit direction to model `sng-rusty`'s good
  design rather than rediscovering it later. For content with no natural
  stable identifier of its own (unlike an item, which already has an
  authored `id` and should just use a structured `item.<id>.*` key
  instead — content-hashing an item description would spuriously
  invalidate translations on a copyedit-only fix, which structured keys
  avoid).
- **`system_locale()`** — the platform-agnostic concept the user asked
  for explicitly: one function per `loadngo-host-desktop` platform
  backend (macOS/iOS via `NSLocale.preferredLanguages`, Android via
  `Locale.getDefault()` through JNI, Linux/netbsd/other via
  `LANGUAGE`/`LC_ALL`/`LANG`, Windows via `GetUserDefaultLocaleName`),
  normalized down to a bare base-language tag via a shared
  `base_language_tag` helper, always returning `"en"` as the ultimate
  default when the OS gives nothing usable. Verified for real on macOS
  (built and ran a throwaway binary calling it — returned `"en"`
  correctly); iOS/macOS clippy clean; Android/Windows verified by careful
  reading of this exact codebase's own established JNI/windows-rs
  patterns and (for Windows specifically) the actual `windows-0.58.0`
  crate source, since this session has no working cross-compile target
  for either platform to build-check directly — real confirmation for
  those two still needs an on-device run.
- 11 unit tests: parse/validate success and every validation failure
  mode, `Localizer::t`'s three resolution paths (real translation, miss
  falling through to default, no catalog loaded at all), and
  `stable_key_from_text`'s determinism/context-sensitivity/
  text-sensitivity.

Verified: `cargo build` / `clippy --all-targets --all-features -D
warnings` / `fmt --check` / `cargo test`, all clean for every target this
session can actually build (macOS native, iOS cross-check). Android
blocked on this session's missing NDK cross-compiler outside
`android_device_build.sh`'s own env setup (pre-existing, unrelated to
this change); Windows blocked on this session's known MSVC/`blake3`
cross-compile gap (also pre-existing).

## Explicitly not done yet

- **Font glyph-coverage wiring.** The `fallback_fonts` mechanism in
  `loadngo/renderer` is still unused by any game, and no broad-coverage
  font is bundled yet. Without this, a real (non-English, non-Latin-only)
  locale catalog could parse and look up fine while still rendering as
  tofu/missing glyphs on screen. English-only adoption (the current plan
  for `sng-roguelite` and `sng-zhoenus`) doesn't need this yet.
- **`sng-rusty` stays untouched.** Explicit user direction (2026-09-03):
  it's a visual novel engine, already text-heavy, with its own working
  voiceover tooling built around `stable_line_id`-style content hashing.
  `stable_key_from_text` above is `loadngo-localization`'s own copy of
  that same algorithm, lifted in now per the user's request rather than
  waiting for a future full lift of `sng-rusty`'s tooling into `loadngo`
  — but `sng-rusty` itself keeps its own separate system for now.
- **Real translated (non-English) catalog content.** Phase 1 is the
  mechanism; no `de.ron`/`ja.ron`/etc. exists for any game yet.

## Phase 2 — done (2026-09-03): `sng-zhoenus` and `sng-roguelite` adoption

- **`sng-zhoenus`**: full migration. Its entire player-facing text surface
  turned out to be four HUD strings (`src/render.rs`'s `push_hud`) — wave
  label, countdown, all-waves-complete, score. `src/localization.rs`
  wires `system_locale()` + `Localizer`, threaded as an explicit
  parameter through `build_paint_ops`/`push_hud` (small enough call graph
  that parameter-threading was simpler than a global). Verified: build/
  clippy/fmt/test clean, and a live run of the release binary produced no
  errors with no catalog file present (HUD renders via English defaults).
- **`sng-roguelite`**: full migration, 73 `localization::t(...)` call
  sites across `crates/game-app/src/lib.rs` (title screen, HUD,
  run-summary, achievements, reward-draft, sound settings, item labels/
  descriptions, room-role labels). Unlike `sng-zhoenus`, this game's
  render call graph is deep enough (many layered functions) that
  threading `&Localizer` as a parameter everywhere wasn't worth it —
  `crates/game-app/src/localization.rs` instead holds it in a
  process-wide `OnceLock` (the same shape `loadngo/host-desktop/src/
  audio.rs`'s `AUDIO_BACKEND_FAILURE` already uses), with a free
  function `localization::t(key, default)` callable from anywhere in the
  crate with zero signature changes needed at any call site. Item labels/
  descriptions are keyed by the item's own `id` (`item.<id>.label`/
  `.description`), looked up at the display layer with the RON-authored
  text as `default` — `items.ron` itself is completely untouched, stays
  exactly as readable as it always was. Same treatment for the HUD's
  room-role word ("Combat"/"Reward"/"Recovery"/"Elite"/"Guardian"): its
  accessor lives in `game-core`, which is deliberately engine-agnostic
  (no `loadngo` dependency), so the localization lookup happens in
  `game-app` at the point of display, not in `game-core` itself.
  Deliberately left un-migrated: the window title and title-screen logo
  text (product identity, not translatable content — same judgment call
  as `sng-zhoenus`'s window title), the achievements-close-button "X"
  glyph (a universal symbol), internal widget-constructor label
  arguments that are never actually painted (this game paints its own
  text via separate `RenderOp::Text` calls), and `format_playtest_report`
  (a terminal/developer diagnostic tool, not in-game text). Verified:
  build/clippy/fmt clean, 108 tests passing, and a live run of the
  release binary produced no errors with no catalog file present.

## Sequencing for later phases (not scheduled)

1. Font glyph-coverage: source/bundle one broad-coverage fallback font,
   wire `FontCatalogManifest.fallback_fonts` all the way through to
   actual glyph rasterization, verify a real non-Latin string renders
   correctly on a real device. Necessary before either game's adoption
   is useful for any language `loadngo`'s current fonts don't cover.
2. Real translated catalog content for at least one additional language,
   for at least one game, to prove the whole pipeline under real (not
   synthetic-test) conditions.

Also worth remembering while doing any of this: card/label layouts
designed assuming fixed-length English strings (e.g. the reward-card
work happening in the same conversation this roadmap came out of) should
budget real slack for translated text commonly running 30-50% longer.
