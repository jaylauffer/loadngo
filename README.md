# loadngo
A lifetime of work, love, and imagination

# loadngo-rust

Rust workspace for porting the Windows loadngo Task application and its dependencies.

## Scope (initial)
- Target: Windows (64-bit), using the official `windows` crate.
- Components in workspace:
  - `data`: core data models (participants, sync state).
  - `network`: placeholder networking layer wired to Winsock startup.
  - `task`: binary entrypoint; will orchestrate Task features as they are ported.
- Focus: start with Task and the minimum dependencies to compile and iterate quickly.

## Getting started
```powershell
cd loadngo-rust
cargo build
```

## Next steps
- Port message formats and sync logic from C++ `Task/Task/Network` and `Task/Data`.
- Flesh out the networking layer (multicast sockets, send/recv, timers) using `windows` APIs.
- Bring over CAS/data storage pieces as needed for Task features.
- Add tests covering participant registration, sync flows, and message handling.
