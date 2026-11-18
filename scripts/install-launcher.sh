#!/bin/sh
# Installs loadngo's launcher for the current user, so it is started like any
# other program:
#   macOS  the app "loadngo Launcher": Spotlight (Cmd-Space, "loadngo"),
#          Launchpad or the Dock.
#   Linux  "loadngo Launcher" in the desktop's menu (a .desktop entry with
#          its icon), the binary as ~/.local/bin/loadngo-launcher.
#
#   install-launcher.sh [--release [TAG] | --binary PATH] [--to FOLDER] [--open]
#
#   --release [TAG]  optional  Install a published build: fetch it from
#                              loadngo's releases on GitHub (the newest
#                              launcher release, or TAG, such as
#                              launcher-v0.1.0), check it against that
#                              release's SHA256SUMS, and install it. Needs no
#                              checkout and no Rust; this script is itself one
#                              of the release's files (see the last example).
#   --binary PATH    optional  The built launcher to install.
#                              With neither --release nor --binary, the script
#                              builds target/release/launcher in its checkout
#                              first (cargo build --release -p loadngo-launcher).
#   --to FOLDER      optional  macOS: where the app goes. Default
#                              /Applications, or ~/Applications when that
#                              cannot be written. Not used on Linux.
#   --open           optional  Start the launcher once it is installed.
#   -h, --help       optional  Print this help.
#
# What is installed is a copy: run the script again to replace it. A launcher
# that is open keeps running the old copy until it is quit. On macOS the app
# is signed ad hoc for this Mac.
#
# What the launcher lists is not part of it and is never touched: catalog.json
# in ~/Library/Application Support/loadngo-launcher (macOS) or
# ~/.local/share/loadngo-launcher (Linux); see docs/LAUNCHER.md.
#
# Undo, macOS: move "loadngo Launcher" from Applications to the Trash.
# Undo, Linux: rm ~/.local/bin/loadngo-launcher
#   ~/.local/share/applications/loadngo-launcher.desktop
#   ~/.local/share/icons/hicolor/*/apps/loadngo-launcher.png
#
# Examples:
#   scripts/install-launcher.sh --open
#   scripts/install-launcher.sh --release
#   curl -fsSL https://github.com/jaylauffer/loadngo/releases/download/launcher-v0.1.0/install-launcher.sh | sh -s -- --release launcher-v0.1.0
set -eu

# Where releases are published. LOADNGO_REPO names another GitHub repository
# (a fork) to fetch from.
repo=${LOADNGO_REPO:-jaylauffer/loadngo}

usage() {
    # Run from a file, the help is this file's own heading; piped in, it is
    # the one line that fits.
    if [ -f "$0" ]; then
        sed -n '2,44p' "$0" | sed 's/^# \{0,1\}//'
    else
        echo "install-launcher.sh [--release [TAG] | --binary PATH] [--to FOLDER] [--open]; see docs/LAUNCHER.md"
    fi
}
fail() {
    echo "install-launcher: $1; see --help" >&2
    exit 2
}

binary=
release=no
tag=
to=
open_after=no
while [ $# -gt 0 ]; do
    case "$1" in
    --binary)
        [ $# -ge 2 ] || fail "--binary needs a path"
        binary=$2
        shift 2
        ;;
    --release)
        release=yes
        shift
        # A tag may follow; anything starting with a dash is the next option.
        if [ $# -gt 0 ]; then
            case "$1" in
            -*) ;;
            *)
                tag=$1
                shift
                ;;
            esac
        fi
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
[ "$release" = no ] || [ -z "$binary" ] || fail "--release and --binary are two ways to say what to install; give one"

system=$(uname -s)
case "$system" in
Darwin) os=macos ;;
Linux) os=linux ;;
*) fail "this installs on macOS and Linux; on $system run the launcher binary as it is" ;;
esac
case "$(uname -m)" in
arm64 | aarch64) arch=aarch64 ;;
x86_64 | amd64) arch=x86_64 ;;
*) arch=$(uname -m) ;;
esac
# The Mac build is named for Apple's own word for the same processors.
[ "$os" = macos ] && [ "$arch" = aarch64 ] && arch=arm64

# The folder this script is in, when it is run from a file: a checkout's
# scripts/, or an unpacked release.
here=
if [ -f "$0" ]; then
    here=$(cd "$(dirname "$0")" && pwd)
fi

work=
cleanup() { [ -z "$work" ] || rm -rf "$work"; }
trap cleanup EXIT

fetch() {
    # fetch URL FILE
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --retry 3 -o "$2" "$1"
    elif command -v wget >/dev/null 2>&1; then
        wget -q -O "$2" "$1"
    else
        fail "neither curl nor wget is on PATH"
    fi
}
sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

assets=
version=
commit=
if [ "$release" = yes ]; then
    work=$(mktemp -d "${TMPDIR:-/tmp}/loadngo-launcher.XXXXXX")
    if [ -z "$tag" ]; then
        # The newest release whose tag is the launcher's (the repository may
        # publish other things): GitHub lists releases newest first.
        fetch "https://api.github.com/repos/$repo/releases?per_page=50" "$work/releases.json" || fail "could not list $repo's releases"
        tag=$(grep -o '"tag_name": *"launcher-v[^"]*"' "$work/releases.json" | head -1 | sed 's/.*"\(launcher-v[^"]*\)"/\1/')
        [ -n "$tag" ] || fail "$repo has no launcher release yet"
    fi
    version=${tag#launcher-v}
    name="loadngo-launcher-$version-$os-$arch"
    base="https://github.com/$repo/releases/download/$tag"
    echo "fetching $name.tar.gz from $repo's release $tag"
    fetch "$base/$name.tar.gz" "$work/$name.tar.gz" || fail "release $tag has no build for $os $arch ($name.tar.gz)"
    fetch "$base/SHA256SUMS" "$work/SHA256SUMS" || fail "release $tag has no SHA256SUMS"
    want=$(grep " \*\{0,1\}$name.tar.gz\$" "$work/SHA256SUMS" | cut -d' ' -f1)
    [ -n "$want" ] || fail "SHA256SUMS does not list $name.tar.gz"
    have=$(sha256_of "$work/$name.tar.gz")
    [ "$want" = "$have" ] || fail "$name.tar.gz does not match the release's SHA256SUMS (got $have, the release says $want)"
    tar -xzf "$work/$name.tar.gz" -C "$work"
    binary="$work/$name/launcher"
    assets="$work/$name/assets"
    [ -f "$work/$name/VERSION" ] && commit=$(sed -n '2p' "$work/$name/VERSION")
elif [ -n "$here" ] && [ -f "$here/assets/icon-1024.png" ]; then
    # Run from an unpacked release: its own binary unless another was named.
    assets="$here/assets"
    [ -n "$binary" ] || binary="$here/launcher"
    if [ -f "$here/VERSION" ]; then
        version=$(sed -n '1p' "$here/VERSION")
        commit=$(sed -n '2p' "$here/VERSION")
    fi
else
    [ -n "$here" ] || fail "piped in, the script can only install a release: add --release"
    root=$(cd "$here/.." && pwd)
    assets="$root/launcher/assets"
    version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/launcher/Cargo.toml" | head -1)
    commit=$(git -C "$root" rev-parse HEAD 2>/dev/null || true)
    if [ -z "$binary" ]; then
        command -v cargo >/dev/null 2>&1 || fail "cargo is not on PATH; build the launcher and pass --binary, or install a published build with --release"
        (cd "$root" && cargo build --release -p loadngo-launcher)
        binary="$root/target/release/launcher"
    fi
fi
[ -x "$binary" ] || fail "not an executable: $binary"
[ -f "$assets/icon-1024.png" ] || fail "the icon is missing: $assets/icon-1024.png"
version=${version:-0.1.0}
name="loadngo Launcher"

if [ "$os" = linux ]; then
    data=${XDG_DATA_HOME:-$HOME/.local/share}
    installed="$HOME/.local/bin/loadngo-launcher"
    mkdir -p "$HOME/.local/bin" "$data/applications"
    # Copied beside its destination and moved over it, so a launcher that is
    # open keeps the file it runs from, and a failed copy leaves the old one.
    install -m 755 "$binary" "$installed.new"
    mv -f "$installed.new" "$installed"
    for size in 48 64 128 256; do
        [ -f "$assets/icon-$size.png" ] || continue
        mkdir -p "$data/icons/hicolor/${size}x${size}/apps"
        install -m 644 "$assets/icon-$size.png" "$data/icons/hicolor/${size}x${size}/apps/loadngo-launcher.png"
    done
    # The entry's file name is the window's app id (launcher/src/main.rs
    # APP_ID), which is how a Wayland desktop gives the open window this icon.
    entry="$data/applications/loadngo-launcher.desktop"
    cat >"$entry.new" <<EOF
[Desktop Entry]
Type=Application
Version=1.0
Name=$name
GenericName=Launcher
Comment=Lists what this machine can start, and starts it
Exec=$installed
Icon=loadngo-launcher
Terminal=false
Categories=Utility;
StartupWMClass=loadngo-launcher
X-Loadngo-Version=$version
EOF
    mv -f "$entry.new" "$entry"
    # Both are optional helpers: a menu reads the folders itself without them.
    command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$data/applications" >/dev/null 2>&1 || true
    command -v gtk-update-icon-cache >/dev/null 2>&1 && [ -f "$data/icons/hicolor/index.theme" ] && gtk-update-icon-cache -q "$data/icons/hicolor" >/dev/null 2>&1 || true
    echo "installed $installed (version $version${commit:+, commit $(printf %.8s "$commit")}); \"$name\" is in the desktop's menu"
    if [ "$open_after" = yes ]; then
        if [ -n "${WAYLAND_DISPLAY:-}${DISPLAY:-}" ]; then
            nohup "$installed" >/dev/null 2>&1 &
        else
            echo "no desktop session in this shell: start \"$name\" from the menu"
        fi
    fi
    exit 0
fi

# macOS: the app.
if [ -z "$to" ]; then
    if [ -w /Applications ]; then
        to=/Applications
    else
        to="$HOME/Applications"
    fi
fi
mkdir -p "$to"
[ -w "$to" ] || fail "cannot write to $to"
icon_source="$assets/icon-1024.png"
build=1
if [ -z "$work" ] && [ -n "$here" ]; then
    build=$(git -C "$here" rev-list --count HEAD 2>/dev/null || echo 1)
fi

# Built beside its destination and moved into place whole, so a failed
# install leaves the app that was there.
staging=$(mktemp -d "$to/.loadngo-launcher.XXXXXX")
cleanup() {
    rm -rf "$staging"
    [ -z "$work" ] || rm -rf "$work"
}
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
  <key>CFBundleShortVersionString</key><string>$version</string>
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

echo "installed $installed (version $version, build $build)"
if [ "$open_after" = yes ]; then
    open "$installed"
fi
