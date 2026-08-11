#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "local-update-acceptance: macOS is required" >&2
    exit 1
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
app="${1:-}"
root="${2:-}"
if [[ ! -d "$app/Contents" || -z "$root" || "$root" != /* ]]; then
    echo "usage: local-update-acceptance.sh /path/to/Turn.app /absolute/test/root" >&2
    exit 2
fi
if [[ "$root" == *['&<>']* ]]; then
    echo "local-update-acceptance: test root contains XML metacharacters" >&2
    exit 2
fi
mkdir -p "$root/Applications"

release_plist="$app/Contents/Resources/release.plist"
version="$(plutil -extract version raw -o - "$release_plist")"
architecture="$(plutil -extract architecture raw -o - "$release_plist")"
protocol_min="$(plutil -extract protocol_min raw -o - "$release_plist")"
protocol_max="$(plutil -extract protocol_max raw -o - "$release_plist")"
archive="$root/Turn-$version-macos-$architecture.zip"
manifest="$root/turn-test-$architecture.plist"

ditto -c -k --sequesterRsrc --keepParent "$app" "$archive"
sha256="$(shasum -a 256 "$archive" | awk '{print $1}')"
size="$(stat -f '%z' "$archive")"
cat > "$manifest" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>schema</key><integer>1</integer>
  <key>channel</key><string>test</string>
  <key>version</key><string>$version</string>
  <key>bundle_id</key><string>io.github.theburrowhub.turn</string>
  <key>minimum_macos</key><string>13.0</string>
  <key>architecture</key><string>$architecture</string>
  <key>protocol_min</key><integer>$protocol_min</integer>
  <key>protocol_max</key><integer>$protocol_max</integer>
  <key>artifact</key>
  <dict>
    <key>url</key><string>$archive</string>
    <key>sha256</key><string>$sha256</string>
    <key>size</key><integer>$size</integer>
  </dict>
</dict>
</plist>
EOF
plutil -lint "$manifest" >/dev/null

TURN_ALLOW_ADHOC_UPDATE=1 \
TURN_SOCKET="$root/no-daemon.sock" \
    "$script_dir/install-macos-update.sh" "$manifest" "$root/Applications/Turn.app"
TURN_EXPECT_VERSION="$version" \
    "$script_dir/verify-macos-app.sh" "$root/Applications/Turn.app"

reinstall_error="$root/reinstall.err"
if TURN_ALLOW_ADHOC_UPDATE=1 \
   TURN_SOCKET="$root/no-daemon.sock" \
       "$script_dir/install-macos-update.sh" "$manifest" "$root/Applications/Turn.app" \
       >/dev/null 2>"$reinstall_error"; then
    echo "local-update-acceptance: the updater accepted the installed version again" >&2
    exit 1
fi
if ! grep -q "is not newer than installed $version" "$reinstall_error"; then
    echo "local-update-acceptance: same-version refusal was not actionable" >&2
    exit 1
fi

echo "local-update-acceptance: clean install succeeded and a same-version reinstall was refused"
