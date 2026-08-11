#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "verify-macos-app: macOS is required" >&2
    exit 1
fi

app="${1:-}"
if [[ -z "$app" || ! -d "$app/Contents" ]]; then
    echo "usage: verify-macos-app.sh /path/to/Turn.app" >&2
    exit 2
fi
app="$(cd "$(dirname "$app")" && pwd)/$(basename "$app")"
manifest="$app/Contents/Resources/release.plist"
plist="$app/Contents/Info.plist"

for path in "$plist" "$manifest"; do
    if [[ ! -f "$path" ]]; then
        echo "verify-macos-app: missing $path" >&2
        exit 1
    fi
    plutil -lint "$path" >/dev/null
done
for binary in turn turnd turn-hook; do
    if [[ ! -x "$app/Contents/MacOS/$binary" ]]; then
        echo "verify-macos-app: missing executable $binary" >&2
        exit 1
    fi
done

extract() {
    plutil -extract "$1" raw -o - "$manifest"
}

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

bundle_id="$(plutil -extract CFBundleIdentifier raw -o - "$plist")"
plist_version="$(plutil -extract CFBundleShortVersionString raw -o - "$plist")"
manifest_version="$(extract version)"
if [[ "$bundle_id" != "io.github.theburrowhub.turn" ]]; then
    echo "verify-macos-app: unexpected bundle identifier: $bundle_id" >&2
    exit 1
fi
if [[ "$plist_version" != "$manifest_version" ]]; then
    echo "verify-macos-app: Info.plist and release manifest versions differ" >&2
    exit 1
fi
if [[ -n "${TURN_EXPECT_VERSION:-}" && "$manifest_version" != "$TURN_EXPECT_VERSION" ]]; then
    echo "verify-macos-app: expected $TURN_EXPECT_VERSION, found $manifest_version" >&2
    exit 1
fi

turn_info="$($app/Contents/MacOS/turn --build-info)"
daemon_info="$($app/Contents/MacOS/turnd --build-info)"
hook_info="$($app/Contents/MacOS/turn-hook --build-info)"
for tuple in \
    "$turn_info|turn" \
    "$daemon_info|turnd" \
    "$hook_info|turn-hook"; do
    info="${tuple%|*}"
    expected_component="${tuple##*|}"
    if [[ "$(field "$info" component || true)" != "$expected_component" ]]; then
        echo "verify-macos-app: wrong component build info: $info" >&2
        exit 1
    fi
    if [[ "$(field "$info" version || true)" != "$manifest_version" ]]; then
        echo "verify-macos-app: $expected_component version differs from the bundle" >&2
        exit 1
    fi
    if [[ "$(extract "components.$expected_component.version")" != "$manifest_version" ]]; then
        echo "verify-macos-app: $expected_component version differs from release.plist" >&2
        exit 1
    fi
done

protocol_min="$(extract protocol_min)"
protocol_max="$(extract protocol_max)"
for info in "$turn_info" "$daemon_info"; do
    if [[ "$(field "$info" protocol_min || true)" != "$protocol_min" || \
          "$(field "$info" protocol_max || true)" != "$protocol_max" ]]; then
        echo "verify-macos-app: packaged protocol windows differ: $info" >&2
        exit 1
    fi
done

for binary in turnd turn-hook; do
    actual="$(shasum -a 256 "$app/Contents/MacOS/$binary" | awk '{print $1}')"
    expected="$(extract "components.$binary.sha256")"
    if [[ "$actual" != "$expected" ]]; then
        echo "verify-macos-app: $binary checksum differs from release.plist" >&2
        exit 1
    fi
done

codesign --verify --deep --strict --verbose=2 "$app"
for binary in turn turnd turn-hook; do
    codesign --verify --strict --verbose=2 "$app/Contents/MacOS/$binary"
done

if [[ "${TURN_REQUIRE_NOTARIZATION:-0}" == "1" ]]; then
    xcrun stapler validate "$app"
    spctl --assess --type execute --verbose=2 "$app"
fi

if [[ -n "${TURN_EXPECT_TEAM_ID:-}" ]]; then
    team_id="$(codesign -d --verbose=4 "$app" 2>&1 | awk -F= '/^TeamIdentifier=/{print $2; exit}')"
    if [[ "$team_id" != "$TURN_EXPECT_TEAM_ID" ]]; then
        echo "verify-macos-app: expected signing team $TURN_EXPECT_TEAM_ID, found ${team_id:-none}" >&2
        exit 1
    fi
fi

echo "verify-macos-app: $manifest_version, protocol $protocol_min..=$protocol_max, signatures valid"
