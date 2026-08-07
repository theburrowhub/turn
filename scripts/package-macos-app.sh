#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "package-macos-app: macOS is required" >&2
    exit 1
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"
output="${1:-$repo_root/dist/Turn.app}"

if [[ "$output" != *.app ]]; then
    echo "package-macos-app: output must end in .app: $output" >&2
    exit 1
fi
if [[ -e "$output" ]]; then
    echo "package-macos-app: refusing to replace existing output: $output" >&2
    exit 1
fi

cargo_bin="${CARGO:-cargo}"
target_dir="${CARGO_TARGET_DIR:-$repo_root/target}"
if [[ "$target_dir" != /* ]]; then
    target_dir="$repo_root/$target_dir"
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

version="$($cargo_bin metadata \
    --manifest-path "$repo_root/Cargo.toml" \
    --locked \
    --no-deps \
    --format-version 1 \
    | sed -n 's/.*"name":"turn-gui","version":"\([^"]*\)".*/\1/p')"
if [[ -z "$version" ]]; then
    version="$(sed -n '/^\[workspace\.package\]$/,/^\[/s/^version = "\([^"]*\)"/\1/p' "$repo_root/Cargo.toml")"
fi
if [[ -z "$version" ]]; then
    echo "package-macos-app: could not resolve the workspace version" >&2
    exit 1
fi

stage="$(mktemp -d "${TMPDIR:-/tmp}/turn-app.XXXXXX")"
trap 'rm -rf "$stage"' EXIT
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

mkdir -p "$(dirname "$output")"
mv "$app" "$output"
trap - EXIT
rm -rf "$stage"

echo "package-macos-app: wrote $output"
