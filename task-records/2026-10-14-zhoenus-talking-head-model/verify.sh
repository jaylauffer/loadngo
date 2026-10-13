#!/bin/bash
# The submitter's check for the Task "the Zhoenus talking head asks the local model" (Jay,
# 2026-10-14: "remove the noise and wire it up"). Committed and pushed before the work
# started. task_submitter runs it with LOADNGO_TASK_ARTIFACT_HINT (the worker's result) and
# LOADNGO_TASK_ARTIFACT_COPY_PATH (the copy the receipt hashes). Exit 0 accepts the work.
#
# Before: the talking head answered only from built-in hints. Its design said a later
# bridge would send the player's words to a local llama-server/gpt-oss endpoint; nothing
# called a model, and nothing in the game let the player ask.
#
# What the result must name:
#   commit: <40 hex>   the Zhoenus commit with the change
#
# Success criteria:
#  1. The commit is on GitHub's dev of jaylauffer/zhoenus. It changes
#     Source/Zhoenus/ZhoenusTalkingHeadAssistant.cpp and Docs/talking-head-assistant.md
#     and adds Source/Zhoenus/ZhoenusModelCall.cpp; the doc no longer mentions llama.
#  2. Zhoenus's HEAD contains it with Source/ unchanged, and the ZhoenusEditor target
#     builds there.
#  3. With loadngo's gpt_oss_serve (release, from loadngo HEAD) on a socket of its own,
#     Zhoenus in game mode on Level-1, given
#       -ZhoenusAsk="How do I save the flyers?" -ZhoenusModelSocket=<that socket>,
#     logs "LogZhoenusTalkingHead: Display: model replied in <seconds> s: <text>" with
#     non-empty text within 300 s of launch.
#  4. With no server at the socket, a second run logs "no model reply (unreachable"
#     and that it kept the built-in reply; the game keeps running (no crash before it).
#  5. Afterwards no ZhoenusEditor process is left, and the server stops cleanly.
set -u
loadngo=/Users/jay/pudding/loadngo
zhoenus=/Volumes/Micro-Data-Transfer/Zhoenus/zhoenus
fail() {
    echo "verify: FAIL: $*" >&2
    pkill -f "ZhoenusEditor.app/Contents/MacOS/ZhoenusEditor" 2>/dev/null
    [ -n "${server:-}" ] && kill "$server" 2>/dev/null
    exit 1
}
pass() { echo "verify: ok: $*"; }

result=${LOADNGO_TASK_ARTIFACT_HINT:?no artifact hint}
copy=${LOADNGO_TASK_ARTIFACT_COPY_PATH:?no artifact copy path}
[ -f "$result" ] || fail "no result file at $result"
cp "$result" "$copy" || fail "cannot copy the result to $copy"
commit=$(sed -n 's/^commit: *//p' "$copy" | head -1)
[[ $commit =~ ^[0-9a-f]{40}$ ]] || fail "the result names no commit (a line 'commit: <40 hex>')"
demo=$(dirname "$copy")/demo
mkdir -p "$demo"

# 1. On GitHub.
cd "$zhoenus" || fail "no $zhoenus (is the Micro-Data-Transfer drive attached?)"
git fetch -q github dev || fail "cannot fetch zhoenus's github dev"
git merge-base --is-ancestor "$commit" github/dev || fail "$commit is not on zhoenus's github/dev"
changed=$(git diff --name-status "$commit^" "$commit")
grep -Eq "^M[[:space:]]+Source/Zhoenus/ZhoenusTalkingHeadAssistant.cpp$" <<<"$changed" || fail "the talking head's source is not changed"
grep -Eq "^M[[:space:]]+Docs/talking-head-assistant.md$" <<<"$changed" || fail "the talking head's doc is not changed"
grep -Eq "^A[[:space:]]+Source/Zhoenus/ZhoenusModelCall.cpp$" <<<"$changed" || fail "ZhoenusModelCall.cpp is not added"
if git show "$commit:Docs/talking-head-assistant.md" | grep -i -n llama; then fail "the doc still mentions llama"; fi
pass "1: $commit on zhoenus github/dev with the bridge and its doc"

# 2. It builds.
git merge-base --is-ancestor "$commit" HEAD || fail "zhoenus HEAD does not contain $commit"
git diff --quiet HEAD -- Source || fail "zhoenus Source/ has uncommitted changes"
../unreal/Engine/Build/BatchFiles/Mac/Build.sh ZhoenusEditor Mac Development \
    -Project="$zhoenus/Zhoenus.uproject" -WaitMutex >"$demo/build.log" 2>&1 || fail "the ZhoenusEditor build failed (demo/build.log)"
pass "2: ZhoenusEditor builds at $(git rev-parse --short HEAD)"

# The game in game mode on Level-1 until PATTERN is logged, the game exits, or LIMIT s.
run_game() { # LOG PATTERN LIMIT [arguments...]
    local log=$1 pattern=$2 limit=$3
    shift 3
    : >"$log"
    "$zhoenus/Binaries/Mac/ZhoenusEditor.app/Contents/MacOS/ZhoenusEditor" "$zhoenus/Zhoenus.uproject" \
        /Game/Map/Level-1 -game -nullrhi -nosound -unattended -nosplash -NoLoadingScreen \
        -ABSLOG="$log" "$@" >/dev/null 2>&1 &
    local game=$! started=$SECONDS
    game_outcome=timeout
    while ((SECONDS - started < limit)); do
        if grep -Eq "$pattern" "$log"; then game_outcome=matched; break; fi
        if ! kill -0 "$game" 2>/dev/null; then game_outcome=exited; break; fi
        sleep 1
    done
    game_seconds=$((SECONDS - started))
    kill "$game" 2>/dev/null
    wait "$game" 2>/dev/null
}

# 3. With the model.
cd "$loadngo" || fail "no $loadngo"
cargo build -q --release -p loadngo-gpt-oss --bin gpt_oss_serve || fail "cannot build gpt_oss_serve"
home=$(mktemp -d)
socket=$home/node/gpt-oss.sock
"$loadngo/target/release/gpt_oss_serve" --socket "$socket" >"$demo/server.out" 2>"$demo/server.log" &
server=$!
ready=$SECONDS
until grep -q "^gpt_oss_serve_listening" "$demo/server.out"; do
    kill -0 "$server" 2>/dev/null || fail "gpt_oss_serve exited before listening (demo/server.log)"
    ((SECONDS - ready < 300)) || fail "gpt_oss_serve did not listen within 300 s"
    sleep 1
done
run_game "$demo/game-with-model.log" "LogZhoenusTalkingHead: Display: (model replied|no model reply)" 300 \
    -ZhoenusAsk="How do I save the flyers?" -ZhoenusModelSocket="$socket"
reply=$(grep -E "LogZhoenusTalkingHead: Display: model replied in [0-9.]+ s: .*[^[:space:]]" "$demo/game-with-model.log" | head -1)
[ -n "$reply" ] || fail "no model reply in the game's log ($game_outcome after $game_seconds s; demo/game-with-model.log)"
pass "3: the talking head answered from the model after ${game_seconds} s: ${reply#*Display: }"

# 4. Without it.
kill -TERM "$server"
wait "$server"
code=$?
server=
[ "$code" -eq 0 ] || fail "gpt_oss_serve exited $code on SIGTERM"
[ ! -e "$socket" ] || fail "gpt_oss_serve left its socket"
run_game "$demo/game-without-model.log" "LogZhoenusTalkingHead: Display: (model replied|no model reply)" 300 \
    -ZhoenusAsk="How do I save the flyers?" -ZhoenusModelSocket="$socket"
grep -Eq "LogZhoenusTalkingHead: Display: no model reply \(unreachable.*kept the built-in reply" "$demo/game-without-model.log" ||
    fail "no fallback line without the model ($game_outcome after $game_seconds s; demo/game-without-model.log)"
[ "$game_outcome" = matched ] || fail "the game $game_outcome before the fallback line"
pass "4: without the model the talking head kept its built-in reply"

# 5. Nothing left running.
sleep 2
if pgrep -fl "ZhoenusEditor.app/Contents/MacOS/ZhoenusEditor"; then fail "a ZhoenusEditor process is still running"; fi
pass "5: no ZhoenusEditor left; gpt_oss_serve stopped cleanly on SIGTERM"
echo "verify: accepted"
