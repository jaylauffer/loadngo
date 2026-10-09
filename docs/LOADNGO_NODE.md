# The loadngo node

Status: plan, 2026-10-10 (Jay: "perhaps we're looking at the task node the wrong way,
maybe we're really building the loadngo node"). Nothing here is built yet beyond the
pieces it names.

One resident process per machine that holds the machine's capabilities and offers them
on the network. Taking Task work is one of its roles, not the whole of it.

## Why

Today each capability lives in whichever program loaded it:

- `task-node` (`network/src/bin/task-node.rs`) listens for `TaskRequest`s, offers on its
  capability tags and, once accepted, runs one shell command
  (`LOADNGO_TASK_NODE_EXEC_COMMAND`), one assignment at a time. It holds nothing
  resident.
- Every chat loads its own model: `gpt_oss_generate` (12 GB of weights), kimi's `k3` with
  Kimi Linear (25 GB of experts, ready about 36 s after launch) or Gemma 4 (18 GB,
  6.5 s). An evaluation run loads another copy.
- `zhoenus_head_model` supervises Homebrew's `llama-server`, which Jay does not trust;
  only the model is checked (`ZHOENUS_HEAD_MODEL_RUNNER.md`).
- A decision model such as Strands Decider (Qwen3.5-2B and a pointer head, about 4 GB)
  would be one more copy inside every chat that asks a System One question.

Every agent on a machine should share the models loaded there, and so should other
machines that are allowed to ask.

## What the node is

- **One process per machine,** started by the platform's service manager (launchd,
  systemd) and run on the loadngo proactor like every loadngo program.
- **It holds capabilities:**
  - models, restored from the Archive CAS by BLAKE3 hash and checked at every launch, as
    `network::model_service` already does;
  - the decision model;
  - CAS roots, the tools the agent loop uses, thermal state.

  What it holds is configured per machine. The Mac mini holds models; agnes might hold
  only CAS reads and Task work.
- **It speaks two kinds of traffic,** with one node identity and one discovery:
  - **Task**, unchanged: `TaskRequest`, `TaskOffer`, `TaskAccept`, `TaskStatus`,
    `TaskResult`, `TaskAck`. This is for meaningful, verifiable work, as Jay's huddle note
    of 2026-09-15 defines it: selection, status cadence, the submitter's verification and
    rewards. Scoring a batch of reports could be a Task; one decision is not.
  - **Calls:** a request and a reply, with a deadline and cancellation. A System One
    decision takes about 150 ms, and the agent loop's Jev checkpoint asks one every six
    tool calls. Running those through offers, acceptance and acknowledgement would be the
    "synthetic command-running" the huddle note rules out.
- **Roles are switched on explicitly.** As `WORKER_FIRST_TASK_MODEL.md` says of the
  worker, holding a capability does not mean offering it. Each role (Task worker, model
  host, CAS host) is enabled in the node's configuration with its own limits:
  concurrency, energy and thermal budget, which callers may use it.

## What exists and what is new

| Needed | Already in loadngo |
|---|---|
| Discovery, capability tags | Task's multicast groups and request/offer; node ids from `data::machine_identity` |
| Transport | `network`, `data::p2pmsg` messages, the proactor |
| Request shape for decisions | `inference::system_one`: TypeSafe's request and response shape, the same one Strands Decider serves |
| Model provenance | `network::model_service`: restore from the CAS by hash, refuse a substituted copy |
| Authentication | `loadngo-pq-auth`: `SignedAuthToken` (issuer, audience, scopes, nonce, expiry, post-quantum signature) and `VerifyPolicy`, written and not used by anything |
| Engines | `loadngo-gpt-oss`, `loadngo-metal-compute`, `inference::agent` (the shared chat loop) |

New:

1. **The call messages** (a sketch, not yet a wire format):
   - `Call { call_id, capability, deadline_ms, token, body }`: `body` is the
     capability's own request, for a decision the `system_one` request JSON;
   - `Reply { call_id, status, body }`;
   - `Cancel { call_id }`.
2. **A capability query:** a multicast "who holds `<capability>`?" answered by unicast,
   mirroring `TaskRequest`/`TaskOffer` without their assignment semantics, so a caller
   can find a node before calling it.
3. **The node process:** its configuration, the roles above, a bounded queue per hosted
   capability with backpressure, and clean shutdown.

## Authentication comes first

`TASK_FABRIC_TRUST_MODEL.md` records that the Task fabric authenticates nothing: any
host on the LAN can drive a `task-node`. With the shipped exec script that is a limited
exposure. A node that runs models over private text, such as the agent board, for
whoever asks is not acceptable on those terms. So:

- **Every call from another machine carries a `SignedAuthToken`.**
  - Its audience is the node's id and its scopes name the capability.
  - The challenge is bound to `call_id`, so a token cannot be replayed onto another call.
  - The node verifies it with a `VerifyPolicy` against the callers it trusts.
- **Task messages get the same treatment.** That is the fix the trust document sketches,
  and the node is the place to do it once.
- **Calls from the same machine carry no token** (Jay, 2026-10-10): the OS channel is
  enough.
  - The channel is a Unix domain socket, which never leaves the machine. The socket lives
    in a directory only the user can open (`~/.loadngo/node/`, mode 0700).
  - The node also checks the peer's user id when it accepts a connection (`getpeereid`
    on macOS and the BSDs, `SO_PEERCRED` on Linux and Android). That is one system call
    per connection, and it still holds if the directory's mode is ever loosened.
  - The proactor already handles Unix-domain peers on kqueue, epoll and io_uring
    (`proactor/src/io_port.rs`, `PeerAddr::Unix`, with room left there for the peer
    credentials). The Windows port (`iocp.rs`) has no `AF_UNIX` path yet. Windows 10 and
    later support `AF_UNIX`, so that path is added when the node runs on Windows, rather
    than switching to named pipes.

## Order of work

1. **Decision model in process first.** Strands Decider measured on the orchestration
   cases (`~/pudding/eval`). If it earns a place, it is ported into loadngo and connected
   to the agent loop through one decision interface, in-process. No node is needed for
   this step.
2. **The node with one hosted capability:**
   - the existing `task-node` becomes its worker role, behaviour unchanged;
   - it hosts the decision model;
   - the call messages and capability query are added, with pq-auth on calls and Task
     messages;
   - local agents (gpt-oss chat, Kimi, the eval) call the node instead of loading their
     own decider.
3. **Chat models move into the node.** gpt-oss and Gemma are hosted once per machine and
   the chats become clients. `zhoenus_head_model` and its `llama-server` are retired.
4. **Other machines.** dolores and agnes run the node with the roles they can hold.
   dolores's `loadngo-task-node` service (disabled since 2026-09-30 while QCoin is stopped)
   is replaced by the node's worker role.

Each step is measured before the next: a hosted decision against the in-process one
(the same answers, the added latency), and idle cost and wakeups on every platform per
`AGENTS.md`'s thermal rules.

## Left out

- **HTTP.** No `/v1/systemone` over HTTP on localhost (Jay, 2026-10-10). loadngo has no
  HTTP server, and nothing here needs one. Revisit only if a third-party client must call
  the node unchanged.
- **Routing across machines by load or price.** Discovery finds the nodes that hold a
  capability; choosing among them stays the caller's job.

## Rewards

Jay, 2026-10-10: rewards should generally be made available when work actually benefits
human beings in a real, meaningful and positive way.

What follows for the node:

- **A call earns nothing by being made.** Serving a decision or a generation is
  machinery. Whether it helped anyone shows only in the work it served.
- **Rewards stay with Task,** where a submitter states the success criteria, verifies the
  result and acknowledges it (`TASK_REWARD_FLOW.md`). Even there, an acknowledgement says
  the criteria were met, not that the work helped someone. A submitter offers a reward
  for work whose benefit to people it can name, and the reward follows its
  verification.
- **Nothing pays for traffic,** so nothing is gained by making calls or Tasks for their
  own sake. That matches the huddle note's exclusion of synthetic command-running.

## Open

- How trusted keys reach each node (manual placement works for the lab; the trust
  document asks the same question).
- How a Task names the people its work benefits, so that a submitter's reward decision
  can be checked against it.
- The node's name on the wire and as a binary (`loadngo-node`), and when `task-node`
  becomes an alias for it.
