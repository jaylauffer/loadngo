#!/usr/bin/env bash
# Builds loadngo's launcher for an iPhone, signs it, installs it on the
# attached phone (or in the iOS Simulator), sends it its list, and starts it.
# Run on a Mac, from a loadngo checkout. What the launcher is and does on a
# phone: docs/LAUNCHER.md, "On an iPhone".

set -euo pipefail

usage() {
  cat <<'EOF'
install-launcher-ios.sh - put loadngo's launcher on an iPhone

Usage:
  install-launcher-ios.sh [--simulator] [--device UDID] [--profile FILE]
                          [--identity SHA1] [--bundle-id ID] [--catalog FILE]
                          [--catalog-only] [--no-start]

With no options: builds the launcher for the iPhone, signs it with the
provisioning profile it finds for its bundle id, installs it on the one
attached iPhone that profile names, sends the list, and starts it.

  --simulator      optional  Use the iOS Simulator instead of a phone: the
                             booted simulator, or the first iPhone one, which
                             is booted. Needs no profile and no signing
                             identity.
  --device UDID    optional  The phone to install on, when more than one that
                             the profile names is attached. With --simulator,
                             the simulator's identifier.
  --profile FILE   optional  The .mobileprovision to sign with. Default: the
                             newest unexpired one for the bundle id among
                             ~/Library/Developer/Xcode/UserData/Provisioning Profiles
                             and ~/Library/MobileDevice/Provisioning Profiles.
  --identity SHA1  optional  The signing certificate, by its SHA-1 as
                             `security find-identity -v -p codesigning` lists
                             it. Default: the certificate the profile names
                             that this Mac's keychain holds.
  --bundle-id ID   optional  Default: com.jarraya.games.loadngo, the id Jay's
                             profiles are made for.
  --catalog FILE   optional  The list to send, a catalog.json. Default:
                             iphone-catalog.json in
                             ~/Library/Application Support/loadngo-launcher,
                             if there is one. With neither, the phone keeps
                             the list it has.
  --catalog-only   optional  Send the list to a launcher that is already
                             installed; build and install nothing. Press
                             Reload on the phone to read it.
  --no-start       optional  Do not start the launcher afterwards.
  -h, --help       optional  Print this help.

A phone opens addresses only, so its list holds links: a web page, or an app
by a scheme that app declares.
  { "entries": [ { "name": "SNG Mahjong", "group": "Games", "open": "sng-mahjong://" } ] }

A profile comes from Xcode and lasts seven days on a free Apple team: a new
iOS App project with the bundle id above, the phone as its run destination,
and Xcode's automatic signing writes one. When there is none, or it has
expired, this script says so and stops.

The first time, and again whenever the phone has dropped its trust in the
developer, iOS will not start the launcher until that is given on the phone:
Settings, General, VPN & Device Management, the entry under Developer App,
Trust. The script says so when that is what stopped it (exit 4); the
launcher and its list are on the phone by then.

Examples:
  scripts/install-launcher-ios.sh
  scripts/install-launcher-ios.sh --catalog ~/phone-list.json
  scripts/install-launcher-ios.sh --catalog-only
  scripts/install-launcher-ios.sh --simulator
EOF
}

fail() {
  echo "install-launcher-ios.sh: $1" >&2
  exit "${2:-1}"
}

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUNDLE_ID="com.jarraya.games.loadngo"
APP_NAME="loadngo Launcher"
HOME_SCREEN_NAME="loadngo"
BIN_NAME="launcher"
# Where the launcher keeps its list inside its own container.
CATALOG_ON_DEVICE="Library/Application Support/loadngo-launcher/catalog.json"
DEFAULT_CATALOG="$HOME/Library/Application Support/loadngo-launcher/iphone-catalog.json"
DEPLOYMENT_TARGET="${IPHONEOS_DEPLOYMENT_TARGET:-16.0}"

simulator=0
device=""
profile=""
identity=""
catalog=""
catalog_only=0
start=1

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    --simulator) simulator=1 ;;
    --catalog-only) catalog_only=1 ;;
    --no-start) start=0 ;;
    --device|--profile|--identity|--bundle-id|--catalog)
      [[ $# -ge 2 ]] || fail "$1 needs a value; see --help" 2
      case "$1" in
        --device) device="$2" ;;
        --profile) profile="$2" ;;
        --identity) identity="$2" ;;
        --bundle-id) BUNDLE_ID="$2" ;;
        --catalog) catalog="$2" ;;
      esac
      shift ;;
    *) fail "unknown argument $1; see --help" 2 ;;
  esac
  shift
done

[[ "$(uname -s)" == "Darwin" ]] || fail "an iPhone build needs a Mac with Xcode"
for tool in xcrun codesign security plutil python3; do
  command -v "$tool" >/dev/null 2>&1 || fail "$tool is needed (install Xcode)"
done

work="$(mktemp -d "${TMPDIR:-/tmp}/loadngo-launcher-ios.XXXXXX")"
trap 'rm -rf "$work"' EXIT

# ---- The list -------------------------------------------------------------

if [[ -z "$catalog" && -f "$DEFAULT_CATALOG" ]]; then
  catalog="$DEFAULT_CATALOG"
fi
if [[ -n "$catalog" ]]; then
  [[ -f "$catalog" ]] || fail "no such list: $catalog" 2
  python3 -c 'import json, sys; json.load(open(sys.argv[1]))' "$catalog" 2>"$work/json-error" \
    || fail "$catalog is not JSON: $(tail -n 1 "$work/json-error")" 2
elif [[ "$catalog_only" == 1 ]]; then
  fail "--catalog-only needs a list: --catalog FILE, or $DEFAULT_CATALOG" 2
fi

# ---- The simulator --------------------------------------------------------

if [[ "$simulator" == 1 ]]; then
  if [[ -z "$device" ]]; then
    device="$(xcrun simctl list devices booted | awk -F '[()]' '/Booted/ { print $2; exit }')"
  fi
  if [[ -z "$device" ]]; then
    device="$(xcrun simctl list devices available | awk -F '[()]' '/iPhone/ { print $2; exit }')"
    [[ -n "$device" ]] || fail "no iPhone simulator is installed (Xcode, Settings, Components)"
  fi
  xcrun simctl bootstatus "$device" -b >/dev/null
fi

send_catalog() {
  [[ -n "$catalog" ]] || return 0
  if [[ "$simulator" == 1 ]]; then
    local container
    container="$(xcrun simctl get_app_container "$device" "$BUNDLE_ID" data)"
    mkdir -p "$container/$(dirname "$CATALOG_ON_DEVICE")"
    cp "$catalog" "$container/$CATALOG_ON_DEVICE"
  else
    # devicectl names the copy after its source, so the source is called
    # what the launcher looks for.
    mkdir -p "$work/send"
    cp "$catalog" "$work/send/catalog.json"
    xcrun devicectl device copy to --device "$device" \
      --domain-type appDataContainer --domain-identifier "$BUNDLE_ID" \
      --source "$work/send/catalog.json" --destination "$CATALOG_ON_DEVICE" --quiet
  fi
  echo "Sent the list: $catalog"
}

start_launcher() {
  [[ "$start" == 1 ]] || return 0
  if [[ "$simulator" == 1 ]]; then
    xcrun simctl launch --terminate-running-process "$device" "$BUNDLE_ID" >/dev/null
  elif ! xcrun devicectl device process launch --device "$device" --terminate-existing \
      "$BUNDLE_ID" >"$work/launch.log" 2>&1; then
    if grep -q "has not been explicitly trusted" "$work/launch.log"; then
      echo "It is on the phone, but iOS will not start it until the developer is trusted there:
  Settings, General, VPN & Device Management, the entry under Developer App, Trust.
Then tap \"$HOME_SCREEN_NAME\" on the phone, or run this again with --catalog-only." >&2
      exit 4
    fi
    cat "$work/launch.log" >&2
    fail "the phone would not start the launcher"
  fi
  echo "Started the launcher."
}

# ---- The profile, the certificate and the phone ---------------------------

# Prints "expiry<TAB>path" for each unexpired profile that covers BUNDLE_ID.
profiles_for_bundle() {
  local folder file decoded app_id expiry now
  now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  for folder in "$HOME/Library/Developer/Xcode/UserData/Provisioning Profiles" \
                "$HOME/Library/MobileDevice/Provisioning Profiles"; do
    [[ -d "$folder" ]] || continue
    while IFS= read -r -d '' file; do
      decoded="$work/profile.plist"
      security cms -D -i "$file" >"$decoded" 2>/dev/null || continue
      app_id="$(plutil -extract Entitlements.application-identifier raw "$decoded" 2>/dev/null)" || continue
      expiry="$(plutil -extract ExpirationDate raw "$decoded" 2>/dev/null)" || continue
      # TEAMID.bundle.id, or a team's wildcard TEAMID.*
      case "${app_id#*.}" in
        "$BUNDLE_ID"|"*") ;;
        *) continue ;;
      esac
      [[ "$expiry" > "$now" ]] || continue
      printf '%s\t%s\n' "$expiry" "$file"
    done < <(find "$folder" -maxdepth 1 -name '*.mobileprovision' -print0)
  done
}

if [[ "$simulator" == 0 ]]; then
  if [[ -z "$profile" ]]; then
    profile="$(profiles_for_bundle | sort -r | head -n 1 | cut -f 2-)"
    [[ -n "$profile" ]] || fail "no unexpired provisioning profile for $BUNDLE_ID on this Mac.
Make one in Xcode: File, New, Project, iOS App; set its Bundle Identifier to
$BUNDLE_ID; pick the phone as the run destination and let Xcode sign it. The
profile it writes lasts seven days on a free Apple team. Then run this again." 3
  fi
  [[ -f "$profile" ]] || fail "no such profile: $profile" 2
  security cms -D -i "$profile" >"$work/chosen.plist" 2>/dev/null \
    || fail "$profile is not a provisioning profile" 2
  expiry="$(plutil -extract ExpirationDate raw "$work/chosen.plist")"
  [[ "$expiry" > "$(date -u +%Y-%m-%dT%H:%M:%SZ)" ]] || fail "the profile expired on $expiry: $profile" 3
  app_id="$(plutil -extract Entitlements.application-identifier raw "$work/chosen.plist")"
  case "${app_id#*.}" in
    "$BUNDLE_ID"|"*") ;;
    *) fail "the profile is for ${app_id#*.}, not $BUNDLE_ID (see --bundle-id): $profile" 2 ;;
  esac
  /usr/libexec/PlistBuddy -x -c "Print :Entitlements" "$work/chosen.plist" >"$work/entitlements.plist"
  if [[ "${app_id#*.}" == "*" ]]; then
    # A wildcard profile: the app is signed as itself.
    plutil -replace application-identifier -string "${app_id%%.*}.$BUNDLE_ID" "$work/entitlements.plist"
  fi

  if [[ -z "$identity" ]]; then
    # Two certificates here share one display name, so a name cannot pick
    # one. The profile lists the certificates it accepts; take the one this
    # keychain can sign with.
    held="$(security find-identity -v -p codesigning | awk '{ print $2 }')"
    index=0
    while plutil -extract "DeveloperCertificates.$index" raw "$work/chosen.plist" >"$work/cert.b64" 2>/dev/null; do
      sha="$(base64 -D <"$work/cert.b64" | shasum -a 1 | awk '{ print toupper($1) }')"
      if grep -qx "$sha" <<<"$held"; then
        identity="$sha"
        break
      fi
      index=$((index + 1))
    done
    [[ -n "$identity" ]] || fail "this Mac's keychain holds none of the certificates the profile names: $profile" 3
  fi

  xcrun devicectl list devices --json-output "$work/devices.json" >/dev/null 2>&1 \
    || fail "devicectl could not list devices (is Xcode installed?)"
  # Attached phones the profile names: "udid<TAB>name" each.
  attached="$(python3 - "$work/devices.json" "$work/chosen.plist" <<'PY'
import json, plistlib, sys
devices = json.load(open(sys.argv[1]))["result"]["devices"]
named = set(plistlib.load(open(sys.argv[2], "rb")).get("ProvisionedDevices", []))
for device in devices:
    udid = device.get("hardwareProperties", {}).get("udid", "")
    state = device.get("connectionProperties", {}).get("tunnelState", "")
    paired = device.get("connectionProperties", {}).get("pairingState", "")
    if udid in named and paired == "paired" and state != "unavailable":
        print(udid + "\t" + device.get("deviceProperties", {}).get("name", "?"))
PY
)"
  if [[ -z "$device" ]]; then
    count="$(grep -c . <<<"$attached" || true)"
    [[ "$count" -ge 1 ]] || fail "no attached iPhone is named in the profile. Attach and unlock the phone; if it is attached, the profile was made for another device." 3
    [[ "$count" -eq 1 ]] || fail "more than one attached iPhone is in the profile; pick one with --device:
$attached" 2
    device="$(cut -f 1 <<<"$attached")"
  fi
  echo "Phone: $(grep "^$device" <<<"$attached" | cut -f 2- || true) ($device)"
  echo "Profile: $(plutil -extract Name raw "$work/chosen.plist"), expires $expiry"
fi

if [[ "$catalog_only" == 1 ]]; then
  send_catalog
  start_launcher
  exit 0
fi

# ---- Build ----------------------------------------------------------------

if [[ "$simulator" == 1 ]]; then
  target="aarch64-apple-ios-sim"
  [[ "$(uname -m)" == "arm64" ]] || target="x86_64-apple-ios"
  platform="iPhoneSimulator"
else
  target="aarch64-apple-ios"
  platform="iPhoneOS"
fi
command -v cargo >/dev/null 2>&1 || fail "cargo is needed to build the launcher"
rustup target list --installed 2>/dev/null | grep -qx "$target" || rustup target add "$target"

export IPHONEOS_DEPLOYMENT_TARGET="$DEPLOYMENT_TARGET"
(cd "$ROOT" && cargo build --release --locked -p loadngo-launcher --bin "$BIN_NAME" --target "$target")
binary="$ROOT/target/$target/release/$BIN_NAME"
[[ -f "$binary" ]] || fail "the build left no $binary"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/launcher/Cargo.toml" | head -n 1)"

# ---- The app --------------------------------------------------------------

app="$ROOT/target/ios-launcher/$target/$APP_NAME.app"
rm -rf "$app"
mkdir -p "$app"
cp "$binary" "$app/$BIN_NAME"

icons=""
for spec in "Icon-60@2x:120" "Icon-60@3x:180" "Icon-76@2x:152" "Icon-83.5@2x:167" "Icon-1024:1024"; do
  name="${spec%%:*}"
  sips -z "${spec##*:}" "${spec##*:}" "$ROOT/launcher/assets/icon-ios-1024.png" \
    --out "$app/$name.png" >/dev/null
  icons="$icons<string>$name</string>"
done

cat >"$app/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleExecutable</key><string>$BIN_NAME</string>
  <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
  <key>CFBundleName</key><string>$APP_NAME</string>
  <key>CFBundleDisplayName</key><string>$HOME_SCREEN_NAME</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>CFBundleSupportedPlatforms</key><array><string>$platform</string></array>
  <key>LSRequiresIPhoneOS</key><true/>
  <key>MinimumOSVersion</key><string>$DEPLOYMENT_TARGET</string>
  <key>UIDeviceFamily</key><array><integer>1</integer><integer>2</integer></array>
  <!-- An empty launch screen: the whole display is the app's, with no
       storyboard to compile. -->
  <key>UILaunchScreen</key><dict/>
  <key>UISupportedInterfaceOrientations</key>
  <array>
    <string>UIInterfaceOrientationPortrait</string>
    <string>UIInterfaceOrientationLandscapeLeft</string>
    <string>UIInterfaceOrientationLandscapeRight</string>
  </array>
  <key>CFBundleIcons</key>
  <dict>
    <key>CFBundlePrimaryIcon</key>
    <dict>
      <key>CFBundleIconFiles</key><array>$icons</array>
    </dict>
  </dict>
</dict>
</plist>
EOF
plutil -lint "$app/Info.plist" >/dev/null || fail "the Info.plist it wrote does not parse"

if [[ "$simulator" == 1 ]]; then
  codesign --force --sign - --timestamp=none "$app" 2>/dev/null
else
  cp "$profile" "$app/embedded.mobileprovision"
  codesign --force --sign "$identity" --timestamp=none \
    --entitlements "$work/entitlements.plist" "$app"
  codesign --verify --strict "$app"
fi
echo "Built $app (launcher $version)"

# ---- Install, send the list, start ----------------------------------------

if [[ "$simulator" == 1 ]]; then
  xcrun simctl install "$device" "$app"
else
  xcrun devicectl device install app --device "$device" "$app" --quiet
fi
echo "Installed."
send_catalog
start_launcher
