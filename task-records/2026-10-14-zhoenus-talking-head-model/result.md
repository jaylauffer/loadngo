# Result: the Zhoenus talking head asks the local model

Worker: claude-code-macmini (Claude Code, Opus 5.5, in Jay's session). Task request in the
logs, assignment 9288111485905133421; intent and check in loadngo d92bb078.

commit: 543ffa3632217af5f22453b42b784c5790340b8a

## What changed (zhoenus 543ffa3)

- `Source/Zhoenus/ZhoenusModelCall.{h,cpp}`: one call to loadngo's gpt_oss_serve over its
  Unix socket (JSON line out, JSON line in; send/receive timeouts at the deadline plus
  2 s; SIGPIPE off); Mac and Linux, "unsupported" elsewhere.
- `ZhoenusTalkingHeadAssistant`: SubmitPlayerUtterance shows the built-in reply, then
  asks the model on a pool thread with the game's state in the instructions; the answer
  replaces the caption on the game thread and holds ModelReplyHoldSeconds; a newer
  question drops an older reply; with no answer the built-in reply stays and the log
  says why. `Zhoenus.Ask` console command, `-ZhoenusAsk=` and `-ZhoenusModelSocket=`.
- `Zhoenus.Build.cs`: Json. `Docs/talking-head-assistant.md`: the bridge, no llama.

## How it was checked before this hand-over

- ZhoenusEditor builds (a full 25-minute rebuild after the new file; the first attempt
  through the /Users/jay/pudding/zhoenus symlink left one truncated object file, so builds
  now use the drive's real path); a later build is a no-op.
- Level-1 in game mode with gpt_oss_serve: "How do I save the flyers?" asked 16 s after
  launch, answered in 2.59 s ("Wake them with a ZapEm shot, then use the ship's smooth
  six-axis turns to guide the awakened flyers into the goal gate..."). Without a server:
  "unreachable", built-in reply kept, 6 s after launch.
- The first launch after relinking spends minutes in macOS's library checks (a 300 s run
  timed out loading modules); later launches reach the level in 6 to 16 s.

## Not done

- How the player asks in play (a text box near the head, or speech) is Jay's call.
- Not built or run for Linux, Windows or iOS.
