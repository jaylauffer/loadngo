#!/bin/sh
# Installs loadngo's launcher as a Mac app, "loadngo Launcher", so it is
# started like any other: from Spotlight (Cmd-Space, "loadngo"), Launchpad or
# the Dock.
#
#   scripts/install-launcher.sh [--binary PATH] [--to FOLDER] [--open]
#
#   --binary PATH  optional  The built launcher to install. Without it the
#                            script builds target/release/launcher first
#                            (cargo build --release -p loadngo-launcher).
#   --to FOLDER    optional  Where the app goes. Default /Applications, or
#                            ~/Applications when that cannot be written.
#   --open         optional  Open the app once it is installed.
#   -h, --help     optional  Print this help.
#
# The app is a copy of the binary with launcher/assets/icon-1024.png as its
# icon, signed ad hoc for this Mac. Running the script again replaces it; a
# launcher that is open keeps running the old copy until it is quit.
#
# What the launcher lists is not part of the app and is not touched:
# ~/Library/Application Support/loadngo-launcher/catalog.json (see
# docs/LAUNCHER.md).
#
# Undo: move "loadngo Launcher" from Applications to the Trash.
#
# Example:
#   scripts/install-launcher.sh --open
set -eu

usage() { sed -n '2,27p' "$0" | sed 's/^# \{0,1\}//'; }
fail() {
    echo "install-launcher: $1; see --help" >&2
    exit 2
}

binary=
to=
open_after=no
while [ $# -gt 0 ]; do
    case "$1" in
    --binary)
        [ $# -ge 2 ] || fail "--binary needs a path"
        binary=$2
        shift 2
        ;;
    --to)
        [ $# -ge 2 ] || fail "--to needs a folder"
        to=$2
        shift 2
        ;;
    --open)
        open_after=yes
        shift
        ;;
    -h | --help)
        usage
        exit 0
        ;;
    *) fail "unknown argument $1" ;;
    esac
done

[ "$(uname -s)" = Darwin ] || fail "this installs the Mac app; on $(uname -s) run target/release/launcher itself"

root=$(cd "$(dirname "$0")/.." && pwd)
icon_source="$root/launcher/assets/icon-1024.png"
[ -f "$icon_source" ] || fail "the icon is missing: $icon_source"

if [ -z "$binary" ]; then
    command -v cargo >/dev/null 2>&1 || fail "cargo is not on PATH; build the launcher and pass --binary"
    (cd "$root" && cargo build --release -p loadngo-launcher)
    binary="$root/target/release/launcher"
fi
[ -x "$binary" ] || fail "not an executable: $binary"

if [ -z "$to" ]; then
    if [ -w /Applications ]; then
        to=/Applications
    else
        to="$HOME/Applications"
    fi
fi
mkdir -p "$to"
[ -w "$to" ] || fail "cannot write to $to"

name="loadngo Launcher"
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/launcher/Cargo.toml" | head -1)
build=$(git -C "$root" rev-list --count HEAD 2>/dev/null || echo 1)

# Built beside its destination and moved into place whole, so a failed
# install leaves the app that was there.
staging=$(mktemp -d "$to/.loadngo-launcher.XXXXXX")
trap 'rm -rf "$staging"' EXIT
app="$staging/$name.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
install -m 755 "$binary" "$app/Contents/MacOS/launcher"

iconset="$staging/AppIcon.iconset"
mkdir "$iconset"
for size in 16 32 128 256 512; do
    double=$((size * 2))
    sips -z "$size" "$size" "$icon_source" --out "$iconset/icon_${size}x${size}.png" >/dev/null
    sips -z "$double" "$double" "$icon_source" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/AppIcon.icns"
rm -rf "$iconset"

cat >"$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleExecutable</key><string>launcher</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundleIdentifier</key><string>com.loadngo.launcher</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>$name</string>
  <key>CFBundleDisplayName</key><string>$name</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${version:-0.1.0}</string>
  <key>CFBundleVersion</key><string>$build</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
EOF
printf 'APPL????' >"$app/Contents/PkgInfo"

codesign --force --sign - --timestamp=none "$app" >/dev/null 2>&1 || fail "codesign refused the app"
codesign --verify --strict "$app" || fail "the signed app does not verify"

installed="$to/$name.app"
rm -rf "$installed"
mv "$app" "$installed"
# Tell the system's app database now, so Spotlight and Launchpad show it at
# once and with this icon.
lsregister=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
[ -x "$lsregister" ] && "$lsregister" -f "$installed" || true

echo "installed $installed (version ${version:-0.1.0}, build $build)"
if [ "$open_after" = yes ]; then
    open "$installed"
fi
