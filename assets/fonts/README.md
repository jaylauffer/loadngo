# Font Assets

Shared renderer fonts for `loadngo` should live here:

- `loadngo/assets/fonts/`

Why here:

- fonts are a renderer/runtime concern, not a single backend concern
- the same families should be available to macOS, iOS, Android, Linux, and Windows backends
- keeping them above backend crates avoids duplicated copies and backend-specific ownership

Recommended layout:

- `loadngo/assets/fonts/<family-name>/`
- keep original upstream filenames when practical
- keep license/readme files next to the font files

Active manifest:

- `loadngo/assets/fonts/manifest.ron`

Current manifest schema:

- `novel_font.asset_rel_paths`: shared asset fonts bundled with `loadngo`
- `novel_font.platform_paths`: direct font file candidates per platform for hosts that can load by path
- `novel_font.platform_families`: preferred built-in families per platform for native backends
- `fallback_fonts`: ordered fallback families for multilingual coverage and future shaping/text engines

Recommended metadata to track for each family:

- family name
- supported scripts/languages
- weight/style variants
- fallback priority
- license/source

Current resolution order:

- bundled `asset_rel_paths`
- platform-specific `platform_paths`

`platform_families` are metadata today. They become active when `loadngo` native backends can request system fonts by family name instead of by file path.
