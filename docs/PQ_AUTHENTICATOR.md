# PQ Authenticator

Purpose: establish a repo-owned post-quantum authenticator for `loadngo`
instead of leaving signed-auth experiments scattered across other projects.

## What It Is

`loadngo-pq-auth` is a small signed challenge-token layer built on top of the
local `qcoin-crypto` PQ signature registry.

It is not a TOTP clone.

The model is:

- a verifier creates or chooses a challenge payload
- an operator or node signs a token that binds:
  - issuer
  - audience
  - optional subject
  - scope list
  - challenge digest
  - issuance/expiry window
  - nonce
- the verifier checks signature, audience, scope, time window, challenge hash,
  and optionally a trusted signer key

That fits the field-network direction better than one-time-password semantics.

## Why This Shape

The field backlog already points toward:

- signed manifest envelopes
- trust roots
- signer identity
- explicit verification results
- transport-independent authority

Those needs are closer to signed capability or challenge tokens than to OTP
codes. `loadngo-pq-auth` is therefore a foundation for:

- operator authentication
- node-to-node challenge response
- signed promotion authority
- local-authority workflows on unreliable or disconnected links

## Current Scope

The crate currently provides:

- key generation for `dilithium2` and `falcon512`
- signed auth-token issue flow
- signed auth-token verification flow
- challenge binding via `sha256`
- audience/subject/scope/time enforcement
- optional trusted public-key match during verification

Current command surface:

```bash
cargo run -p loadngo-pq-auth --bin loadngo_pq_auth -- keygen \
  --scheme dilithium2 \
  --public-key build/pq-auth/public.hex \
  --private-key build/pq-auth/private.hex

cargo run -p loadngo-pq-auth --bin loadngo_pq_auth -- issue \
  --challenge challenge.bin \
  --issuer loadngo-operator \
  --audience field-node-a \
  --subject jay \
  --scope import \
  --scope promote \
  --public-key build/pq-auth/public.hex \
  --private-key build/pq-auth/private.hex \
  --out build/pq-auth/token.ron

cargo run -p loadngo-pq-auth --bin loadngo_pq_auth -- verify \
  --token build/pq-auth/token.ron \
  --challenge challenge.bin \
  --audience field-node-a \
  --subject jay \
  --require-scope import \
  --trusted-public-key build/pq-auth/public.hex \
  --now 1711929600
```

Add `--quiet` to any command when the caller needs a single-line receipt for
logs, deployment scripts, or agent transcripts.

## Token Model

The signed token envelope currently binds:

- `issuer`
- `audience`
- `subject`
- `scopes`
- `challenge_sha256`
- `nonce_hex`
- `issued_at_unix_s`
- `expires_at_unix_s`
- `signature_scheme`
- `public_key_hex`
- `signature_hex`

The challenge digest and nonce exist for replay resistance and verifier
ownership of the authentication moment.

## Relation To Other Loadngo Work

This is intentionally parallel to, but distinct from, the CAS signing model.

- CAS signing proves authenticity of a promoted bundle statement.
- PQ auth tokens prove that a signer authorized a bounded action or session
  against a specific verifier challenge.

Both should eventually feed a common trust-root story, but they are not the
same artifact.

## Quiet Operational Receipts

For noisy bring-up work, prefer a small challenge payload plus quiet receipts
over raw command transcripts. The challenge should state:

- target node or audience
- intended action
- files or manifest roots being moved
- validation command
- expected scope, such as `netbsd-deploy`

Then issue and verify a token with `--quiet`, archive the challenge and token,
and only surface the concise `issue ok ...` / `verify ok ...` lines unless a
failure needs full logs.

## Near-Term Next Steps

- move shared PQ encoding helpers into a common loadngo crypto module if CAS and
  auth keep overlapping
- add trust-root store and signer labels instead of only embedded public keys
- define explicit verification result enums for quarantine/reject/accept flows
- bind tokens to removable-media import/export actions where appropriate
- add UI/operator surfaces once the trust model settles
