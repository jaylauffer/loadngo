# loadngo's copy of `fips204`

- **What:** the `fips204` crate, version 0.5.0, by Eric Schorn (IntegrityChain): a pure-Rust implementation of
  FIPS 204, the Module-Lattice-Based Digital Signature Standard (ML-DSA). <https://github.com/integritychain/fips204>
- **Licence:** MIT OR Apache-2.0, as upstream (`LICENSE-MIT`, `LICENSE-APACHE`). loadngo's changes to it are under
  the same terms.
- **Taken from:** the crate as published on crates.io (`src/`, `README.md`, the two licence files). Not copied:
  `benches/`, `tests/` (its integration tests need vector files the published crate does not carry), `deny.toml`,
  `SECURITY.md`, `CHANGELOG.md`, and its `rustfmt.toml` (nightly-only options; the copy is formatted with this
  repository's `cargo fmt`, which moved line breaks and nothing else).
- **Why a copy:** Jay, 2026-11-24, after the pure-Rust crate measured 3 to 4 times slower than AWS-LC's assembly:
  "use the intrinsics our goal is least energy waste." The arithmetic's inner loops had to change, and they are
  private to the crate.

## What loadngo changed

Vector arithmetic (commit `698ecd05`):

1. `src/neon.rs` (new): the forward transform, the inverse transform and the multiply-and-add step of the
   matrix-by-vector product on 64-bit ARM vector instructions (NEON), four coefficients at a time. Compiled only for
   `aarch64` targets with NEON, which is every 64-bit ARM target Rust ships.
2. `src/ntt.rs`: each transform's work on one polynomial is a function (`ntt_poly`, `inv_ntt_poly`) that calls the
   vector kernel on 64-bit ARM and upstream's loop elsewhere. Upstream's loops are kept unchanged as
   `ntt_poly_scalar` and `inv_ntt_poly_scalar`.
3. `src/helpers.rs`: the inner loop of `mat_vec_mul` is `mul_acc`, with the same split; `gen_zeta_table_mont` is
   visible inside the crate so that `neon.rs` can build its constants from it.

Erasing and copying (Jay, 2026-11-24: "do the block erasing and working in place"):

4. `src/wipe.rs` (new) and `src/types.rs`: a polynomial (`R`, `T`) still erases itself when it goes out of use, as
   upstream's `#[derive(Zeroize, ZeroizeOnDrop)]` made it, but with one volatile write for its 1,024 bytes and
   one compiler fence, where `zeroize` wrote and fenced each of the 256 coefficients in turn. What is erased, and
   when, is the same.
5. `src/helpers.rs` (`to_mont_in_place`), `src/ntt.rs` (`mat_vec_mul_inv_ntt`, `mul_inv_ntt`) and `src/ml_dsa.rs`
   (`key_gen_internal`, `sign_internal`, `verify_internal`): products are formed in the memory their result lives
   in and transformed back there, and the signing loop's `z` and `h` are filled where they stand on each attempt.
   Upstream built each intermediate vector anew, copied it into the next and erased it. The arithmetic, its order
   and every value are upstream's; fewer copies of secret-dependent values exist at all. Counted over 2,000
   operations each: a signature creates and erases 284 polynomials where it was 430, a verification 65 (82), a
   key generation 122 (145).

Housekeeping:

6. `src/lib.rs`: the `mod neon` and `mod wipe` lines. `src/conversion.rs`: one
   `#[allow(clippy::needless_range_loop)]` on a loop of upstream's, which this repository's newer Clippy rejects
   under the crate's own `deny(clippy::pedantic)`.
7. `Cargo.toml`: rewritten for a crate that is not published; the test-only dependencies are the three the tests
   use. `tests/known_answers.rs` (new): see below.

`unsafe` is in `src/neon.rs` and `src/wipe.rs` only, each of which says why. Nothing else is touched: no
algorithm, no encoding, no hashing, no random numbers.

## How the change is held

- **Known answers** (`tests/known_answers.rs`): for each of the three parameter sets, sixteen key pairs from fixed
  seeds and a deterministic signature by each, hashed. The digests were taken before the work on erasing and
  copying, from the crate with upstream's arithmetic. They are the same after it, with the vector kernels (ARM)
  and with upstream's loops (an x86-64 build run under Rosetta).
- The vector kernels give **bit for bit** what the plain loops give (`src/neon.rs`, tests): the same butterflies
  in the same order with the same reductions, on random inputs and on inputs at the edge of the plain code's own
  limits.
- A dropped polynomial leaves zeros in its memory, in an optimised build too (`src/wipe.rs`, tests).
- Upstream's own tests inside `src/` run against all of it.
- `loadngo-pq-crypto`'s tests on top: key generation against the NIST ACVP vector, OpenSSL both ways (opt-in), and,
  run by hand on 2026-11-24 after each of the two rounds, the cross-check against AWS-LC from commit `53ec22b2`
  (the same keys from 48 seeds; each accepts the other's signatures; both refuse damaged ones).

## Stack

The crate keeps every polynomial on the stack and uses no heap. How much stack an operation needs depends heavily
on whether the crate is built optimised. Measured on the Mac mini (64-bit ARM) by running each operation on a
thread with a given stack and searching for the smallest that survives; kilobytes:

| | key generation | sign | verify | private key from bytes | upstream's key round-trip example |
| --- | --- | --- | --- | --- | --- |
| ML-DSA-65, unoptimised | 499 | 323 | 291 | 323 | 690 |
| ML-DSA-65, optimised | 147 | 131 | 99 | 115 | 291 |
| ML-DSA-87, unoptimised | 738 | 515 | 467 | 451 | 1,010 |
| ML-DSA-87, optimised | 227 | 195 | 163 | 147 | 419 |

The crate as published needs the same or a little more (its example: 1,026 for ML-DSA-87, unoptimised). A Windows
main thread has 1,024 kB, so that example (`SerDes::try_from_bytes`, a documentation test) overflowed its stack in
loadngo's Windows CI the first time the copy's tests ran there (run 45334063481, 2026-11-24); Linux and macOS give
a main thread 8 MB: both of CI's Linux systems passed, and so did the same tests on the Mac mini (that run's own
macOS job was cancelled by the next push before its tests finished). The workspace's `Cargo.toml` therefore builds this crate at `opt-level = 3` in
debug and test builds as well (`[profile.dev.package.fips204]`): with that, a debug build of the example needs
483 kB, and the copy's own tests run in 8 s where they took 41 s. Debug assertions and overflow checks stay on. A project that takes this crate from outside loadngo's
workspace and builds it unoptimised gets the first and third rows: give its signing thread 1 MB or more.

## Taking a newer upstream version

Copy its `src/` over this one, reapply the seven changes above (the diff of this directory against the published
0.5.0, after `cargo fmt`, is exactly them), and rerun everything under "How the change is held", the AWS-LC
cross-check included.
