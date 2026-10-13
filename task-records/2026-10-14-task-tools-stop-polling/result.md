# Result: the Task tools stop polling their sockets

Worker: claude-code-macmini (Claude Code, Opus 5.5, in Jay's session). Task request
17763077972088297954, assignment 4158359342281407921; intent and check in loadngo 6300d0ce.

commit: efdb1ecc20aeee2f6520cb0390876498ffb166d9
test: an_inbox_receives_a_message_and_returns_none_at_its_deadline

## What changed

- `network/src/inbox.rs` (new): `Inbox` keeps one `recv_from` posted per socket on the
  platform proactor (kqueue, epoll, io_uring, IOCP); `next_until(deadline)` sleeps in the
  kernel until a datagram or the deadline. One 64 KiB buffer per socket, reused for
  every receive. `datagram` and `receive_again` serve code on its own proactor.
  `Network::raw_sockets` gives the handles.
- `task_worker`, `task_submitter`, `task_offer`, `task_request`: their 25 ms sleep loops
  are `while let Some(..) = inbox.next_until(deadline)?` loops; no `thread::sleep` is left
  in `network/src/bin/`.
- `task-node`: on the platform port instead of `ChannelPort`, with its receives posted;
  the 250 ms drain timer is gone. `--idle-interval-millis` is accepted and ignored
  (dolores's launcher passes it).
- `docs/TASK_OFFER_PROTOCOL.md` "How the tools wait".

## How it was checked

- CI run 38460546575 at efdb1ecc passed on all four platforms; the inbox test ran and
  passed in each job (kqueue on macOS, io_uring on Linux x86-64 and arm64, IOCP on
  Windows).
- Doing nothing (`/usr/bin/time -l`, Mac mini): task_worker 5 s 163 -> 5 to 8 context
  switches, task_submitter 5 s 167 -> 9, task-node 10 s 53 -> 8 to 11; deadlines kept.
- Loopback cycles with task_worker and with task-node accepted.
- verify.sh as pushed in 6300d0ce was run once on scratch paths before this hand-over:
  all five parts passed.

## Not done

- Not run on Linux, Android or Windows hardware outside CI; dolores's task-node service
  stays disabled (QCoin stopped).
- `network::model_service` still waits for llama-server's startup in 250 ms steps; it
  is not a Task tool and was left alone.
