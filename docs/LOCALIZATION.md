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

## Phase 1 — done (2026-09-03): catalog format and lookup API

New crate: `loadngo/localization` (package `loadngo-localization`),
mirroring `sng-roguelite/crates/game-data`'s already-proven
content-catalog pattern (`parse_*_ron` + `validate_*`,
`schema_version`/`revision` header fields, a diagnostics-collecting
validator that reports every problem found, not just the first) rather
than inventing a new convention for content that happens to be strings.

- `LocaleCatalogDefinition` — one file per locale (e.g.
  `assets/localization/en.ron`), `{ schema_version, locale, revision,
  strings: HashMap<String, String> }`. Keys are opaque lookup strings
  (`"title.press_to_start"`), not the English text itself, so every
  locale's catalog — including the base/English one — has the same
  shape.
- `parse_locale_catalog_ron` / `validate_locale_catalog` — same
  two-function shape as `game-data`'s item/reward/encounter catalogs.
- `Localizer` — the single thing a game's UI code is expected to consult
  for any player-facing text (mirrors `sng-roguelite`'s `FormFactor`:
  one obvious fact to check, not something to re-derive ad hoc).
  `Localizer::t(key) -> String` looks up the primary (player-selected)
  catalog, falls back to a secondary catalog (typically the game's base/
  English one, so a partially-translated locale still shows *something*
  readable), and finally falls back to a visibly-broken `[[key]]`
  placeholder if the key exists in neither — never panics, and a missing
  translation is loud in testing rather than silently blank in
  production.
- 11 unit tests: parse/validate success and every validation failure
  mode, plus all three `Localizer::t` resolution paths (primary hit,
  fallback hit, placeholder).

Verified: `cargo build` / `clippy --all-targets --all-features -D
warnings` / `fmt --check` / `cargo test`, all clean.

## Explicitly not done in Phase 1

- **Locale selection/auto-detection.** `Localizer::new` takes already-
  loaded catalogs; nothing yet queries the OS for the player's locale
  (`Locale.getDefault()` on Android, `NSLocale` on iOS, `LANG`/similar on
  desktop). Each platform's `host-desktop` backend would need its own
  detection, the same shape as `SNG_ASSETS_ROOT` resolution already is
  per-platform.
- **Font glyph-coverage wiring.** The existing `fallback_fonts` mechanism
  is still unused by any game, and no broad-coverage font is bundled yet.
  Without this, a real (non-English, non-Latin-only) locale catalog could
  parse and look up fine while still rendering as tofu/missing glyphs on
  screen. This is necessary before Phase 1's catalogs are useful for any
  language `loadngo`'s current fonts don't cover.
- **Migrating any game's existing hardcoded strings onto this.** Genuinely
  large, mechanical, per-game work (again, ~290 sites in `sng-roguelite`
  alone) — a deliberate follow-up once Phase 1 is proven, not bundled
  into landing the engine piece itself.
- **Real translated content.** No non-English locale catalogs exist yet
  for any game — Phase 1 is the mechanism, not the content.

## Sequencing for later phases (not scheduled)

1. Pick one game (likely `sng-roguelite`, already mid-conversation about
   its UI) and migrate a bounded slice of its hardcoded strings onto
   `Localizer`, proving the adoption path end to end before doing the
   rest.
2. Font glyph-coverage: source/bundle one broad-coverage fallback font,
   wire `FontCatalogManifest.fallback_fonts` all the way through to
   actual glyph rasterization, verify a real non-Latin string renders
   correctly on a real device.
3. Locale auto-detection per platform.
4. Full string migration per game, and real translated catalog content
   for at least one additional language, to prove the whole pipeline
   under real (not synthetic-test) conditions.

Also worth remembering while doing any of this: card/label layouts
designed assuming fixed-length English strings (e.g. the reward-card
work happening in the same conversation this roadmap came out of) should
budget real slack for translated text commonly running 30-50% longer.
