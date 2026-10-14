# Agent Board

Live coordination state for agents working in `~/pudding`. The rules are in
[`COLLABORATION.md`](COLLABORATION.md). Update this file when you start,
pause, hand off, or finish. Newest rows first. Times are local (+07) unless
marked UTC.

Keep it short, because every session loads this file. A row is one or two
lines: what changed, where (commit SHAs or paths), and what is still open.
Evidence belongs in commit messages, repo docs or `reviews/`, not here.
Handoffs older than about a week move to [`agent-board-archive/`](agent-board-archive/).
Unedited copies of the full board before each condensing are kept there:
`2026-09-30-before-condensing.md` and `2026-10-14-before-condensing.md` (Jay:
"prune it"; every handoff before 10-12, Codex's espeak-ng-rs checkpoint rows and
resolved uncommitted-work rows are there word for word).

## Roles

Set by Jay.

| Role | Who |
|---|---|
| Priorities, approvals, pushes, releases | Jay |
| Orchestrator (assigns and sequences agent work) | Jay |
| Implementer | undecided; both agents currently take work only from Jay |
| Reviewer | undecided |

## Active claims

| Since | Agent | Repo / area | Paths | Task | Status |
|---|---|---|---|---|---|
| 2026-10-14 | Claude Code | loadngo; pudding CAS on Zhoenus II | `data/src/{archive_cas.rs,bin/archive_cas_add.rs}`, `docs/ARCHIVE_CAS.md`, `task-records/`; archive `pudding-20260917` (new versions) | Task 6 (Jay): `archive_cas_add --replace` records a replace as `changed` in one version; the board, its archive and the Task records versioned in the pudding CAS, signed with `jay-macmini`. | Active |
| 2026-10-14 | Codex | espeak-ng-rs | Native MBROLA host Rust module and C configuration, `rust/ffi.rs`, `src/libespeak-ng/CMakeLists.txt`, `tests/{CMakeLists.txt,rust_mbrola_host.c}`, port docs; build/target directories | Replace complete synth_mbrola.c native host with Rust ownership, startup/effect dispatch and retained clause/PCM state; preserve original C as oracle. | Active; serial local builds; no device hold; push authorization retained. |

## Jay's huddle

Use this section for a concise decision or clarification from Jay that both
agents should apply. It is asynchronous shared context, not a replacement for
Jay's direct approval, and neither agent may use it to direct the other.
Read the newest note before starting related work.

| When | From | Topic | Note |
|---|---|---|---|
| 2026-10-14 | Jay | Unreal Zhoenus | No further work on the Unreal Zhoenus game for now. The talking head asks gpt_oss_serve (zhoenus `543ffa3`); speech input comes later. |
| 2026-10-12 | Jay | Local agent direction | Change of direction: build a functional local agent that writes code and completes tasks (replaces the orchestrator proposal). First, the Strands Decider on the GPU. |
| 2026-10-10 | Jay | Rewards; loadngo node | Rewards should generally be made available when work actually benefits human beings in a real, meaningful and positive way. The task node is one role of a resident "loadngo node" per machine (loadngo `docs/LOADNGO_NODE.md`); calls on the same machine use a Unix domain socket and skip tokens; no HTTP. |
| 2026-10-04 | Jay | CI-first platform validation | Make edits on this Mac; let CI check Linux and correct platform failures here. Direct Linux development/testing needs an agreed reason; for sustained Linux work, consider running Codex there. Encounter direction: three broader tiers by room depth; elite and guardian separate. |
| 2026-09-15 06:20 +07 | Jay | Loadngo Task | `loadngo` Task is the submitter-driven protocol for distributing **meaningful, verifiable work** across the lab: `TaskRequest` discovers workers; a direct `TaskOffer` expresses capability; only `TaskAccept` assigns work; the selected worker reports `TaskStatus` and `TaskResult`; the submitter verifies the success criteria and closes with `TaskAck`. The current runtime attaches the qcoin receipt only after accepted work is durably anchored. Having a task skill or runtime does **not** make an agent an automatic listener or worker. It is not generic issue tracking, a backlog, or synthetic command-running. |

## Open items

Unfinished work and decisions that outlive their handoff. Remove a row when it
is done or dropped.

| Since | Item | Next / who |
|---|---|---|
| 2026-10-14 | `espeak-ng-rs` full Rust/proactor port active; MBROLA compiler and platform CI fixes pushed through `f5e0b0fa`; checkout clean. | Exact-head [Rust-port CI38449270268](https://github.com/jaylauffer/espeak-ng-rs/actions/runs/38449270268) passes all seven Linux/macOS/Windows jobs; Gradle38449270481 also passes. Remaining frontend/API/primitive hosts, production MBROLA process integration, CLI/platform and physical runtime/thermal gates are in `docs/REMAINING_PORT.md`; `docs/RUST_PORT.md` records compiler and CI evidence. |
| 2026-10-14 | Task receipts (`loadngo/task-records/`) and this board are unsigned. Their home is the signed pudding CAS (Dilithium2-signed versions with parents and change records), not QCoin; today only the repository pushes and this Mac hold them. | Claude Code: version them in the pudding CAS and sign with this Mac's `jay-macmini` key (Jay, 10-14: the agents run on this device). First, `archive_cas_add` records a replace as `changed`. |
| 2026-10-14 | Codex's worktree `~/pudding/loadngo-espeak-stdio` (branch `espeak-stdio`) is clean; its commit `1e9d480f` is in loadngo `dev`. | Codex or Jay: `git worktree remove`. |
| 2026-10-13 | sng-roguelite next step after the play logs (`docs/GAMEPLAY_IMPROVEMENT_PLAN.md`, "0.5.17 and 0.5.18 results"). | Jay: sentinel/swarm choreography, middle overlap, or a second floor. |
| 2026-10-13 | Game audio (`loadngo/docs/GAME_AUDIO_RUNTIME.md`): Linux playback not checked by ear; Android music still on MediaPlayer; iOS/desktop thermal budget not measured; mahjong's sound effects not heard. | Jay to listen; the rest unassigned. |
| 2026-10-12 | Achievement catalogs proposed for sng-zhoenus and sng-mahjong (their `docs/game-design.md`). | Jay: approve or change; Achievements then joins those menus. |
| 2026-10-12 | Coding-agent tasks (`agent-tasks/` README, "run 2"): gpt-oss 21/58, but 20 tasks flipped between runs. | Jay: choose step 3 from the README; 3 runs per task. |
| 2026-10-12 | Strands Decider: weights from the CAS as bf16 (loading peaks at 11 GB), then hosted by the node (`loadngo/docs/LOADNGO_NODE.md`). | Unassigned. |
| 2026-10-11 | Code editor (`loadngo/docs/CODE_EDITOR.md`): a Finder drop not checked live; the terminal has no Windows (ConPTY) path or reflow; later: replace in files, outline, run/debug. | Jay: daily use via `~/pudding/launch-ide.sh`. |
| 2026-10-02 | `loadngo-archive-cas` compressed (zstd 3, finished 10-02, 0 failed); not re-verified since the 10-01 power loss. | Jay: run `archive_cas_verify` when a day is free. |
| 2026-09-30 | dolores user service `loadngo-task-node` was restarting every 5 s (`No such device`, 13,500+ restarts, 22 MB `~/.local/state/loadngo/task-node.err`), probably because QCoin is stopped. Jay disabled and stopped it 09-30. | Re-enable (`systemctl --user enable --now loadngo-task-node`) when QCoin runs again; the log can be trimmed. |
| 2026-09-29 | agnes shows a steady 25% I/O wait: starlight waiting in io_uring, counted as I/O wait by kernel 6.18. Harmless (`reviews/2026-09-29-agnes-iowait-claude.md`). | Jay: leave it, or have loadngo-proactor pass `IORING_ENTER_NO_IOWAIT` on 6.15+ kernels (test on both Pis). |
| 2026-09-29 | Kimi Linear on the GPU: 3,895-token prompt ~9.9 s, decode ~31 ms/token. | Next step, not started: one GPU submission per layer. |
| 2026-09-29 | System monitor: leak fixed (`2ac9f905`). Not done from Codex's list: proactor-offloaded sampling, close/restart controls. | Unassigned. |
| 2026-09-27 | Kimi speech: Apple on-device recognizer is a stopgap; plan in loadngo `docs/PHONEME_RECOGNITION.md`. | Jay: checkpoint/download decision. |
| 2026-09-25 | Games take loadngo by git pin; crates.io publishing later. The loadngo<->qcoin cycle that blocked it is gone (10-06, loadngo `6bae953a`); qcoin and EAB could now take loadngo by git too. | Deferred by Jay. |
| — | iOS provisioning profiles on Zheonus VI are valid to 2026-10-19 (refreshed 10-13; `ios-signing-notes.md`). | Ask Jay to refresh before a device build after that. |

## Uncommitted work

Found in the working trees; not committed by its author yet. Do not commit,
revert, or tidy someone else's row — ask Jay. Checked against `git status` 2026-10-14.

| Found | Author | Repo | Paths | Notes |
|---|---|---|---|---|
| 2026-10-13 | gpt-oss | sng-mahjong | `src/render.rs` | Tile depth/shadow change from Jay's gpt-oss chat; its `cargo check`/`test` passed; installed on both phones by the `device` tool. Review and commit: Jay. |
| 2026-10-01 | Kimi | sng-mahjong | `SOUND_IMPLEMENTATION_PLAN.md` (untracked) | Her sound plan from before her `Cargo.toml` edit failed; mahjong's sound was later built another way (`70f0032`). Keep or discard: Jay. |
| 2026-09-21 | Claude Code | sng-rusty | `Cargo.lock`, `assets_src/drawthings_model_registry.json`, `assets_src/atelier/tiles/`, `assets_src/prompts/atelier_tile_*`, `assets/experimental/atelier/tiles/` | Rong Atelier mahjong-tile workflow (09-21 handoff, in the archive). Jay rejected the images on 09-22. sng-rusty has no reachable remote. Keep or discard: Jay. |
| 2026-09-20 | Claude Code | sng-roguelite | `docs/IOS_BUILD.md` | Pointer to `ios-signing-notes.md`, never committed. (The mahjong and zhoenus copies are now committed.) |
| 2026-09-17 | unknown (engine fork) | unreal (symlink to Micro-Data-Transfer) | untracked `Engine/Config/DefaultEngine.ini` | On 10-14 only this file is left; the 181 deletions and the modified plugin `.cpp` noted 09-17 are gone from the working tree. Local `release` is Epic 5.8, far ahead of the `jareal` fork, so a push would upload a whole engine release. Jay to decide. |

## Handoffs

| When | Agent | What | Where | Verified / open |
|---|---|---|---|---|
| 2026-10-14 | Claude Code | Model service on loadngo inference (Jay: no llama-server): `gpt_oss_serve` answers JSON-line calls on `~/.loadngo/node/gpt-oss.sock`; the Zhoenus talking head asks it (Tasks 4 and 5). Jay: speech input later, no more Unreal Zhoenus work now. | loadngo `0a0a728a`, `85c54a5d`; records `843ec9d6` (first run rejected by its own check), `7d00be4b`, `c0ec0399`; zhoenus `543ffa3` | Accepted by their checks; CI green on four. |
| 2026-10-14 | Claude Code | Task plane, Tasks 2 and 3: `task_submitter` keeps its check's output (a closure record for every checked result); the Task tools wait on proactor receives (`network::inbox`), not 25 ms sleeps; task-node's 250 ms timer is gone. | loadngo `eb8d25f1`, `efdb1ecc`; records `20be0673`, `2c1e843f` | Idle context switches 163/167/53 down to 5-12. |
| 2026-10-14 | Claude Code | gpt-oss refusal note: a keyword check and a command check, one note a turn; an ignored note leaves the first answer. First Task kept as proof of intent and result; `loadngo/task-records/` started. | loadngo `a2d26113`, `76072414`, `936e1dac` | Receipt signing: see Open items. |
| 2026-10-14 | Codex | Complete MBROLA table compiler in Rust with pooled proactor I/O; repaired GNU static archive extraction, standalone Windows Cargo host binding and platform test fixtures. | espeak-ng-rs `cd7d59d2`, `402d2352`, `f5e0b0fa`, pushed to `origin/dev`; port docs and `/private/tmp/espeak-{mbrcompile,ci}-*` | 226 C differential cases, all 52 three-lane CLI payloads, Rust581/minimal482, native127/128/130 runnable, strict Clippy and four native-host target checks pass. Final CI38449270268 all seven jobs green. Clean checkout, claim released, no device held or owned command running. Full migration remains active; push authorization retained. |
| 2026-10-04..14 | Codex | espeak-ng-rs full Rust/proactor port: 97 earlier checkpoint rows, each moving one C subsystem into Rust with C-oracle parity, sanitizer and four-target gates. | espeak-ng-rs `docs/RUST_PORT.md`, `docs/REMAINING_PORT.md`; every row word for word in `agent-board-archive/2026-10-14-before-condensing.md` | See Open items. |
| 2026-10-13 | Claude Code | `device` tool for the local models (gpt-oss had said it was sandboxed): list; android/ios through each repo's device script; mac runs the game. | loadngo `81508d9d`; kimi `d8101bd` | Checked on both phones and by gpt-oss itself. |
| 2026-10-13 | Claude Code | Released sng-roguelite v0.5.19 (itch: android, both Linux) and sng-zhoenus v0.1.6; release workflows pass the sysroot `-L` and give Linux jobs 60 min. | roguelite `9298329` (tag v0.5.19); zhoenus `ab8987d` (tag v0.1.6) | — |
| 2026-10-13 | Claude Code | One audio backend (rodio removed): loadngo's mixer everywhere, blocking audio work on the offload pool, music decoded ahead; the output closes after 3 s of silence on iOS and desktop. | loadngo `3e0f57f0`..`75f47386`, `b7308354`; mahjong 0.1.4, zhoenus 0.1.6, roguelite 0.5.19 | See Open items (game audio). |
| 2026-10-13 | Claude Code | Task app on ui-core/host-desktop: one source for macOS, Linux and Windows (Win32 port removed); the Linux host no longer redraws idle apps at 60 Hz. | loadngo `e29597a7`, `f6710564`, `6e1ac699`, `c0980d5c`, `82c477eb`, `a545f5d7` | Not run by hand on Windows; Schedule, Report, Print and Sync not ported. |
| 2026-10-13 | Claude Code | sng-mahjong sound effects (synthesized, deterministic) and a Sound panel; 0.1.2. | mahjong `70f0032` | Jay to listen. |
| 2026-10-13 | Claude Code | sng-roguelite play logs: 20 runs since 10-07 from the iPhone and Mac mini. | roguelite `902db55`; root `reviews/2026-10-13-play-logs/` | Jay's choice: see Open items. |
| 2026-10-12 | Claude Code | Game title menus: new `loadngo-game-menu` crate, used by roguelite, zhoenus and mahjong. | loadngo `dd3f849f`, `d84568d4`; mahjong `9b3ff23`; zhoenus `70a9c8f`; roguelite `1add7e6` | Achievement catalogs: see Open items. |
| 2026-10-12 | Claude Code | Coding-agent tasks: 58 tasks scored by cargo; gpt-oss-20b 14/51, then 21/58 after new guards. | root `agent-tasks/`; loadngo `4090943d`, `38a7142f` | Step 3: see Open items. |
| 2026-10-12 | Claude Code | sng-rusty LFS back: rudolfs on agnes rebound to 10.10.10.4; dolores's duplicate retired, so agnes is the only LFS server. | sng-rusty `d0a61ff` (local; push on hold); zhoenus `.lfsconfig` | Besides the Mac checkout, agnes's microSD is the only copy; backup in `loadngo-archive` `f138f2f7` (unsigned). |
| 2026-10-12 | Claude Code | Strands Decider on the GPU (bf16, LoRA kept apart): 0.45 s per report. | loadngo `41b615aa`, `ad29903f` | Next steps: see Open items. |

## Shared devices

**Latest update from Jay, 2026-09-24:** Agnes and Dolores now use **2.4 GHz** Wi-Fi; 5 GHz has been unstable. Gretta is shut down and remains **offline indefinitely**. Do not probe or provision Gretta.

| Resource | Held by | Since | Notes |
|---|---|---|---|
| Android phone — Xiaomi 22111317I, adb `e8a6ab881222` | free | 2026-10-14 | sng-mahjong release with gpt-oss's uncommitted `src/render.rs`; sng-zhoenus `70a9c8f`; sng-roguelite 0.5.18 `1add7e6`. |
| iPhone — Zheonus VI | free | 2026-10-13 | sng-mahjong (with that `src/render.rs`), sng-roguelite 0.5.19, sng-zhoenus 0.1.6. Profiles valid to 2026-10-19. |
| `dolores` 10.10.10.6 | free | 2026-10-14 | No game left running (checked 10-14). `~/pudding/loadngo` fast-forwarded to `85c54a5d` for Linux gates. Nubwo pad attached. Its rudolfs is retired: agnes serves LFS. |
| `agnes` 192.168.1.160 | free | 2026-10-12 | Pi 4. The only Git LFS server: rudolfs on 10.10.10.4:8080 (wired only, no auth), `/var/lib/rudolfs` (sng-rusty, rrg/zhoenus). starlight on `wlan0`; four CI runners CPU-capped in `ci-runners.slice`. |
| QCoin services (agnes, dolores) | stopped at Jay's request | 2026-09-24 | Still enabled at boot. Runbook: `qcoin/docs/LAB_OPERATIONS.md`. |
| `gretta` | offline indefinitely | 2026-09-24 | Do not probe. |
| Jarraya NVMe (2 TB, APFS) | free | 2026-10-03 | Kimi K3 checkpoint (complete), Kimi Linear bf16 and MXFP4-experts checkpoints, wav2vec2 model, Gemma 4 31B-it. |
| Mac mini GPU / Neural Engine | free | 2026-10-14 | Nothing running (`gpt_oss_serve` and the Zhoenus test runs stopped). |
| Zhoenus II (1 TB, APFS) | free | 2026-10-01 | `pudding-cas`: signed `pudding-20260917` root `4d8babf2` (build output removed 10-01), compressed (zstd 3, `compression.json` on); 22 GiB used. |
| Loadngo Archive Staging | free | 2026-10-12 | `loadngo-archive-cas`: `loadngo-archive` `dfec4e4f` signed; unsigned on top, for Jay to sign: `473c8f4e` (decider pilot, 10-10), then `f138f2f7` (10-12, adds `git-lfs/rudolfs-agnes-2026-10-12/`: agnes's 1,692 LFS objects, SHA-256 checked after copying). Also holds Jay's `gemma-4-31B-it` and `GLM-5.3` downloads. |
| My Passport (2 TB, NTFS) | free | 2026-09-15 | Jay may wipe it. Only unique content found: `zhoenus-gj2-1827/unreal/Zhoenus/` (849 MB, 2021-11-08) and two engine commits in `zhoenus-gamejam/unreal`. |
| Intel B250 BTC-12P board | tabled by Jay | 2026-09-15 | No VGA output; netboot not set up. |
