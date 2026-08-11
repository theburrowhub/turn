#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "package-macos-app: macOS is required" >&2
    exit 1
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"
invocation_dir="$PWD"
output="${1:-$repo_root/dist/Turn.app}"

if [[ "$output" != *.app ]]; then
    echo "package-macos-app: output must end in .app: $output" >&2
    exit 1
fi
if [[ "$output" != /* ]]; then
    output="$invocation_dir/$output"
fi
if [[ -e "$output" ]]; then
    echo "package-macos-app: refusing to replace existing output: $output" >&2
    exit 1
fi

cargo_bin="${CARGO:-cargo}"
target_dir="${CARGO_TARGET_DIR:-$repo_root/target}"
if [[ "$target_dir" != /* ]]; then
    target_dir="$invocation_dir/$target_dir"
fi

"$cargo_bin" build \
    --manifest-path "$repo_root/Cargo.toml" \
    --locked \
    --release \
    --bin turn \
    --bin turnd \
    --bin turn-hook

for binary in turn turnd turn-hook; do
    if [[ ! -x "$target_dir/release/$binary" ]]; then
        echo "package-macos-app: missing release binary: $target_dir/release/$binary" >&2
        exit 1
    fi
done

package_id="$("$cargo_bin" pkgid \
    --manifest-path "$repo_root/Cargo.toml" \
    --locked \
    -p turn-gui)"
version="${package_id##*#}"
version="${version##*@}"
if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]]; then
    echo "package-macos-app: could not resolve the workspace version" >&2
    exit 1
fi

field() {
    local line="$1"
    local wanted="$2"
    local token
    for token in $line; do
        if [[ "$token" == "$wanted="* ]]; then
            printf '%s\n' "${token#*=}"
            return 0
        fi
    done
    return 1
}

turn_info="$($target_dir/release/turn --build-info)"
daemon_info="$($target_dir/release/turnd --build-info)"
hook_info="$($target_dir/release/turn-hook --build-info)"
for tuple in \
    "$turn_info|turn" \
    "$daemon_info|turnd" \
    "$hook_info|turn-hook"; do
    info="${tuple%|*}"
    expected_component="${tuple##*|}"
    component="$(field "$info" component || true)"
    binary_version="$(field "$info" version || true)"
    if [[ "$component" != "$expected_component" || "$binary_version" != "$version" ]]; then
        echo "package-macos-app: incompatible $expected_component build info: $info" >&2
        exit 1
    fi
done

protocol_min="$(field "$turn_info" protocol_min || true)"
protocol_max="$(field "$turn_info" protocol_max || true)"
daemon_protocol_min="$(field "$daemon_info" protocol_min || true)"
daemon_protocol_max="$(field "$daemon_info" protocol_max || true)"
if [[ ! "$protocol_min" =~ ^[0-9]+$ || ! "$protocol_max" =~ ^[0-9]+$ ]]; then
    echo "package-macos-app: the UI did not report a valid protocol window: $turn_info" >&2
    exit 1
fi
if [[ "$protocol_min" != "$daemon_protocol_min" || "$protocol_max" != "$daemon_protocol_max" ]]; then
    echo "package-macos-app: UI and daemon protocol windows differ" >&2
    echo "  $turn_info" >&2
    echo "  $daemon_info" >&2
    exit 1
fi

update_channel="${TURN_UPDATE_CHANNEL:-stable}"
if [[ ! "$update_channel" =~ ^[a-z0-9][a-z0-9.-]*$ ]]; then
    echo "package-macos-app: invalid update channel: $update_channel" >&2
    exit 1
fi

output_parent="$(dirname "$output")"
mkdir -p "$output_parent"
stage="$(mktemp -d "$output_parent/.turn-app.XXXXXX")"
reserved_output=false
cleanup() {
    rm -rf "$stage"
    if [[ "$reserved_output" == true ]]; then
        rmdir "$output" 2>/dev/null || true
    fi
}
trap cleanup EXIT
app="$stage/Turn.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"

install -m 0755 "$target_dir/release/turn" "$app/Contents/MacOS/turn"
install -m 0755 "$target_dir/release/turnd" "$app/Contents/MacOS/turnd"
install -m 0755 "$target_dir/release/turn-hook" "$app/Contents/MacOS/turn-hook"
install -m 0644 "$repo_root/crates/turn-gui/assets/turn-icon.icns" \
    "$app/Contents/Resources/turn-icon.icns"
install -m 0644 "$repo_root/packaging/macos/Info.plist" "$app/Contents/Info.plist"

plutil -replace CFBundleShortVersionString -string "$version" "$app/Contents/Info.plist"
plutil -replace CFBundleVersion -string "$version" "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist"

codesign_identity="${TURN_CODESIGN_IDENTITY:--}"
timestamp=(--timestamp=none)
if [[ "$codesign_identity" != "-" ]]; then
    timestamp=(--timestamp)
fi
for binary in turnd turn-hook; do
    codesign --force --sign "$codesign_identity" --options runtime "${timestamp[@]}" \
        "$app/Contents/MacOS/$binary"
done
codesign --force --sign "$codesign_identity" --options runtime "${timestamp[@]}" \
    --entitlements "$repo_root/packaging/macos/Turn.entitlements" \
    "$app/Contents/MacOS/turn"

daemon_sha256="$(shasum -a 256 "$app/Contents/MacOS/turnd" | awk '{print $1}')"
hook_sha256="$(shasum -a 256 "$app/Contents/MacOS/turn-hook" | awk '{print $1}')"
architecture="$(uname -m)"
cat > "$app/Contents/Resources/release.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>schema</key><integer>1</integer>
  <key>channel</key><string>$update_channel</string>
  <key>version</key><string>$version</string>
  <key>architecture</key><string>$architecture</string>
  <key>protocol_min</key><integer>$protocol_min</integer>
  <key>protocol_max</key><integer>$protocol_max</integer>
  <key>components</key>
  <dict>
    <key>turn</key><dict><key>version</key><string>$version</string></dict>
    <key>turnd</key><dict><key>version</key><string>$version</string><key>sha256</key><string>$daemon_sha256</string></dict>
    <key>turn-hook</key><dict><key>version</key><string>$version</string><key>sha256</key><string>$hook_sha256</string></dict>
  </dict>
</dict>
</plist>
EOF
plutil -lint "$app/Contents/Resources/release.plist"

codesign --force --sign "$codesign_identity" --options runtime "${timestamp[@]}" \
    --entitlements "$repo_root/packaging/macos/Turn.entitlements" "$app"
codesign --verify --deep --strict --verbose=2 "$app"

notarised=false
notary_archive="$stage/Turn-notary.zip"
if [[ -n "${TURN_NOTARY_PROFILE:-}" ]]; then
    ditto -c -k --keepParent "$app" "$notary_archive"
    xcrun notarytool submit "$notary_archive" \
        --keychain-profile "$TURN_NOTARY_PROFILE" --wait
    notarised=true
elif [[ -n "${TURN_NOTARY_KEY:-}" || -n "${TURN_NOTARY_KEY_ID:-}" || -n "${TURN_NOTARY_ISSUER:-}" ]]; then
    if [[ -z "${TURN_NOTARY_KEY:-}" || -z "${TURN_NOTARY_KEY_ID:-}" || -z "${TURN_NOTARY_ISSUER:-}" ]]; then
        echo "package-macos-app: TURN_NOTARY_KEY, TURN_NOTARY_KEY_ID and TURN_NOTARY_ISSUER must be set together" >&2
        exit 1
    fi
    ditto -c -k --keepParent "$app" "$notary_archive"
    xcrun notarytool submit "$notary_archive" \
        --key "$TURN_NOTARY_KEY" \
        --key-id "$TURN_NOTARY_KEY_ID" \
        --issuer "$TURN_NOTARY_ISSUER" \
        --wait
    notarised=true
fi

if [[ "$notarised" == true ]]; then
    xcrun stapler staple "$app"
    xcrun stapler validate "$app"
    codesign --verify --deep --strict --verbose=2 "$app"
    spctl --assess --type execute --verbose=2 "$app"
elif [[ "${TURN_REQUIRE_NOTARIZATION:-0}" == "1" ]]; then
    echo "package-macos-app: notarization is required but no notary credentials were supplied" >&2
    exit 1
fi

TURN_EXPECT_VERSION="$version" \
TURN_REQUIRE_NOTARIZATION="$([[ "$notarised" == true ]] && echo 1 || echo 0)" \
    "$script_dir/verify-macos-app.sh" "$app"

if ! mkdir "$output"; then
    echo "package-macos-app: refusing to replace existing output: $output" >&2
    exit 1
fi
reserved_output=true
mv "$app/Contents" "$output/Contents"
reserved_output=false
trap - EXIT
rm -rf "$stage"

echo "package-macos-app: wrote $output"
