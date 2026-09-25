# How games depend on loadngo

Status: 2026-09-25. Applies to sng-roguelite, sng-mahjong, sng-zhoenus and
sng-bass-blaster.

## The model

Games take loadngo as a **git dependency**, and `Cargo.lock` records the
exact commit:

```toml
ui-core = { git = "https://github.com/jaylauffer/loadngo", branch = "dev" }
```

sng-roguelite takes `eab-core` from entitlement-achievement-blockchain the
same way.

- CI and every plain local `cargo` command build exactly the loadngo commit
  in the game's `Cargo.lock`. CI passes `--locked`, so a lock that disagrees
  with `Cargo.toml` fails loudly.
- A release therefore records its engine. Nothing depends on what a runner
  happens to have checked out next to the game.
- The loadngo repository is public, so Cargo fetches it without credentials.

Using `branch = "dev"` rather than `rev = "..."` keeps every game crate and
every loadngo crate on one git source. Pinning is done by `Cargo.lock`.

### What this replaced

Until 2026-09-25, games used `path = "../loadngo/..."`. CI built against
whatever `~/ci/pudding/loadngo` a runner had checked out. No workflow ever
updated it, so builds silently used stale engines unless someone
fast-forwarded that checkout by hand on every runner host.

## Everyday workflow

| Task | Command (from the game repo) |
|---|---|
| Build or test what CI builds | `cargo build`, `cargo test` |
| Try uncommitted loadngo changes in the game | `../loadngo/scripts/with-local-loadngo.sh run --release` (any cargo arguments) |
| Adopt pushed loadngo changes | `cargo update -p ui-core`, then commit `Cargo.lock` |

`with-local-loadngo.sh` passes Cargo a `[patch]` pointing every loadngo
package in the lock file at the local checkout. Cargo rewrites `Cargo.lock`
while patched, so the script restores the committed lock afterwards. The
patch is opt-in on purpose. A permanent one (for example in
`~/pudding/.cargo/config.toml`) would rewrite every game's lock file on
every local build. Those rewrites would be committed by accident and fail
CI.

The Android and iOS packaging scripts (`android_packager.sh`,
`ios_device_build.sh`, `ios_simulator_build.sh`) bundle loadngo's
`assets/` (the fonts the Android and iOS hosts load). They find the pinned
checkout under `~/.cargo/git` through `cargo metadata`, and fail if it has
no `assets/fonts/manifest.ron`. Set `LOADNGO_DIR=../loadngo` to package
local assets instead.

For a change that spans loadngo and a game:

1. Edit both, and test the game through `with-local-loadngo.sh`.
2. Commit and push loadngo.
3. In the game, run `cargo update -p ui-core`, then build and test plainly.
4. Commit the game change together with its `Cargo.lock`.

## Not yet covered: loadngo, qcoin and EAB

These three still build with sibling path dependencies, because they form a
cycle. loadngo's workspace uses `qcoin-types`, and `qcoin-types` uses
`loadngo-pq-crypto` from loadngo. Fetched by git, each side would get its
own copy of the other, and the types would no longer match. Breaking the
cycle comes first, for example by moving what loadngo needs from
`qcoin-types` into loadngo. Their CI keeps its current sibling setup until
then.

## Next: crates.io

Publishing is the destination. The 14 loadngo crate names the games use were
all free on crates.io on 2026-09-25. Moving a game from this model is one
line per dependency: `git = ..., branch = "dev"` becomes `version = "0.1"`.
Local co-development then uses `[patch.crates-io]` in place of the git
patch. Earlier findings (mandatory renames of `data` and `network`, required
manifest fields) are in `~/pudding/loadngo-crates-publishing-notes.md`.
