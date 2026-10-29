# Task 9 result: the espeak-ng command-line program is Rust

commit: 58b6319075dc5bf620367405024a159bd44b7c02

Third run. The first (assignment 5589755449493568496) was rejected by its own check,
which could not see the passing parity summary (corrected in loadngo bf07abae). The
second (assignment 5387736940578917631) was handed a result without this commit line:
the worker had already opened the result file when I rewrote it. The work is unchanged.

espeak-ng-rs `dev`, three commits:
- `03044f0a` Read C chars through byte pointers; allow an Android Clippy false positive
  (24 same-type casts that Clippy 1.99 rejects on Linux aarch64 and Android).
- `a18e2161` Own the espeak-ng command-line program in Rust: Codex's 10-14 stage
  (src/rust_cli.c, rust/cli_native.rs, rust/cli_io.rs, engine_io::OutputFile,
  tests/rust_cli_parity.py), finished: --stdin wins over a regular -f file as in the
  original, and input is zero-padded to a whole wchar_t (the -b 3 parity case read
  past the C oracle's buffer and varied run to run; it now feeds real wchar_t text).
- `58b63190` Round CLI input padding to whole wide units (Windows, 2-byte wchar_t;
  CI 40843929869 caught it).

test: rust_cli_parity 175/175 (static synchronous) and 178/178 (shared, async, MBROLA),
five consecutive runs; cargo test all-features 586, no-default-features 482; CMake
CTests 129/129 and 133/133.
ci: Rust port run 40844963646 at 58b63190, all 7 jobs green.
inventory: C logic in the sync library/CLI configuration 1,993 -> 1,444 lines.
