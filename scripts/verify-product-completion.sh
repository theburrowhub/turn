#!/usr/bin/env bash
set -euo pipefail

export LC_ALL=C

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
requirements="$repo_root/docs/PRODUCT_REQUIREMENTS.md"
evidence="$repo_root/docs/PRODUCT_IMPLEMENTATION_EVIDENCE_V1.tsv"

die() {
  code=$1
  shift
  echo "product-completion-acceptance: $code: $*" >&2
  exit 1
}

hash_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    die E_HASH_TOOL "sha256sum or shasum is required"
  fi
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

validate_relative_path() {
  path=$1
  case "$path" in
    ''|/*|-*|*/|*\\*|*@*|*,*|*//*|.git|.git/*) return 1 ;;
  esac
  [[ "$path" =~ ^[A-Za-z0-9._/-]+$ && "/$path/" != *"/../"* && "/$path/" != *"/./"* ]]
}

require_tracked_clean_regular() {
  path=$1
  [[ -f "$repo_root/$path" && ! -L "$repo_root/$path" ]] || die E_AUTHORITY_FILE "missing, non-regular or symlinked $path"
  git -C "$repo_root" ls-files --error-unmatch -- "$path" >/dev/null 2>&1 || die E_UNTRACKED_AUTHORITY "$path is not tracked"
  git -C "$repo_root" diff --quiet HEAD -- "$path" || die E_DIRTY_AUTHORITY "$path differs from HEAD"
}

spec_mode=--verify-local
if [[ -n "${TURN_EXPECTED_PRODUCT_SPEC_AUTHORITY_SHA256:-}" || "${CI:-}" == true ]]; then
  spec_mode=verify
fi
bash "$repo_root/scripts/verify-product-spec.sh" "$spec_mode" >/dev/null

not_implemented=$(awk -F '|' '
  /^\| `PRD-[A-Z]+-[0-9][0-9][0-9]` / {
    id=$2; status=$5
    gsub(/[ `]/, "", id); gsub(/^ +| +$/, "", status)
    if (status != "implemented") print id "=" status
  }
' "$requirements")

if [[ -n "$not_implemented" ]]; then
  count=$(printf '%s\n' "$not_implemented" | wc -l | tr -d ' ')
  echo "product-completion-acceptance: E_NOT_IMPLEMENTED: $count requirements are not implemented" >&2
  printf '%s\n' "$not_implemented" | sed 's/^/  /' >&2
  exit 1
fi

[[ -s "$evidence" && ! -L "$evidence" ]] || die E_EVIDENCE_MISSING "machine evidence ledger is missing"
git -C "$repo_root" rev-parse --is-inside-work-tree >/dev/null 2>&1 || die E_GIT "repository identity is unavailable"

require_tracked_clean_regular docs/PRODUCT_IMPLEMENTATION_EVIDENCE_V1.tsv
require_tracked_clean_regular docs/PRODUCT_IMPLEMENTATION_EVIDENCE.md
require_tracked_clean_regular docs/PRODUCT_REQUIREMENTS.md
require_tracked_clean_regular docs/PRODUCT_REQUIREMENTS_V1.manifest
require_tracked_clean_regular docs/PRODUCT_SPEC_V1.authority
require_tracked_clean_regular Makefile
require_tracked_clean_regular scripts/verify-product-spec.sh
require_tracked_clean_regular scripts/verify-product-completion.sh

[[ -z "$(git -C "$repo_root" status --porcelain --untracked-files=normal)" ]] ||
  die E_DIRTY_CHECKOUT "checkout must be completely clean, including non-ignored untracked files"

head_commit=$(git -C "$repo_root" rev-parse HEAD)
object_format=$(git -C "$repo_root" rev-parse --show-object-format=storage 2>/dev/null || true)
case "$object_format" in
  sha256) oid_length=64 ;;
  sha1) oid_length=40 ;;
  *) oid_length=${#head_commit} ;;
esac

scratch=$(mktemp -d "${TMPDIR:-/tmp}/turn-product-completion.XXXXXX")
trap 'rm -rf "$scratch"' EXIT

# Execute only committed HEAD content in a fresh checkout. Ignored build/cache/config files from the
# caller's working tree therefore cannot influence an oracle. Product entrypoints must keep all generated
# state in the supplied roots; any file they leave in this checkout is a proof failure.
oracle_repo="$scratch/oracle-checkout"
git clone --quiet --no-local --no-checkout "$repo_root" "$oracle_repo" || die E_ORACLE_CHECKOUT "cannot create isolated oracle checkout"
git -C "$oracle_repo" checkout --quiet --detach "$head_commit" || die E_ORACLE_CHECKOUT "cannot check out exact HEAD"
[[ -z "$(git -C "$oracle_repo" status --porcelain --ignored --untracked-files=all)" ]] ||
  die E_ORACLE_CHECKOUT "isolated oracle checkout is not empty and clean"

awk -F '|' '/^\| `PRD-[A-Z]+-[0-9][0-9][0-9]` / { id=$2; gsub(/[ `]/, "", id); print id }' \
  "$requirements" | sort >"$scratch/requirements"

if ! awk -F '\t' -v records="$scratch/records" '
  /^#/ || /^$/ { next }
  {
    if (NF != 5 || $1 !~ /^PRD-[A-Z]+-[0-9][0-9][0-9]$/ || $2 !~ /^[0-9a-f]+$/ ||
        $3 !~ /^acp-[a-z]+-[0-9][0-9][0-9]$/ || $4 == "" || length($5) != 64 || $5 !~ /^[0-9a-f]+$/) {
      print "product-completion-acceptance: E_EVIDENCE_PARSE: malformed evidence row at line " FNR > "/dev/stderr"; bad=1; next
    }
    expected=tolower($1); sub(/^prd-/, "acp-", expected)
    if ($3 != expected) {
      print "product-completion-acceptance: E_TARGET: target mismatch for " $1 > "/dev/stderr"; bad=1
    }
    print $0 > records
    print $1
  }
  END { exit bad }
' "$evidence" | sort >"$scratch/evidence-ids"; then
  exit 1
fi

sort "$scratch/evidence-ids" | uniq -d >"$scratch/duplicate-evidence"
[[ ! -s "$scratch/duplicate-evidence" ]] || die E_EVIDENCE_DUPLICATE "duplicate requirement evidence rows"
diff -u "$scratch/requirements" "$scratch/evidence-ids" >/dev/null ||
  die E_EVIDENCE_SET "evidence ledger is not one-to-one with requirements"

while IFS=$'\t' read -r id commit target descriptor_path descriptor_hash; do
  [[ ${#commit} -eq $oid_length && "$commit" =~ ^[0-9a-f]+$ ]] || die E_COMMIT_OID "$id has an invalid full object id"
  [[ "$(git -C "$repo_root" cat-file -t "$commit" 2>/dev/null || true)" == commit ]] || die E_COMMIT_MISSING "$id implementation commit is missing"
  git -C "$repo_root" merge-base --is-ancestor "$commit" "$head_commit" || die E_COMMIT_ANCESTRY "$id implementation commit is not an ancestor"

  expected_descriptor="tests/product-acceptance/descriptors/$target.tsv"
  [[ "$descriptor_path" == "$expected_descriptor" ]] || die E_DESCRIPTOR_PATH "$id descriptor path must be $expected_descriptor"
  validate_relative_path "$descriptor_path" || die E_DESCRIPTOR_PATH "$id descriptor path is unsafe"
  require_tracked_clean_regular "$descriptor_path"
  [[ "$(hash_file "$repo_root/$descriptor_path")" == "$descriptor_hash" ]] || die E_DESCRIPTOR_HASH "$id current descriptor hash differs"
  commit_mode=$(git -C "$repo_root" ls-tree "$commit" -- "$descriptor_path" | awk '{print $1}')
  [[ -n "$commit_mode" && "$commit_mode" != 120000 ]] || die E_ORACLE_NOT_AT_COMMIT "$id descriptor is missing or symlinked at implementation commit"
  [[ "$(git -C "$repo_root" show "$commit:$descriptor_path" | hash_stream)" == "$descriptor_hash" ]] ||
    die E_ORACLE_NOT_AT_COMMIT "$id descriptor differs at implementation commit"

  descriptor_records="$scratch/$target.records"
  if ! awk -F '\t' -v out="$descriptor_records" '
    {
      if ($0 ~ /\r/ || NF < 2 || NF > 3) {
        print "product-completion-acceptance: E_DESCRIPTOR_PARSE: malformed descriptor line " FNR > "/dev/stderr"; bad=1; next
      }
      kind=$1
      if (kind == "schema" && NF == 2 && $2 == "1") schema++
      else if (kind == "requirement" && NF == 2) requirement++
      else if (kind == "target" && NF == 2) target++
      else if ((kind == "entrypoint" || kind == "implementation" || kind == "support" || kind == "artifact") &&
               NF == 3 && length($3) == 64 && $3 ~ /^[0-9a-f]+$/) {
        if (kind == "entrypoint") entrypoint++
        if (kind == "implementation") implementation++
        if (kind == "artifact") artifact++
      } else {
        print "product-completion-acceptance: E_DESCRIPTOR_PARSE: invalid descriptor line " FNR > "/dev/stderr"; bad=1
      }
      print $0 > out
    }
    END {
      if (schema != 1 || requirement != 1 || target != 1 || entrypoint != 1 || implementation < 1 || artifact < 1) {
        print "product-completion-acceptance: E_DESCRIPTOR_PARSE: descriptor cardinality is invalid" > "/dev/stderr"; bad=1
      }
      exit bad
    }
  ' "$repo_root/$descriptor_path"; then
    exit 1
  fi

  descriptor_requirement=$(awk -F '\t' '$1=="requirement" {print $2}' "$descriptor_records")
  descriptor_target=$(awk -F '\t' '$1=="target" {print $2}' "$descriptor_records")
  [[ "$descriptor_requirement" == "$id" && "$descriptor_target" == "$target" ]] ||
    die E_DESCRIPTOR_ID "$id descriptor identity differs"

  entrypoint=$(awk -F '\t' '$1=="entrypoint" {print $2}' "$descriptor_records")
  expected_entrypoint="scripts/product-acceptance/$target.sh"
  [[ "$entrypoint" == "$expected_entrypoint" ]] || die E_ENTRYPOINT_PATH "$id entrypoint must be $expected_entrypoint"

  awk -F '\t' '
    $1 == "entrypoint" || $1 == "implementation" || $1 == "support" || $1 == "artifact" {
      key=$1 "\t" $2
      if (seen[key]++) exit 1
    }
  ' "$descriptor_records" || die E_DESCRIPTOR_DUPLICATE "$id descriptor repeats a typed path"

  while IFS=$'\t' read -r kind path expected_hash; do
    [[ "$kind" == entrypoint || "$kind" == implementation || "$kind" == support ]] || continue
    validate_relative_path "$path" || die E_SOURCE_PATH "$id source path is unsafe: $path"
    if [[ "$kind" == implementation ]]; then
      [[ "$path" == crates/* ]] || die E_IMPLEMENTATION_PATH "$id production implementation must be below crates/: $path"
    fi
    require_tracked_clean_regular "$path"
    [[ "$(hash_file "$repo_root/$path")" == "$expected_hash" ]] || die E_SOURCE_HASH "$id current source hash differs: $path"
    commit_mode=$(git -C "$repo_root" ls-tree "$commit" -- "$path" | awk '{print $1}')
    [[ -n "$commit_mode" && "$commit_mode" != 120000 ]] || die E_ORACLE_NOT_AT_COMMIT "$id source missing or symlinked at implementation commit: $path"
    if [[ "$kind" == entrypoint ]]; then
      [[ -x "$repo_root/$path" && "$commit_mode" == 100755 ]] || die E_ENTRYPOINT_MODE "$id entrypoint must be executable now and at implementation commit"
    fi
    [[ "$(git -C "$repo_root" show "$commit:$path" | hash_stream)" == "$expected_hash" ]] ||
      die E_ORACLE_NOT_AT_COMMIT "$id source differs at implementation commit: $path"
  done <"$descriptor_records"

  run_root="$scratch/run-$target"
  mkdir -m 700 "$run_root"
  token="$head_commit:$target:$$"
  oracle_entrypoint="$oracle_repo/$entrypoint"
  TURN_PRODUCT_ACCEPTANCE_ROOT="$run_root" \
  TURN_PRODUCT_ACCEPTANCE_TOKEN="$token" \
  TURN_PRODUCT_ACCEPTANCE_TARGET="$target" \
  CARGO_TARGET_DIR="$scratch/cargo-target" \
    "$oracle_entrypoint" || die E_ORACLE_FAILED "$target failed for $id"

  marker="$run_root/.oracle-invocations/$target"
  [[ -f "$marker" && ! -L "$marker" && "$(cat "$marker")" == "$token" ]] ||
    die E_ORACLE_NOT_INVOKED "$target did not execute its declared entrypoint"

  find "$run_root" -type l -print >"$scratch/$target.symlinks"
  [[ ! -s "$scratch/$target.symlinks" ]] || die E_ARTIFACT_SYMLINK "$target generated a symlink"
  find "$run_root" -mindepth 1 ! -type d ! -type f -print >"$scratch/$target.unsupported-nodes"
  [[ ! -s "$scratch/$target.unsupported-nodes" ]] || die E_ARTIFACT_NODE "$target generated a FIFO, socket or device"

  : >"$scratch/$target.expected-artifacts"
  while IFS=$'\t' read -r kind artifact_path expected_hash; do
    [[ "$kind" == artifact ]] || continue
    validate_relative_path "$artifact_path" || die E_ARTIFACT_PATH "$id artifact path is unsafe: $artifact_path"
    [[ "$artifact_path" == "$target/"* ]] || die E_ARTIFACT_PATH "$id artifact must be scoped below $target/"
    printf '%s\n' "$artifact_path" >>"$scratch/$target.expected-artifacts"
    absolute_artifact="$run_root/$artifact_path"
    [[ -f "$absolute_artifact" && ! -L "$absolute_artifact" ]] || die E_FRESH_ARTIFACT_MISSING "$target did not freshly generate $artifact_path"
    [[ "$(hash_file "$absolute_artifact")" == "$expected_hash" ]] || die E_ARTIFACT_HASH "$id artifact hash mismatch: $artifact_path"
  done <"$descriptor_records"
  sort -u "$scratch/$target.expected-artifacts" -o "$scratch/$target.expected-artifacts"
  find "$run_root" -type f ! -path "$run_root/.oracle-invocations/$target" -print | sed "s#^$run_root/##" | sort >"$scratch/$target.actual-artifacts"
  diff -u "$scratch/$target.expected-artifacts" "$scratch/$target.actual-artifacts" >/dev/null ||
    die E_ARTIFACT_SET "$target generated an undeclared or omitted artifact"

  [[ -z "$(git -C "$oracle_repo" status --porcelain --ignored --untracked-files=all)" ]] ||
    die E_ORACLE_DIRTY "$target modified its isolated checkout"
  [[ -z "$(git -C "$repo_root" status --porcelain --untracked-files=normal)" ]] ||
    die E_ORACLE_DIRTY "$target modified repository state outside its isolated checkout"
done <"$scratch/records"

echo "product-completion-acceptance: every requirement has commit-bound oracle sources and fresh exact hash-verified evidence at $head_commit"
