#!/usr/bin/env bash
# Stopgap: run one cargo test binary on a physical iPhone.
#
# Wraps the binary in a minimal signed .app under a borrowed bundle id,
# installs it (replacing whatever app owns that id), launches it with
# console capture and exits with the test binary's exit code. Not a test
# framework; see docs/ON_DEVICE_TESTING.md for what is missing.
#
#   cargo test -p <crate> --target aarch64-apple-ios --no-run
#   IOS_TEST_BUNDLE_ID=<id the profile covers> \
#   IOS_PROVISIONING_PROFILE=<path>.mobileprovision \
#   IOS_SIGN_IDENTITY=<SHA-1 of the signing identity> \
#   IOS_DEVICE_ID=<device UDID> \
#   scripts/ios-device-test.sh target/aarch64-apple-ios/debug/deps/<test-bin> [libtest args...]
set -euo pipefail

BIN="${1:?usage: ios-device-test.sh <test-binary> [libtest args...]}"
shift
BUNDLE_ID="${IOS_TEST_BUNDLE_ID:?set IOS_TEST_BUNDLE_ID}"
PROFILE="${IOS_PROVISIONING_PROFILE:?set IOS_PROVISIONING_PROFILE}"
IDENTITY="${IOS_SIGN_IDENTITY:?set IOS_SIGN_IDENTITY}"
DEVICE="${IOS_DEVICE_ID:?set IOS_DEVICE_ID}"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
APP="$WORK/DeviceTest.app"
mkdir -p "$APP"
cp "$BIN" "$APP/device-test"
cp "$PROFILE" "$APP/embedded.mobileprovision"
cat > "$APP/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
<key>CFBundleExecutable</key><string>device-test</string>
<key>CFBundleName</key><string>DeviceTest</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleVersion</key><string>1</string>
<key>CFBundleShortVersionString</key><string>1.0</string>
<key>CFBundleSupportedPlatforms</key><array><string>iPhoneOS</string></array>
<key>MinimumOSVersion</key><string>16.0</string>
<key>UIDeviceFamily</key><array><integer>1</integer></array>
<key>UIRequiredDeviceCapabilities</key><array><string>arm64</string></array>
</dict></plist>
EOF
security cms -D -i "$PROFILE" > "$WORK/profile.plist"
/usr/libexec/PlistBuddy -x -c "Print :Entitlements" "$WORK/profile.plist" > "$WORK/entitlements.plist"
codesign --force --sign "$IDENTITY" --entitlements "$WORK/entitlements.plist" --timestamp=none "$APP"
xcrun devicectl device install app --device "$DEVICE" "$APP" >/dev/null

OUT="$WORK/console.txt"
xcrun devicectl device process launch --device "$DEVICE" --console --terminate-existing \
  "$BUNDLE_ID" -- "$@" 2>&1 | tee "$OUT"
# devicectl itself exits 0 whatever the app returned; recover the code.
CODE="$(sed -n 's/.*terminated with the exit code \([0-9]*\).*/\1/p' "$OUT" | tail -1)"
exit "${CODE:-1}"
