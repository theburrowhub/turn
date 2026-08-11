#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "release-macos: macOS is required" >&2
    exit 1
fi
if [[ -z "${TURN_CODESIGN_IDENTITY:-}" || "$TURN_CODESIGN_IDENTITY" == "-" ]]; then
    echo "release-macos: TURN_CODESIGN_IDENTITY must name a Developer ID Application identity" >&2
    exit 1
fi
if [[ -z "${TURN_NOTARY_PROFILE:-}" && \
      ( -z "${TURN_NOTARY_KEY:-}" || -z "${TURN_NOTARY_KEY_ID:-}" || -z "${TURN_NOTARY_ISSUER:-}" ) ]]; then
    echo "release-macos: notarization credentials are required" >&2
    exit 1
fi
if [[ -z "${TURN_RELEASE_BASE_URL:-}" || "$TURN_RELEASE_BASE_URL" != https://* ]]; then
    echo "release-macos: TURN_RELEASE_BASE_URL must be the HTTPS release-asset directory" >&2
    exit 1
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"
output_dir="${1:-$repo_root/dist}"
if [[ "$output_dir" != /* ]]; then
    output_dir="$PWD/$output_dir"
fi
mkdir -p "$output_dir"

cargo_bin="${CARGO:-cargo}"
package_id="$("$cargo_bin" pkgid --manifest-path "$repo_root/Cargo.toml" --locked -p turn-gui)"
version="${package_id##*#}"
version="${version##*@}"
if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]]; then
    echo "release-macos: invalid workspace version: $version" >&2
    exit 1
fi
if [[ -n "${TURN_RELEASE_TAG:-}" && "$TURN_RELEASE_TAG" != "v$version" ]]; then
    echo "release-macos: tag $TURN_RELEASE_TAG does not match workspace version $version" >&2
    exit 1
fi

channel="${TURN_UPDATE_CHANNEL:-stable}"
architecture="$(uname -m)"
archive_name="Turn-$version-macos-$architecture.zip"
archive="$output_dir/$archive_name"
channel_manifest="$output_dir/turn-$channel-$architecture.plist"
for path in "$archive" "$channel_manifest"; do
    if [[ -e "$path" ]]; then
        echo "release-macos: refusing to replace existing artifact: $path" >&2
        exit 1
    fi
done

app="$output_dir/.Turn-$version-$architecture.app"
if [[ -e "$app" ]]; then
    echo "release-macos: stale staging app exists: $app" >&2
    exit 1
fi
cleanup() {
    rm -rf "$app"
}
trap cleanup EXIT

TURN_REQUIRE_NOTARIZATION=1 \
TURN_UPDATE_CHANNEL="$channel" \
    "$script_dir/package-macos-app.sh" "$app"
TURN_REQUIRE_NOTARIZATION=1 \
TURN_EXPECT_VERSION="$version" \
    "$script_dir/verify-macos-app.sh" "$app"

protocol_min="$(plutil -extract protocol_min raw -o - "$app/Contents/Resources/release.plist")"
protocol_max="$(plutil -extract protocol_max raw -o - "$app/Contents/Resources/release.plist")"
ditto -c -k --sequesterRsrc --keepParent "$app" "$archive"
archive_sha256="$(shasum -a 256 "$archive" | awk '{print $1}')"
archive_size="$(stat -f '%z' "$archive")"
artifact_url="${TURN_RELEASE_BASE_URL%/}/$archive_name"

cat > "$channel_manifest" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>schema</key><integer>1</integer>
  <key>channel</key><string>$channel</string>
  <key>version</key><string>$version</string>
  <key>bundle_id</key><string>io.github.theburrowhub.turn</string>
  <key>minimum_macos</key><string>13.0</string>
  <key>architecture</key><string>$architecture</string>
  <key>protocol_min</key><integer>$protocol_min</integer>
  <key>protocol_max</key><integer>$protocol_max</integer>
  <key>artifact</key>
  <dict>
    <key>url</key><string>$artifact_url</string>
    <key>sha256</key><string>$archive_sha256</string>
    <key>size</key><integer>$archive_size</integer>
  </dict>
</dict>
</plist>
EOF
plutil -lint "$channel_manifest"

trap - EXIT
rm -rf "$app"
echo "release-macos: wrote $archive"
echo "release-macos: wrote $channel_manifest"
