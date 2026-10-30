# Result: espeak-ng-rs, the engine's mutable state moves from C to Rust

commit: 5cc8ce385967bfc0d9a7be9ebc2b921ff3e4af4e

## What changed (espeak-ng-rs dev, 5cc8ce38)

- `rust/engine_state_compat.rs` (new, native host only) defines the 66
  globals that translate.c (43), voices.c (10), speech.c (5), soundicon.c (3),
  readclause.c (3) and dictionary.c (2) defined in the Rust-core build, under
  the same C names, sizes and initial values (`tone_points`, `voice =
  &voicedata`); arrays use `CArray` for the 16-byte alignment C gave them.
- The six C files keep their definitions for the C oracle only
  (`#ifndef USE_RUST_CORE`); the Rust build declares `extern` the few the C
  bridges still use. translate.c's `TRANSLATE_STATE` macro is gone.
- A shared library exports exactly the C engine's API plus
  `espeak_rs_cli_main`: `cmake/libespeak-ng.map` (ELF version script) and
  `cmake/libespeak-ng.exports` (Apple), next to the existing Windows
  `rust-exports.def`. Before, every `#[no_mangle]` Rust symbol was exported
  from the .so/.dylib; with the state in Rust that would have included
  `voice`, `translator` and the other C names.
- `.github/workflows/rust.yml`: the Linux shared lane checks
  `nm -D --defined-only` of libespeak-ng.so against that list.
- `docs/REMAINING_PORT.md` records both.

## Evidence (this Mac)

- `nm -m` over the library's and CLI's C objects: 66 writable data symbols
  at 72a8c53b in both configurations; 0 after, in both.
- Measured configuration: 129/129 CTests; async/MBROLA configuration:
  133/133; shared async/MBROLA build: 133/133; C oracle: 19/19.
- Shared dylib exports 73 symbols: the 72 a -DUSE_RUST_CORE=OFF shared build's
  own objects export, plus `espeak_rs_cli_main`. The oracle's dylib also
  exports bundled ucd-tools and speechPlayer code (no installed header); the
  Rust library does not.
- Gates: `cargo fmt --check`; workspace clippy `--all-targets --all-features
  -D warnings`; native-host clippy for the host and for i686/armv5te Linux,
  i686/x86_64 Windows and aarch64 Android.
- CI at 5cc8ce38, all successful: Rust port 41002781460 (7/7; the Linux
  shared lane's export check: libespeak-ng.so.1.53.0 exports 73 symbols),
  CI 41002808976 (57/57), Windows 41002811873 (8/8), windows-msbuild
  41002814815 (2/2), Android 41002817446 (3/3), Dist 41002820516 (3/3),
  C oracle 41002781402 (2/2).

## Check revision

The first check allowed only espeak_*, espeak_ng_*, text decoder names and
`espeak_rs_cli_main` as exports. The C engine also exports `GetFileLength`,
`strncpy0` and `utf8_in` (the reference CLI the tests build calls them),
`LoadSpectSeq`, `SpectSeqCreate`, `SpectSeqDestroy` and `param_defaults`;
my first list broke the reference CLI's link in the shared build. The
revised check (loadngo 4ca9ca80, pushed before this hand-over) compares
with the C engine's own exports.

Not covered: Windows' `rust-exports.def` does not list `param_defaults`,
which the C engine exports (a gap since the Rust core took it over, not
changed here).
