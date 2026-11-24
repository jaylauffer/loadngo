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

1. `src/neon.rs` (new): the forward transform, the inverse transform and the multiply-and-add step of the
   matrix-by-vector product on 64-bit ARM vector instructions (NEON), four coefficients at a time. Compiled only for
   `aarch64` targets with NEON, which is every 64-bit ARM target Rust ships. This is the only file with `unsafe`.
2. `src/ntt.rs`: each transform's work on one polynomial is now a function (`ntt_poly`, `inv_ntt_poly`) that calls
   the vector kernel on 64-bit ARM and upstream's loop elsewhere. Upstream's loops are kept unchanged as
   `ntt_poly_scalar` and `inv_ntt_poly_scalar`.
3. `src/helpers.rs`: the inner loop of `mat_vec_mul` is `mul_acc`, with the same split; `gen_zeta_table_mont` is
   visible inside the crate so that `neon.rs` can build its constants from it.
4. `src/lib.rs`: the `mod neon` line.
5. `src/conversion.rs`: one `#[allow(clippy::needless_range_loop)]` on a loop of upstream's, which this
   repository's newer Clippy rejects under the crate's own `deny(clippy::pedantic)`.
6. `Cargo.toml`: rewritten for a crate that is not published; the test-only dependencies are the three the tests
   inside `src/` use.

Nothing else is touched: no algorithm, no encoding, no hashing, no random numbers, no wiping of secrets.

## How the change is held

- The vector kernels give **bit for bit** what the plain loops give (`src/neon.rs`, tests): the same butterflies
  in the same order with the same reductions, on random inputs and on inputs at the edge of the plain code's own
  limits. So the rest of the crate cannot tell which ran.
- Upstream's own tests inside `src/` run against the vector kernels on an ARM machine and the plain ones elsewhere.
- `loadngo-pq-crypto`'s tests on top: key generation against the NIST ACVP vector, OpenSSL both ways (opt-in), and,
  run once on 2026-11-24 with the kernels in place, the cross-check against AWS-LC from commit `53ec22b2` (the
  same keys from 48 seeds; each accepts the other's signatures; both refuse damaged ones).

## Taking a newer upstream version

Copy its `src/` over this one, reapply the six changes above (the diff of this directory against the published
0.5.0, after `cargo fmt`, is exactly them), and rerun everything under "How the change is held", the AWS-LC
cross-check included.
