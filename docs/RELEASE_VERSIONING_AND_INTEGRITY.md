# Release Versioning and Integrity

Purpose: a cross-cutting design note for every `loadngo`-based product
(`sng-roguelite`, `sng-rusty`, `sng-zhoenus`) covering two related, currently
open problems raised 2026-09-02 while hardening `sng-roguelite`'s itch.io CD
pipeline (see [BUILD_RELEASE_PIPELINE.md](../../sng-roguelite/docs/BUILD_RELEASE_PIPELINE.md)):

1. a consistent, in-game way to check a build's version, built on each target
   platform's own trusted/supported mechanism rather than an ad hoc per-game
   convention.
2. a `loadngo`-native post-quantum signature layer for release artifacts, so
   authenticity/integrity checking doesn't depend solely on whatever trust
   root (or lack of one) the distribution channel happens to provide.

**Status: open design question, not implemented.** This note exists so the
discussion and the options considered are tracked somewhere real — not only
in a session's own memory — ahead of a dedicated design conversation. Once a
direction is chosen, it belongs as a `docs/decisions/NNNN-*.md` record in
whichever repo ends up owning the implementation, matching the convention
`sng-roguelite`'s own release doc already points to for exactly this kind of
cross-repo, hard-to-reverse call.

## Problem 1: cross-platform version checking

State today, per platform:

- **Android**: `versionCode`/`versionName` in the generated
  `AndroidManifest.xml` derive automatically from the workspace `Cargo.toml`
  version (`android_packager.sh`), and are enforced at install time by the
  OS's own APK v2/v3 signature verification. But nothing in-game reads that
  back — the value is baked in at build time and never re-queried through
  `PackageManager` at runtime, so a game can't currently confirm what the
  *installed* package actually reports versus what it was compiled with.
- **iOS**: `CFBundleShortVersionString`/`CFBundleVersion` in `Info.plist`
  (`ios_device_build.sh`), enforced by the OS's code-signing/provisioning
  checks at install/launch. No App Store presence exists yet for any of the
  three games, so there's no receipt-based "is a newer version available"
  check either — device builds are entirely manual reinstalls today.
- **Linux (itch.io)**: no OS-level package manager or store in the loop at
  all. The closest thing to a "platform" here is itch.io's own app plus the
  `butler` channel model — `--userversion` now correctly reflects the git
  tag (see the filename-disambiguation fix landed 2026-09-02 in
  `deploy-itch.yml`), but nothing reads that back in-game either.
- **Desktop macOS/Windows**: not yet shipped as installable bundles at all
  (see [DESKTOP_PLATFORM_ROADMAP.md](DESKTOP_PLATFORM_ROADMAP.md)) — no
  version-check story exists yet because there's no distribution artifact to
  check against.

No platform currently gives a player, or the game itself, a uniform in-game
answer to "what version am I running, and is there a newer one?" What exists
is fragmented, and only Android/iOS get any integrity guarantee at all (OS
code-signing) — itch.io's Linux path currently has none beyond trusting the
download URL and itch.io's own TLS.

**Proposed direction (not committed):** a small `loadngo_host_desktop`
module (e.g. `version::current_version()` /
`version::installed_via() -> DistributionChannel`) exposing one
platform-agnostic surface to games, backed underneath by whichever
native/trusted mechanism the target platform actually has:

- Android: read back via `PackageManager.getPackageInfo().versionName` at
  runtime (JNI, same pattern `android.rs` already uses for other platform
  queries), not just the build-time constant.
- iOS: read `CFBundleShortVersionString` via `NSBundle.main`, the same
  source Apple's own review process and TestFlight rely on.
- Linux/itch.io: **no native authority exists**, flagged explicitly rather
  than glossed over — this is the one leg where "native, trusted platform
  mechanism" doesn't apply, and a project-level substitute is unavoidable.
  This is exactly where Problem 2 below becomes load-bearing rather than
  redundant.
- Windows: revisit once an installable bundle format (MSI/MSIX) is chosen;
  either carries its own native versioning/signing story to key off of.

## Problem 2: a loadngo PQ secure signature solution

**Build on existing infrastructure — don't invent a new one.** `loadngo`
already has real, working post-quantum signing tooling:

- [`loadngo-pq-auth`](../pq-auth) (`loadngo/pq-auth`, documented in
  [PQ_AUTHENTICATOR.md](PQ_AUTHENTICATOR.md)): a signed challenge-token
  issue/verify flow, with `dilithium2`/`falcon512` key generation already
  wired up.
- `qcoin-crypto` (sibling repo, `../qcoin/qcoin-crypto`, consumed as a
  workspace path dependency): the actual PQ primitives — `dilithium2`,
  `falcon512` — behind an explicit, algorithm-agile `SignatureSchemeId`
  scheme (`Dilithium2`/`Falcon512`/`Unknown(u16)`), designed from the start
  to support adding or rotating schemes later.
- CAS root-manifest signing (`data/src/bin/pudding_cas_ingest.rs`,
  documented in [PUDDING_CAS_PQ_MODEL.md](PUDDING_CAS_PQ_MODEL.md)): an
  existing, working precedent for PQ-signing a manifest that asserts "this
  content is authoritative," using the same `qcoin-crypto` key material.

None of these currently cover player-facing release artifacts (APKs, IPAs,
Linux binaries, itch.io uploads) — that's the actual gap. The proposal is to
extend the same signed-manifest pattern CAS already uses for workspace
lineage to release artifacts instead, so a game (or a future updater) can
verify "this build was actually produced by this project" independently of,
and in addition to, whatever the distribution channel itself enforces.

**Why this matters more on some platforms than others:** Android and iOS
already have a strong native trust root (OS-enforced code signing at
install) — a `loadngo` PQ signature there is a second, quantum-resistant
belt-and-suspenders layer, not filling a gap. itch.io/Linux has none today.
A binary downloaded from itch.io currently carries no cryptographic tie back
to this project beyond the download URL and itch.io's TLS — a PQ-signed
release manifest, verified against a known public key, would give the Linux
path an actual authenticity guarantee for the first time, not just
Android/iOS parity for its own sake.

**Proposed shape (for discussion, not committed):**

- A signed `release-manifest.ron` (or similar) per tag, produced by
  `release.yml` on `dolores` alongside the existing build artifacts:
  version, git commit, and per-artifact `(path, arch, blake3 hash)`, signed
  with the project's `qcoin-crypto` key — `dilithium2`, matching
  `loadngo-pq-auth`'s existing default.
- A small verification routine — a new `loadngo` crate, or an extension of
  `loadngo-pq-auth` — that a game or future updater calls: fetch the
  manifest and signature from a known location, verify against a pinned
  public key, then verify each artifact's hash before trusting it. Same
  issue/verify shape `loadngo-pq-auth` already has for auth challenges,
  applied to release artifacts instead.
- Key custody is its own open decision — same signer as CAS/`pq-auth`, or a
  dedicated release-signing key with its own rotation policy — flagged here,
  not resolved.

## Future direction: an in-engine update-channel mechanism, backed by CAS

Raised 2026-09-03, after `sng-roguelite` v0.5.2's Linux build was manually
play-tested on `dolores` and pushed to itch.io by hand. Broader than
Problem 1 above ("what version am I running, and is there a newer one") —
this is the next step past just *checking*: `loadngo` itself having a
release-channel concept a game can subscribe to, so a running game (or a
launcher) can discover, fetch, and apply an update rather than a human
rebuilding and manually re-uploading/reinstalling every time, the way
every one of the three games' pipelines works today (see
`sng-roguelite/docs/BUILD_RELEASE_PIPELINE.md`).

**Why CAS specifically, not a bespoke download mechanism:** `loadngo`
already has a working content-addressed, PQ-signed manifest precedent —
`PUDDING_CAS_PQ_MODEL.md`'s `pudding_cas_ingest` tooling, which asserts
"this content is authoritative" via a signed root manifest over
`blake3`-hashed content. Problem 2 above already proposes reusing that
same signed-manifest shape for release-artifact authenticity; an
update-channel mechanism would go a step further and reuse CAS as the
actual **transport/storage** for update content too — content-addressed
storage gives delta-friendly, dedup-friendly, integrity-verified-by-
construction distribution essentially for free, rather than inventing a
second content-delivery scheme alongside the one CAS already provides for
workspace lineage.

**Status: an idea to preserve, not a design.** No shape has been proposed
yet for what a "channel" is (stable/beta? per-platform? per-game?), how a
running game would poll or be notified, how a partial/delta update would
apply itself to an already-installed Android/iOS bundle (which have their
own OS-enforced install mechanisms Problem 1 already has to route around
on those platforms), or how this interacts with app-store-style platforms
that forbid apps updating themselves outside the store's own mechanism
(a real constraint once either mobile game ever ships to an actual store,
not just device builds). Recording this now so the eventual design
conversation for Problem 1/2 above also considers *delivery*, not only
*checking* and *signing* — don't let this scope quietly get invented
piecemeal inside whichever problem gets tackled first.

## Next step

A dedicated design conversation, per the user's own framing when this was
raised, before any code changes. This note's job is to make sure that
conversation starts from the real, existing PQ/CAS infrastructure above
instead of re-deriving it, and that the two problems (version-check UX vs.
artifact authenticity) stay distinguished even though they're related.

## Related notes

- [BUILD_RELEASE_PIPELINE.md](../../sng-roguelite/docs/BUILD_RELEASE_PIPELINE.md) —
  where this was raised; itch.io/CI mechanics for `sng-roguelite` today.
- [PQ_AUTHENTICATOR.md](PQ_AUTHENTICATOR.md) — the existing PQ signed-token
  tooling this note proposes extending.
- [PUDDING_CAS_PQ_MODEL.md](PUDDING_CAS_PQ_MODEL.md) — the existing
  signed-manifest precedent this note draws from.
- [DESKTOP_PLATFORM_ROADMAP.md](DESKTOP_PLATFORM_ROADMAP.md) — desktop
  macOS/Windows distribution status referenced in Problem 1.
