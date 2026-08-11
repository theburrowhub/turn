#!/usr/bin/env bash
set -euo pipefail

compare_unsigned() {
    local left="$1"
    local right="$2"
    while [[ ${#left} -gt 1 && "$left" == 0* ]]; do
        left="${left#0}"
    done
    while [[ ${#right} -gt 1 && "$right" == 0* ]]; do
        right="${right#0}"
    done
    if [[ ${#left} -lt ${#right} ]]; then
        VERSION_COMPARE_RESULT=-1
    elif [[ ${#left} -gt ${#right} ]]; then
        VERSION_COMPARE_RESULT=1
    elif [[ "$left" < "$right" ]]; then
        VERSION_COMPARE_RESULT=-1
    elif [[ "$left" > "$right" ]]; then
        VERSION_COMPARE_RESULT=1
    else
        VERSION_COMPARE_RESULT=0
    fi
}

compare_dotted_numeric() {
    local left="$1"
    local right="$2"
    local left_parts right_parts count index left_part right_part
    IFS=. read -r -a left_parts <<< "$left"
    IFS=. read -r -a right_parts <<< "$right"
    count=${#left_parts[@]}
    if [[ ${#right_parts[@]} -gt $count ]]; then
        count=${#right_parts[@]}
    fi
    for ((index = 0; index < count; index++)); do
        left_part="${left_parts[index]:-0}"
        right_part="${right_parts[index]:-0}"
        compare_unsigned "$left_part" "$right_part"
        if [[ "$VERSION_COMPARE_RESULT" != 0 ]]; then
            return
        fi
    done
    VERSION_COMPARE_RESULT=0
}

compare_semver() {
    local left="${1%%+*}"
    local right="${2%%+*}"
    local left_core="${left%%-*}"
    local right_core="${right%%-*}"
    local left_pre=""
    local right_pre=""
    local left_parts right_parts count index left_part right_part
    if [[ "$left" == *-* ]]; then
        left_pre="${left#*-}"
    fi
    if [[ "$right" == *-* ]]; then
        right_pre="${right#*-}"
    fi

    compare_dotted_numeric "$left_core" "$right_core"
    if [[ "$VERSION_COMPARE_RESULT" != 0 ]]; then
        return
    fi
    if [[ -z "$left_pre" && -z "$right_pre" ]]; then
        VERSION_COMPARE_RESULT=0
        return
    elif [[ -z "$left_pre" ]]; then
        VERSION_COMPARE_RESULT=1
        return
    elif [[ -z "$right_pre" ]]; then
        VERSION_COMPARE_RESULT=-1
        return
    fi

    IFS=. read -r -a left_parts <<< "$left_pre"
    IFS=. read -r -a right_parts <<< "$right_pre"
    count=${#left_parts[@]}
    if [[ ${#right_parts[@]} -gt $count ]]; then
        count=${#right_parts[@]}
    fi
    for ((index = 0; index < count; index++)); do
        if [[ $index -ge ${#left_parts[@]} ]]; then
            VERSION_COMPARE_RESULT=-1
            return
        elif [[ $index -ge ${#right_parts[@]} ]]; then
            VERSION_COMPARE_RESULT=1
            return
        fi
        left_part="${left_parts[index]}"
        right_part="${right_parts[index]}"
        if [[ "$left_part" =~ ^[0-9]+$ && "$right_part" =~ ^[0-9]+$ ]]; then
            compare_unsigned "$left_part" "$right_part"
        elif [[ "$left_part" =~ ^[0-9]+$ ]]; then
            VERSION_COMPARE_RESULT=-1
        elif [[ "$right_part" =~ ^[0-9]+$ ]]; then
            VERSION_COMPARE_RESULT=1
        elif [[ "$left_part" < "$right_part" ]]; then
            VERSION_COMPARE_RESULT=-1
        elif [[ "$left_part" > "$right_part" ]]; then
            VERSION_COMPARE_RESULT=1
        else
            VERSION_COMPARE_RESULT=0
        fi
        if [[ "$VERSION_COMPARE_RESULT" != 0 ]]; then
            return
        fi
    done
    VERSION_COMPARE_RESULT=0
}

assert_version_order() {
    local comparator="$1"
    local left="$2"
    local right="$3"
    local expected="$4"
    "$comparator" "$left" "$right"
    if [[ "$VERSION_COMPARE_RESULT" != "$expected" ]]; then
        echo "install-macos-update: version comparison failed for $left and $right" >&2
        exit 1
    fi
}

if [[ "${TURN_INSTALLER_VERSION_SELF_TEST:-0}" == "1" ]]; then
    assert_version_order compare_dotted_numeric 12.6 13.0 -1
    assert_version_order compare_dotted_numeric 15 15.0.0 0
    assert_version_order compare_dotted_numeric 15.10 15.9 1
    assert_version_order compare_semver 0.1.0-beta.2 0.1.0-beta.10 -1
    assert_version_order compare_semver 0.1.0-rc.1 0.1.0 -1
    assert_version_order compare_semver 0.1.0 0.1.0+build.7 0
    assert_version_order compare_semver 1.0.0-alpha 1.0.0-alpha.1 -1
    assert_version_order compare_semver 1.0.0-1 1.0.0-alpha -1
    echo "install-macos-update: version ordering self-test passed"
    exit 0
fi

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "install-macos-update: macOS is required" >&2
    exit 1
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
default_channel_url="https://github.com/theburrowhub/turn/releases/latest/download/turn-stable-$(uname -m).plist"
channel_url="${1:-$default_channel_url}"
target="${2:-/Applications/Turn.app}"
allow_local=false
if [[ "${TURN_ALLOW_ADHOC_UPDATE:-0}" == "1" ]]; then
    allow_local=true
fi
if [[ "$channel_url" != https://* && \
      ( "$allow_local" != true || "$channel_url" != /* || ! -f "$channel_url" ) ]]; then
    echo "install-macos-update: the update channel must use HTTPS" >&2
    exit 2
fi
if [[ "$target" != *.app ]]; then
    echo "install-macos-update: the install target must end in .app" >&2
    exit 2
fi
if [[ "$target" != /* ]]; then
    target="$PWD/$target"
fi
if [[ -L "$target" ]]; then
    echo "install-macos-update: refusing a symlink install target: $target" >&2
    exit 1
fi
target_parent="$(dirname "$target")"
if [[ ! -d "$target_parent" ]]; then
    echo "install-macos-update: target directory does not exist: $target_parent" >&2
    exit 1
fi
if [[ -e "$target" && ! -d "$target/Contents" ]]; then
    echo "install-macos-update: existing target is not an app bundle: $target" >&2
    exit 1
fi

download_stage="$(mktemp -d "${TMPDIR:-/tmp}/turn-update.XXXXXX")"
install_stage=""
backup=""
rollback_needed=false
had_previous=false
cleanup() {
    if [[ "$rollback_needed" == true && -n "$install_stage" ]]; then
        if [[ -e "$target" ]]; then
            mv "$target" "$install_stage/failed-new.app" 2>/dev/null || true
        fi
        if [[ "$had_previous" == true && -n "$backup" && -e "$backup" ]]; then
            mv "$backup" "$target" 2>/dev/null || true
        fi
    fi
    rm -rf "$download_stage"
    if [[ -n "$install_stage" ]]; then
        rm -rf "$install_stage"
    fi
}
trap cleanup EXIT INT TERM

fetch() {
    local source="$1"
    local destination="$2"
    if [[ "$source" == https://* ]]; then
        curl --fail --location --silent --show-error "$source" --output "$destination"
    elif [[ "$allow_local" == true && "$source" == /* && -f "$source" ]]; then
        cp "$source" "$destination"
    else
        echo "install-macos-update: refused a non-HTTPS artifact source" >&2
        return 1
    fi
}

manifest="$download_stage/channel.json"
archive="$download_stage/Turn.zip"
expanded="$download_stage/expanded"
mkdir -p "$expanded"
fetch "$channel_url" "$manifest"
plutil -lint "$manifest" >/dev/null

extract() {
    plutil -extract "$1" raw -o - "$manifest"
}

status_field() {
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

schema="$(extract schema)"
version="$(extract version)"
bundle_id="$(extract bundle_id)"
architecture="$(extract architecture)"
minimum_macos="$(extract minimum_macos)"
protocol_min="$(extract protocol_min)"
protocol_max="$(extract protocol_max)"
artifact_url="$(extract artifact.url)"
expected_sha256="$(extract artifact.sha256)"
expected_size="$(extract artifact.size)"
if [[ "$schema" != "1" || "$bundle_id" != "io.github.theburrowhub.turn" ]]; then
    echo "install-macos-update: unsupported update manifest" >&2
    exit 1
fi
if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]]; then
    echo "install-macos-update: invalid release version: $version" >&2
    exit 1
fi
if [[ "$architecture" != "$(uname -m)" ]]; then
    echo "install-macos-update: release is for $architecture, this Mac is $(uname -m)" >&2
    exit 1
fi
if [[ ! "$minimum_macos" =~ ^[0-9]+(\.[0-9]+){1,2}$ ]]; then
    echo "install-macos-update: invalid minimum macOS version" >&2
    exit 1
fi
current_macos="$(sw_vers -productVersion)"
compare_dotted_numeric "$current_macos" "$minimum_macos"
if [[ "$VERSION_COMPARE_RESULT" == -1 ]]; then
    echo "install-macos-update: Turn $version needs macOS $minimum_macos or newer (this Mac is $current_macos)" >&2
    exit 1
fi
if [[ ! "$protocol_min" =~ ^[0-9]+$ || ! "$protocol_max" =~ ^[0-9]+$ || \
      "$protocol_min" -gt "$protocol_max" ]]; then
    echo "install-macos-update: invalid release protocol window" >&2
    exit 1
fi
if [[ "$artifact_url" != https://* && \
      ( "$allow_local" != true || "$artifact_url" != /* || ! -f "$artifact_url" ) ]]; then
    echo "install-macos-update: invalid artifact source" >&2
    exit 1
fi
if [[ ! "$expected_sha256" =~ ^[0-9a-f]{64}$ || \
      ! "$expected_size" =~ ^[0-9]+$ ]]; then
    echo "install-macos-update: invalid artifact metadata" >&2
    exit 1
fi

fetch "$artifact_url" "$archive"
actual_size="$(stat -f '%z' "$archive")"
actual_sha256="$(shasum -a 256 "$archive" | awk '{print $1}')"
if [[ "$actual_size" != "$expected_size" || "$actual_sha256" != "$expected_sha256" ]]; then
    echo "install-macos-update: downloaded artifact does not match the channel manifest" >&2
    exit 1
fi
ditto -x -k "$archive" "$expanded"
candidate="$expanded/Turn.app"
if [[ ! -d "$candidate/Contents" ]]; then
    echo "install-macos-update: archive does not contain Turn.app at its root" >&2
    exit 1
fi

require_notarization=1
if [[ "${TURN_ALLOW_ADHOC_UPDATE:-0}" == "1" ]]; then
    require_notarization=0
fi
TURN_EXPECT_VERSION="$version" \
TURN_REQUIRE_NOTARIZATION="$require_notarization" \
    "$script_dir/verify-macos-app.sh" "$candidate"

candidate_bundle_id="$(plutil -extract CFBundleIdentifier raw -o - "$candidate/Contents/Info.plist")"
if [[ "$candidate_bundle_id" != "$bundle_id" ]]; then
    echo "install-macos-update: bundle identifier differs from the channel" >&2
    exit 1
fi
candidate_release="$candidate/Contents/Resources/release.plist"
candidate_architecture="$(plutil -extract architecture raw -o - "$candidate_release")"
candidate_protocol_min="$(plutil -extract protocol_min raw -o - "$candidate_release")"
candidate_protocol_max="$(plutil -extract protocol_max raw -o - "$candidate_release")"
if [[ "$candidate_architecture" != "$architecture" || \
      "$candidate_protocol_min" != "$protocol_min" || \
      "$candidate_protocol_max" != "$protocol_max" ]]; then
    echo "install-macos-update: bundle compatibility differs from the channel manifest" >&2
    exit 1
fi

if [[ -d "$target/Contents" ]]; then
    current_bundle_id="$(plutil -extract CFBundleIdentifier raw -o - "$target/Contents/Info.plist")"
    current_version="$(plutil -extract CFBundleShortVersionString raw -o - "$target/Contents/Info.plist")"
    if [[ "$current_bundle_id" != "$bundle_id" ]]; then
        echo "install-macos-update: existing app has a different bundle identifier" >&2
        exit 1
    fi
    if [[ "${TURN_ALLOW_DOWNGRADE:-0}" != "1" ]]; then
        compare_semver "$version" "$current_version"
        if [[ "$VERSION_COMPARE_RESULT" != 1 ]]; then
            echo "install-macos-update: $version is not newer than installed $current_version" >&2
            exit 1
        fi
    fi
    if [[ "$require_notarization" == "1" ]]; then
        current_team="$(codesign -d --verbose=4 "$target" 2>&1 | awk -F= '/^TeamIdentifier=/{print $2; exit}')"
        candidate_team="$(codesign -d --verbose=4 "$candidate" 2>&1 | awk -F= '/^TeamIdentifier=/{print $2; exit}')"
        if [[ -z "$current_team" || "$candidate_team" != "$current_team" ]]; then
            echo "install-macos-update: signing team differs from the installed app" >&2
            exit 1
        fi
    fi
fi

# Ask the daemon through the currently installed client when possible. The query is
# read-only and never launches a companion. A clean install uses the candidate's copy.
status_binary="$candidate/Contents/MacOS/turn"
if [[ -x "$target/Contents/MacOS/turn" ]]; then
    status_binary="$target/Contents/MacOS/turn"
fi
status_file="$download_stage/status.json"
status_error="$download_stage/status.err"
set +e
"$status_binary" --update-status >"$status_file" 2>"$status_error"
status_exit=$?
set -e
if [[ "$status_exit" == "0" ]]; then
    status_line="$(tr -d '\r\n' < "$status_file")"
    daemon_min="$(status_field "$status_line" protocol_min || true)"
    daemon_max="$(status_field "$status_line" protocol_max || true)"
    active_ptys="$(status_field "$status_line" active_ptys || true)"
    daemon_version="$(status_field "$status_line" daemon_version || true)"
    if [[ ! "$daemon_min" =~ ^[0-9]+$ || ! "$daemon_max" =~ ^[0-9]+$ || \
          ! "$active_ptys" =~ ^[0-9]+$ ]]; then
        echo "install-macos-update: the daemon returned invalid update status" >&2
        exit 1
    fi
    if [[ "$protocol_min" -gt "$daemon_max" || "$daemon_min" -gt "$protocol_max" ]]; then
        if [[ "$active_ptys" -gt 0 ]]; then
            echo "install-macos-update: deferred — turnd $daemon_version owns $active_ptys live PTY(s), and Turn $version is protocol-incompatible" >&2
            echo "No process was stopped and no file was replaced. Finish those sessions before updating." >&2
            exit 20
        fi
        echo "install-macos-update: turnd $daemon_version is protocol-incompatible" >&2
        echo "It owns no live PTYs, but stopping it is an explicit action. Stop turnd, then run this update again." >&2
        exit 21
    fi
    echo "install-macos-update: compatible with live turnd $daemon_version; $active_ptys PTY(s) will stay alive"
elif [[ "$status_exit" == "3" ]]; then
    echo "install-macos-update: no running daemon; installing the complete bundle"
else
    sed -n '1,8p' "$status_error" >&2
    echo "install-macos-update: could not prove the daemon is safe to leave untouched" >&2
    exit 1
fi

install_stage="$(mktemp -d "$target_parent/.turn-install.XXXXXX")"
staged_app="$install_stage/Turn.app"
ditto "$candidate" "$staged_app"
TURN_EXPECT_VERSION="$version" \
TURN_REQUIRE_NOTARIZATION="$require_notarization" \
    "$script_dir/verify-macos-app.sh" "$staged_app"

backup="$install_stage/previous.app"
rollback_needed=true
if [[ -e "$target" ]]; then
    mv "$target" "$backup"
    had_previous=true
fi
if ! mv "$staged_app" "$target"; then
    echo "install-macos-update: could not place the new app; restoring the previous app" >&2
    exit 1
fi
if ! TURN_EXPECT_VERSION="$version" \
     TURN_REQUIRE_NOTARIZATION="$require_notarization" \
     "$script_dir/verify-macos-app.sh" "$target"; then
    echo "install-macos-update: installed verification failed; restoring the previous app" >&2
    exit 1
fi

rollback_needed=false
trap - EXIT INT TERM
rm -rf "$download_stage" "$install_stage"
echo "install-macos-update: installed Turn $version at $target"
echo "Quit and reopen only the Turn window when convenient; the running daemon and PTYs were not stopped."
