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

package_id="$($cargo_bin pkgid \
    --manifest-path "$repo_root/Cargo.toml" \
    --locked \
    -p turn-gui)"
version="${package_id##*#}"
version="${version##*@}"
if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]]; then
    echo "package-macos-app: could not resolve the workspace version" >&2
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

# Ad-hoc signing makes this development bundle internally consistent without
# pretending it is the Developer ID signed and notarized artifact tracked by #19.
for binary in turnd turn-hook turn; do
    codesign --force --sign - --timestamp=none "$app/Contents/MacOS/$binary"
done
codesign --force --sign - --timestamp=none "$app"
codesign --verify --deep --strict --verbose=2 "$app"

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
