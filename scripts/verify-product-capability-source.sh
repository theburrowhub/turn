#!/usr/bin/env bash
set -euo pipefail

export LC_ALL=C

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
coverage="$repo_root/docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv"
source_repository=${1:-}

die() {
  code=$1
  shift
  echo "product-capability-source-acceptance: $code: $*" >&2
  exit 1
}

hash_stream() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 | awk '{print $1}'
  else
    die E_HASH_TOOL "sha256sum or shasum is required"
  fi
}

[[ -n "$source_repository" ]] ||
  die E_USAGE "usage: verify-product-capability-source.sh /path/to/audited-source-repository"
[[ -f "$coverage" && ! -L "$coverage" ]] || die E_LEDGER "capability ledger is unavailable"
git -C "$source_repository" rev-parse --is-inside-work-tree >/dev/null 2>&1 ||
  die E_SOURCE_REPOSITORY "argument is not a readable Git worktree"

snapshot=$(sed -n '2s/^# source-snapshot: //p' "$coverage")
expected_tree=$(sed -n '3s/^# source-tree-sha256: //p' "$coverage")
expected_count=$(sed -n '7s/^# expected-feature-count: //p' "$coverage")
[[ "$snapshot" =~ ^[0-9a-f]{40}$ ]] || die E_SNAPSHOT "ledger snapshot is malformed"
[[ "$expected_tree" =~ ^[0-9a-f]{64}$ ]] || die E_TREE "ledger tree digest is malformed"
[[ "$expected_count" =~ ^[1-9][0-9]*$ ]] || die E_COUNT "ledger feature count is malformed"
git -C "$source_repository" cat-file -e "$snapshot^{commit}" 2>/dev/null ||
  die E_SNAPSHOT "frozen source snapshot is absent"

actual_tree=$(git -C "$source_repository" ls-tree -r --full-tree "$snapshot" | hash_stream)
[[ "$actual_tree" == "$expected_tree" ]] || die E_TREE "frozen source tree digest differs"

checked=0
while IFS=$'\t' read -r feature_id capability_key description disposition requirement acceptance decision locator digest rationale; do
  [[ "$feature_id" =~ ^CAP-[0-9]{3}$ ]] || continue
  [[ -n "$locator" && "$locator" != /* && "$locator" != *..* ]] ||
    die E_LOCATOR "$feature_id has an unsafe evidence locator"
  [[ "$(git -C "$source_repository" cat-file -t "$snapshot:$locator" 2>/dev/null || true)" == blob ]] ||
    die E_LOCATOR "$feature_id evidence is absent or is not a blob"
  actual_digest=$(git -C "$source_repository" show "$snapshot:$locator" | hash_stream)
  [[ "$actual_digest" == "$digest" ]] || die E_DIGEST "$feature_id evidence digest differs"
  checked=$((checked + 1))
done <"$coverage"

[[ "$checked" == "$expected_count" ]] ||
  die E_COUNT "expected $expected_count evidence rows, checked $checked"

echo "product-capability-source-acceptance: snapshot $snapshot, tree $actual_tree, $checked evidence blobs verified"
