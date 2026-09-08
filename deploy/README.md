# Deploying `task-node`

`task-node` is the standing worker in loadngo's task fabric: it listens on
the discovery groups, offers on `TaskRequest`s matching its advertised
capabilities, and on an accepted `TaskAccept` runs a configured command
and returns the result. The protocol itself is described in
[`../docs/TASK_OFFER_PROTOCOL.md`](../docs/TASK_OFFER_PROTOCOL.md) and
[`../docs/WORKER_FIRST_TASK_MODEL.md`](../docs/WORKER_FIRST_TASK_MODEL.md);
this directory is how you actually run one.

**Read [Trust model](#trust-model) before deploying.** The fabric does not
authenticate submitters, and the exec command is the security boundary.

## Files

| file | purpose |
|---|---|
| `loadngo-task-node.service` | systemd unit |
| `task-node-launch.sh` | turns the env vars into `task-node`'s CLI flags |
| `task-node-exec.sh` | what runs on an accepted assignment — writes a receipt and handles the `qcoin://` and `repo-tip://` artifact hints |
| `task-node.env.example` | worked example of the full configuration surface |

## Install

The unit invokes the scripts from `/usr/local/bin` under different names
than they carry here, so the copy step is not optional:

```bash
# 1. build the worker
cargo build --release -p network --bin task-node

# 2. install the scripts under the names the unit and env file expect
sudo install -m 0755 deploy/task-node-launch.sh /usr/local/bin/loadngo-task-node-launch.sh
sudo install -m 0755 deploy/task-node-exec.sh   /usr/local/bin/loadngo-task-node-exec.sh

# 3. configuration, outside the repo so real values are never committed
sudo install -d -m 0755 /etc/loadngo
sudo install -m 0640 deploy/task-node.env.example /etc/loadngo/task-node.env
sudoedit /etc/loadngo/task-node.env      # at minimum: node id, endpoints, groups

# 4. receipts directory, owned by the service user
sudo install -d -o jay -g jay -m 0755 /var/lib/loadngo-task-node/receipts

# 5. the unit
sudo install -m 0644 deploy/loadngo-task-node.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now loadngo-task-node.service
```

Verify:

```bash
systemctl status loadngo-task-node.service
journalctl -u loadngo-task-node.service -f
ls /var/lib/loadngo-task-node/receipts/
```

The unit runs as `jay` with `NoNewPrivileges=true` and `PrivateTmp=true`.
Change `User=`/`Group=` and the receipts directory owner together if you
deploy as someone else.

## Trust model

**`task-node` does not authenticate submitters.** Its accept path checks
only that the assignment correlates to an offer it made (`offer_id`) and
is addressed to it (`worker_node_id`). Both are self-consistency checks,
not identity: any host that can reach the discovery groups and this node's
port can send a `TaskRequest`, collect the offer, and send back a matching
`TaskAccept` to have the exec command run.

Consequences to design around:

- **Run this only on a network segment you control.** The discovery groups
  are link-local (`ff02::/16`) and administratively scoped (`239.0.0.0/8`),
  so exposure is bounded to that segment — but everything on that segment
  is effectively trusted.
- **The exec command is the security boundary, not the protocol.** The
  shipped `task-node-exec.sh` is deliberately narrow: every expansion is
  quoted, the artifact hint passes through a `case` allowlist of two
  prefixes, and `assignment_id` is a `u64` on the wire so it cannot
  traverse out of the receipts directory. Task-supplied strings only ever
  reach `printf '%s'` or a quoted argv slot. **A replacement command
  inherits none of that** — anything that interpolates
  `LOADNGO_TASK_*` values into a shell, a path, or a URL is remote code
  execution for anyone on the segment.
- **What the shipped script exposes** if driven by an untrusted submitter:
  it will write a receipt, run `qcoin-node node-info`/`tip` against a
  supplied target, and read `branch`/`HEAD`/`status --short` from a
  supplied path if that path is a git repo. Only `wrote <path>` is
  returned to the submitter; the receipt content stays on local disk.

This gap is tracked in
[`../docs/TASK_FABRIC_TRUST_MODEL.md`](../docs/TASK_FABRIC_TRUST_MODEL.md),
which also covers why `loadngo-pq-auth` exists in this workspace but is
not wired into the task path.
