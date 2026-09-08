# Task Fabric Trust Model

Status: **open gap**, recorded 2026-09-08 while committing
[`deploy/`](../deploy/) — the systemd deployment for `task-node`, which had
until then existed only as untracked files on one Pi.

The task fabric authenticates nothing. This document says exactly what
that means, what currently bounds the exposure, and what the fix would
look like. It is a design record, not a vulnerability report: everything
here is readable from `network/src/bin/task-node.rs`, which is already
public.

## What is actually checked

`task-node`'s accept path (`handle_accept`) rejects an assignment only
when:

- `offered.offer.offer_id != accept.offer_id` — this is not an offer I made
- `accept.worker_node_id != self.args.worker_node_id` — this is not for me

Both are *correlation* checks. Neither establishes who the submitter is.
There is no signature, no token, no allowlist, no shared secret, and no
challenge — searched for across the whole `network` crate.

So the full sequence an arbitrary host on the segment can drive is:

1. send a `TaskRequest` naming a capability the node advertises
2. receive the node's unicast `TaskOffer`, which carries a real `offer_id`
3. send a `TaskAccept` echoing that `offer_id` and the node's
   `worker_node_id`
4. the node runs `LOADNGO_TASK_NODE_EXEC_COMMAND`

## What bounds the exposure today

Three things, none of which is authentication:

1. **Network scope.** Discovery uses link-local IPv6 (`ff02::/16`) and
   administratively-scoped IPv4 (`239.0.0.0/8`) groups. Neither routes off
   the local segment, so the attacker population is "hosts on your LAN".
2. **Capability matching.** A `TaskRequest` naming a capability the node
   does not advertise is ignored rather than offered on. This narrows what
   can be asked for; it does not narrow *who* can ask.
3. **The exec command.** This is the real boundary. The shipped
   `deploy/task-node-exec.sh` is written defensively on purpose — every
   expansion quoted, the artifact hint through a two-prefix `case`
   allowlist, and `assignment_id` typed `u64` on the wire so it cannot
   traverse out of the receipts directory. Task-supplied strings reach
   only `printf '%s'` or a quoted argv slot.

   **That safety is a property of the script, not of the fabric.** Any
   replacement that interpolates a `LOADNGO_TASK_*` value into a shell, a
   path, or a URL turns this into remote code execution for anyone on the
   segment.

With the shipped script the realistic worst case is: an attacker causes
receipt files to be written, runs `qcoin-node node-info`/`tip` against a
target of their choosing, and learns whether a supplied path is a git repo
(its branch, `HEAD`, and `status --short` land in the local receipt).
Only `wrote <path>` is returned over the network.

## The building blocks already exist and are unwired

`loadngo-pq-auth` is a workspace member that provides essentially the
exact primitives this needs:

| type | relevance |
|---|---|
| `SignedAuthToken` | `issuer`, `audience`, `subject`, `scopes`, `nonce_hex`, `issued_at_unix_s`, `expires_at_unix_s`, post-quantum `signature_scheme` + `public_key_hex` + `signature_hex` |
| `UnsignedAuthToken` | the pre-signature shape, so a challenge can be bound before signing |
| `VerifyPolicy` | `expected_audience`, `expected_subject`, `required_scopes`, `expected_challenge_sha256`, `trusted_public_key` |
| `random_nonce_hex`, `sha256_bytes` | replay protection and challenge binding |

**No crate depends on it.** `grep` across every `Cargo.toml` in the
workspace finds `loadngo-pq-auth` named only by the workspace root's member
list and its own manifest. It is fully written and entirely unused.

That is the substance of this gap: the fabric is unauthenticated not
because the primitives are missing, but because they were never connected.

## What a fix would look like

Sketch, not a committed design:

1. `TaskAccept` (and probably `TaskRequest`) carries a `SignedAuthToken`
   whose `challenge_sha256` binds the `offer_id`/`request_id` it answers,
   so a token cannot be replayed onto a different assignment.
2. `audience` is the worker's `worker_node_id`; `scopes` name the
   capability being requested.
3. The worker holds a small set of trusted submitter public keys —
   configured beside `/etc/loadngo/task-node.env`, not in the repo — and
   verifies with a `VerifyPolicy` carrying `expected_audience`,
   `required_scopes` and `expected_challenge_sha256`.
4. `nonce_hex` plus `expires_at_unix_s` give replay protection with a
   bounded window.

Open questions worth settling before building it:

- Does the submitter also need to authenticate the *worker*, or is a
  worker impersonating another worker an acceptable risk on a trusted
  segment? (Mutual auth is more work and may not be worth it here.)
- How are trusted keys distributed to workers? Manual placement is fine
  for a three-machine lab and does not scale past it.
- Does `qcoin` reward settlement need the same identity, or a different
  one? `TASK_REWARD_FLOW.md` ties reward to acknowledged completion, and
  an unauthenticated ack is a reward-theft vector independent of the
  execution concern above.

## Until then

Treat `task-node` as a **trusted-segment-only** service. Do not run it on
a network you share with anything you do not control, and do not point
`LOADNGO_TASK_NODE_EXEC_COMMAND` at anything less constrained than the
shipped script without re-reading the boundary argument above.

See [`../deploy/README.md`](../deploy/README.md) for the deployment
procedure and the operator-facing version of this warning.
