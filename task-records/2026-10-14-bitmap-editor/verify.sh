#!/bin/bash
# The submitter's check for the Task "port loadngo-cpp/BitmapEditor to the loadngo Rust
# framework" (Jay, 2026-10-14: "ensure it can work with all raster type images, jpegs, pngs,
# gifs, tiffs, ideally even mp4s. Utilize the loadngo proactor"). Committed and pushed
# before the work started. task_submitter runs it with LOADNGO_TASK_ARTIFACT_HINT and
# LOADNGO_TASK_ARTIFACT_COPY_PATH. Exit 0 accepts the work.
#
# The C++ original (2004, Win32, ~1,500 lines) opens a .bmp, shows it scaled and as a
# zoomed pixel grid blended by alpha, reads out the hovered pixel (alpha, colour, raw value),
# sets a pixel's alpha on click, pre-multiplies alpha, makes the image opaque, refreshes,
# pastes and saves.
#
# What the result must name:
#   commit: <40 hex>   the last loadngo commit of the work
#   test: <name>       a bitmap-editor test that renders the editor with the software
#                      rasterizer and checks the hovered pixel's cell
#
# The binary's headless interface (part of this intent):
#   bitmap_editor --probe FILE [--frame N] [--pixel X,Y]
#       prints "format: <name>", "size: <W>x<H>", "frames: <n>" and, with --pixel,
#       "pixel: X,Y = rrggbbaa" (hex RGBA, frame N or 0)
#   bitmap_editor --edit FILE [--frame N] [--set-alpha X,Y,A]... [--premultiply]
#       [--make-opaque] --save OUT          (the format follows OUT's extension)
#   bitmap_editor --render FILE --out PNG --size WxH [--hover X,Y]
#       renders the editor window with the software rasterizer and prints
#       "hover_cell: x,y,w,h" (the hovered pixel's cell in the zoomed grid)
#   bitmap_editor FILE                       opens the editor window; prints a line
#       starting "bitmap_editor_loaded" on stderr once the image is shown
#
# Success criteria:
#  1. The commit is on GitHub's dev; at it, bitmap-editor/ is a workspace member and
#     docs/BITMAP_EDITOR.md exists.
#  2. HEAD contains it with bitmap-editor/ unchanged; fmt, strict clippy and the
#     bitmap-editor tests pass, and the named test runs and passes.
#  3. GitHub's CI passed on all four platforms for the commit or a later dev commit.
#  4. Formats: from ffmpeg's 64x48 test pattern, the check makes PNG (with alpha), JPEG, BMP,
#     TIFF, GIF (3 frames) and MP4 (H.264, 10 frames). --probe reports each format's size
#     64x48 and frames 1 (3 for the GIF, 10 for the MP4); its pixel (10,10) equals ffmpeg's
#     own decode exactly for PNG, BMP, TIFF, GIF and MP4 (frame 5 too), and within 12 per
#     channel for JPEG.
#  5. Edits: --set-alpha 10,10,128 then --probe of the saved PNG gives alpha 80 there and the
#     rest unchanged; --premultiply and --make-opaque give the expected pixels; --edit of the
#     MP4 at --frame 5 saved as PNG equals ffmpeg's frame 5.
#  6. Render: --render with --hover 10,10 prints a hover cell whose centre in the rendered
#     PNG has the image's pixel (10,10) colour.
#  7. The window: the editor opened on the PNG prints bitmap_editor_loaded within 60 s; then,
#     doing nothing for 10 s, it makes at most 30 context switches (it idles; it does not
#     poll); it is stopped and none is left running.
set -u
loadngo=/Users/jay/pudding/loadngo
fail() {
    echo "verify: FAIL: $*" >&2
    pkill -f "target/release/bitmap_editor" 2>/dev/null
    exit 1
}
pass() { echo "verify: ok: $*"; }

result=${LOADNGO_TASK_ARTIFACT_HINT:?no artifact hint}
copy=${LOADNGO_TASK_ARTIFACT_COPY_PATH:?no artifact copy path}
[ -f "$result" ] || fail "no result file at $result"
cp "$result" "$copy" || fail "cannot copy the result to $copy"
field() { sed -n "s/^$1: *//p" "$copy" | head -1; }
commit=$(field commit)
test_name=$(field test)
[[ $commit =~ ^[0-9a-f]{40}$ ]] || fail "the result names no commit"
[ -n "$test_name" ] || fail "the result names no test"

# 1. On GitHub.
cd "$loadngo" || fail "no $loadngo"
git fetch -q github dev || fail "cannot fetch github dev"
git merge-base --is-ancestor "$commit" github/dev || fail "$commit is not on github/dev"
git show "$commit:Cargo.toml" | grep -q '"bitmap-editor"' || fail "bitmap-editor is not a workspace member at $commit"
git cat-file -e "$commit:docs/BITMAP_EDITOR.md" || fail "no docs/BITMAP_EDITOR.md at $commit"
pass "1: $commit on github/dev; bitmap-editor is a workspace member; docs/BITMAP_EDITOR.md"

# 2. The gates.
git merge-base --is-ancestor "$commit" HEAD || fail "HEAD does not contain $commit"
git diff --quiet HEAD -- bitmap-editor || fail "bitmap-editor/ has uncommitted changes"
cargo fmt --all -- --check >/dev/null || fail "cargo fmt --check"
cargo clippy -q -p bitmap-editor --all-targets --all-features -- -D warnings || fail "clippy"
cargo test -q -p bitmap-editor >/dev/null 2>&1 || fail "bitmap-editor tests"
one=$(cargo test -q -p bitmap-editor "$test_name" 2>&1)
grep -q "test result: ok. [1-9]" <<<"$one" || fail "$test_name did not run and pass: $one"
pass "2: fmt, clippy, bitmap-editor tests and $test_name pass at $(git rev-parse --short HEAD)"

# 3. CI on the four platforms.
ci=
while read -r id sha conclusion; do
    [ "$conclusion" = success ] || continue
    if git merge-base --is-ancestor "$commit" "$sha" 2>/dev/null; then ci=$id; break; fi
done < <(gh run list --repo jaylauffer/loadngo --branch dev --workflow CI --limit 30 \
    --json databaseId,headSha,conclusion --jq '.[] | "\(.databaseId) \(.headSha) \(.conclusion)"')
[ -n "$ci" ] || fail "no successful CI run on dev contains $commit"
n=$(gh run view "$ci" --repo jaylauffer/loadngo --json jobs --jq '[.jobs[] | select(.conclusion == "success")] | length')
[ "$n" -ge 4 ] || fail "CI run $ci has $n successful jobs, not 4"
pass "3: CI run $ci passed ($n jobs) and contains $commit"

# Fixtures from ffmpeg's test pattern.
cargo build -q --release -p bitmap-editor --bin bitmap_editor || fail "cannot build bitmap_editor"
bin=$loadngo/target/release/bitmap_editor
f=$(mktemp -d)
ff() { ffmpeg -v error -y "$@" || fail "ffmpeg could not make a fixture: $*"; }
ff -f lavfi -i testsrc2=size=64x48:rate=10 -frames:v 1 "$f/base.png"
ff -i "$f/base.png" -vf "format=rgba,geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='if(lt(X,32),255,128)'" "$f/alpha.png"
ff -i "$f/base.png" -q:v 2 "$f/base.jpg"
ff -i "$f/base.png" "$f/base.bmp"
ff -i "$f/base.png" "$f/base.tiff"
ff -f lavfi -i testsrc2=size=64x48:rate=10 -frames:v 3 "$f/anim.gif"
ff -f lavfi -i testsrc2=size=64x48:rate=10 -frames:v 10 -pix_fmt yuv420p -c:v libx264 "$f/clip.mp4"

# ffmpeg's RGBA at (10,10) of FILE's frame N.
reference() { # FILE N
    ffmpeg -v error -i "$1" -vf "select=eq(n\,$2)" -frames:v 1 -f rawvideo -pix_fmt rgba - 2>/dev/null |
        dd bs=1 skip=$(((10 * 64 + 10) * 4)) count=4 2>/dev/null | xxd -p
}
probe_pixel() { # FILE N
    "$bin" --probe "$1" --frame "$2" --pixel 10,10 2>/dev/null | sed -n 's/^pixel: 10,10 = //p'
}
within() { # HEX HEX TOLERANCE
    python3 -I -c 'import sys; a,b,t=bytes.fromhex(sys.argv[1]),bytes.fromhex(sys.argv[2]),int(sys.argv[3]); sys.exit(0 if len(a)==len(b)==4 and all(abs(x-y)<=t for x,y in zip(a,b)) else 1)' "$1" "$2" "$3"
}

# 4. Formats.
for spec in "alpha.png png 1 0" "base.jpg jpeg 1 12" "base.bmp bmp 1 0" "base.tiff tiff 1 0" "anim.gif gif 3 0" "clip.mp4 mp4 10 0"; do
    set -- $spec
    file=$1 format=$2 frames=$3 tolerance=$4
    out=$("$bin" --probe "$f/$file" 2>&1) || fail "--probe $file failed: $out"
    grep -qx "format: $format" <<<"$out" || fail "$file: format is not $format: $out"
    grep -qx "size: 64x48" <<<"$out" || fail "$file: size is not 64x48: $out"
    grep -qx "frames: $frames" <<<"$out" || fail "$file: frames is not $frames: $out"
    ours=$(probe_pixel "$f/$file" 0)
    theirs=$(reference "$f/$file" 0)
    within "$ours" "$theirs" "$tolerance" || fail "$file pixel (10,10): ours $ours, ffmpeg $theirs"
done
ours=$(probe_pixel "$f/clip.mp4" 5)
theirs=$(reference "$f/clip.mp4" 5)
[ -n "$ours" ] && [ "$ours" = "$theirs" ] || fail "clip.mp4 frame 5 pixel (10,10): ours $ours, ffmpeg $theirs"
pass "4: PNG, JPEG, BMP, TIFF, GIF and MP4 probe right (sizes, frames, pixels against ffmpeg)"

# 5. Edits.
before=$(probe_pixel "$f/alpha.png" 0)
"$bin" --edit "$f/alpha.png" --set-alpha 10,10,128 --save "$f/edited.png" >/dev/null 2>&1 || fail "--set-alpha failed"
after=$(probe_pixel "$f/edited.png" 0)
[ "${after:0:6}" = "${before:0:6}" ] && [ "${after:6:2}" = "80" ] || fail "--set-alpha: before $before, after $after"
other_before=$("$bin" --probe "$f/alpha.png" --pixel 20,20 | sed -n 's/^pixel: 20,20 = //p')
other_after=$("$bin" --probe "$f/edited.png" --pixel 20,20 | sed -n 's/^pixel: 20,20 = //p')
[ -n "$other_before" ] && [ "$other_before" = "$other_after" ] || fail "--set-alpha changed another pixel"
"$bin" --edit "$f/alpha.png" --make-opaque --save "$f/opaque.png" >/dev/null 2>&1 || fail "--make-opaque failed"
[ "$("$bin" --probe "$f/opaque.png" --pixel 40,20 | sed -n 's/^pixel: 40,20 = //p' | cut -c7-8)" = ff ] ||
    fail "--make-opaque left pixel (40,20) translucent"
"$bin" --edit "$f/alpha.png" --premultiply --save "$f/pre.png" >/dev/null 2>&1 || fail "--premultiply failed"
src=$("$bin" --probe "$f/alpha.png" --pixel 40,20 | sed -n 's/^pixel: 40,20 = //p')
pre=$("$bin" --probe "$f/pre.png" --pixel 40,20 | sed -n 's/^pixel: 40,20 = //p')
python3 -I -c 'import sys; s,p=bytes.fromhex(sys.argv[1]),bytes.fromhex(sys.argv[2]); a=s[3]; sys.exit(0 if all(abs(p[i]-s[i]*a/255)<=1 for i in range(3)) and p[3]==a else 1)' "$src" "$pre" ||
    fail "--premultiply: source $src, result $pre"
"$bin" --edit "$f/clip.mp4" --frame 5 --save "$f/frame5.png" >/dev/null 2>&1 || fail "MP4 frame export failed"
[ "$(probe_pixel "$f/frame5.png" 0)" = "$(reference "$f/clip.mp4" 5)" ] || fail "the exported MP4 frame 5 differs from ffmpeg's"
pass "5: set alpha, make opaque, premultiply and MP4 frame export are right"

# 6. Render.
cell=$("$bin" --render "$f/alpha.png" --out "$f/scene.png" --size 1000x700 --hover 10,10 2>/dev/null | sed -n 's/^hover_cell: //p')
IFS=, read -r cx cy cw ch <<<"$cell"
[ -n "${ch:-}" ] || fail "--render printed no hover cell"
centre=$("$bin" --probe "$f/scene.png" --pixel $((cx + cw / 2)),$((cy + ch / 2)) | sed -n 's/^pixel: [0-9]*,[0-9]* = //p')
want=$(probe_pixel "$f/alpha.png" 0)
[ "${centre:0:6}" = "${want:0:6}" ] || fail "the hovered cell's centre is $centre, the pixel is $want"
pass "6: the rendered editor shows pixel (10,10) in its hover cell at $cell"

# 7. The window.
"$bin" "$f/alpha.png" 2>"$f/window.log" &
editor=$!
started=$SECONDS
until grep -q "^bitmap_editor_loaded" "$f/window.log"; do
    kill -0 "$editor" 2>/dev/null || fail "the editor exited before loading: $(tail -3 "$f/window.log")"
    ((SECONDS - started < 60)) || fail "the editor did not load within 60 s"
    sleep 1
done
sleep 2
idle=$(top -l 2 -s 10 -pid "$editor" -stats pid,csw | awk -v p="$editor" '$1==p {print $2}' | tr -d '+' | tr '\n' ' ')
read -r first second <<<"$idle"
[ -n "${second:-}" ] || fail "no context-switch samples for the editor"
kill "$editor" 2>/dev/null
wait "$editor" 2>/dev/null
(( second - first <= 30 )) || fail "the idle editor made $((second - first)) context switches in 10 s"
sleep 1
if pgrep -f "target/release/bitmap_editor" >/dev/null; then fail "a bitmap_editor process is still running"; fi
pass "7: the window loaded the image, idled at $((second - first)) context switches in 10 s, and stopped"
echo "verify: accepted"
