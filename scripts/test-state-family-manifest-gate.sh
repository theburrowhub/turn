#!/usr/bin/env bash
set -euo pipefail

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
verifier="$repo_root/scripts/verify-state-family-manifest.sh"
source_protocol="$repo_root/docs/PROTOCOL.md"
source_manifest="$repo_root/docs/STATE_FAMILY_MANIFEST_VNEXT.tsv"
tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/turn-state-manifest-test.XXXXXX")
trap 'rm -rf -- "$tmp_dir"' EXIT

run_gate() {
  STATE_PROTOCOL_PATH="$tmp_dir/PROTOCOL.md" \
  STATE_MANIFEST_PATH="$tmp_dir/STATE_FAMILY_MANIFEST_VNEXT.tsv" \
  "$verifier"
}

run_emit() {
  STATE_PROTOCOL_PATH="$tmp_dir/PROTOCOL.md" \
  STATE_MANIFEST_PATH="$tmp_dir/STATE_FAMILY_MANIFEST_VNEXT.tsv" \
  "$verifier" --emit
}

seed() {
  cp -- "$source_protocol" "$tmp_dir/PROTOCOL.md"
  cp -- "$source_manifest" "$tmp_dir/STATE_FAMILY_MANIFEST_VNEXT.tsv"
}

expect_failure() {
  local label=$1
  local pattern=$2
  local command_name=${3:-run_gate}
  local output
  if output=$("$command_name" 2>&1); then
    echo "E_STATE_MUTATION_SURVIVED: $label" >&2
    exit 1
  fi
  if [[ "$output" != *"$pattern"* ]]; then
    echo "E_STATE_MUTATION_REASON: $label expected=$pattern" >&2
    printf '%s\n' "$output" >&2
    exit 1
  fi
}

remove_manifest_name() {
  local name=$1
  awk -F '\t' -v name="$name" '$2 != name' "$tmp_dir/STATE_FAMILY_MANIFEST_VNEXT.tsv" \
    >"$tmp_dir/manifest.new"
  mv -- "$tmp_dir/manifest.new" "$tmp_dir/STATE_FAMILY_MANIFEST_VNEXT.tsv"
}

seed
run_gate >/dev/null

seed
remove_manifest_name AccountActivityProjection
expect_failure "TSV row deletion" "E_STATE_MANIFEST_DRIFT"

seed
duplicate_row=$(sed -n '2p' "$tmp_dir/STATE_FAMILY_MANIFEST_VNEXT.tsv")
printf '%s\n' "$duplicate_row" >>"$tmp_dir/STATE_FAMILY_MANIFEST_VNEXT.tsv"
expect_failure "TSV duplicate" "E_STATE_MANIFEST_DUPLICATE"

seed
awk -F '\t' 'BEGIN { OFS="\t" } NR == 2 { $6="WrongOwner" } { print }' \
  "$tmp_dir/STATE_FAMILY_MANIFEST_VNEXT.tsv" >"$tmp_dir/manifest.new"
mv -- "$tmp_dir/manifest.new" "$tmp_dir/STATE_FAMILY_MANIFEST_VNEXT.tsv"
expect_failure "TSV owner drift" "E_STATE_MANIFEST_DRIFT"

seed
awk -F '\t' 'BEGIN { OFS="\t" } NR == 2 { $4="ephemeral" } { print }' \
  "$tmp_dir/STATE_FAMILY_MANIFEST_VNEXT.tsv" >"$tmp_dir/manifest.new"
mv -- "$tmp_dir/manifest.new" "$tmp_dir/STATE_FAMILY_MANIFEST_VNEXT.tsv"
expect_failure "TSV lifetime drift" "E_STATE_MANIFEST_DRIFT"

seed
perl -0pi -e 's/    SurfaceRegistry, Surface,/    Surface,/' "$tmp_dir/PROTOCOL.md"
expect_failure "presentation state-family deletion" "E_STATE_PRESENTATION_DRIFT"

seed
perl -0pi -e 's/`AgentBrowserReadPage`, //' "$tmp_dir/PROTOCOL.md"
expect_failure "presentation request-value deletion" "E_STATE_PRESENTATION_DRIFT"

seed
printf '%s\n' $'vNext\tUndeclaredState\tstate_family\tdurable\tInstallation\tInstallation(daemon_generation)' \
  >>"$tmp_dir/STATE_FAMILY_MANIFEST_VNEXT.tsv"
expect_failure "undeclared TSV family" "E_STATE_MANIFEST_DRIFT"

seed
perl -0pi -e 's/^  StateFamilyDeclaration\|WorkItemBinding\n//m' "$tmp_dir/PROTOCOL.md"
expect_failure "declaration marker deletion" "E_STATE_DECLARATION_ORPHAN"

seed
perl -0pi -e 's/^  \@protocol_decl\|vNext\|WorkItemBinding\n//m' "$tmp_dir/PROTOCOL.md"
expect_failure "protocol annotation deletion" "E_STATE_DECLARATION_UNANNOTATED"

seed
perl -0pi -e 's/^  \@state_family\|durable\|Workspace\|Workspace\(daemon_generation,WorkspaceId\)\n(?=  StateFamilyDeclaration\|WorkItemConflict$)//m' \
  "$tmp_dir/PROTOCOL.md"
expect_failure "state classification deletion" "E_STATE_DECLARATION_UNCLASSIFIED"

seed
perl -0pi -e 's/(  \@protocol_decl\|vNext\|WorkItemBinding\n  \@state_family\|durable\|Workspace\|Workspace\(daemon_generation,WorkspaceId\))/$1\n  \@state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)/' \
  "$tmp_dir/PROTOCOL.md"
expect_failure "second state classification" "E_STATE_DECLARATION_ORPHAN"

seed
perl -0pi -e 's/(  \@protocol_decl\|vNext\|PrivateTranscriptSearchPage\n)  \@request_value\|request_value\|request\|none/$1  \@state_family|ephemeral|ephemeral|ConnectionGeneration+RequestId/' \
  "$tmp_dir/PROTOCOL.md"
expect_failure "request value reclassified as state" "E_STATE_DECLARATION_UNCLASSIFIED"

seed
printf '%s\n' \
  'StateFamilyDeclaration|WorkItemBinding' \
  '@protocol_decl|vNext|WorkItemBinding' \
  '@state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)' \
  >>"$tmp_dir/PROTOCOL.md"
expect_failure "duplicate complete declaration" "E_STATE_DECLARATION_DUPLICATE"

seed
printf '%s\n' \
  'StateFamilyDeclaration|UnannotatedReducer' \
  'UnannotatedReducer has closed reducer prepared->running->done and retains bytes.' \
  >>"$tmp_dir/PROTOCOL.md"
expect_failure "new reducer without annotations" "E_STATE_DECLARATION_UNANNOTATED"

seed
printf '%s\n' \
  'StateFamilyDeclaration|NewDurableReducer' \
  '@protocol_decl|vNext|NewDurableReducer' \
  '@state_family|durable|Workspace|Workspace(daemon_generation,WorkspaceId)' \
  >>"$tmp_dir/PROTOCOL.md"
expect_failure "declared family missing presentation and TSV" "E_STATE_PRESENTATION_DRIFT"

seed
perl -0pi -e 's/WorkItemActivity, WorkItemBinding, WorkItemConflict/WorkItemActivity, WorkItemConflict/' \
  "$tmp_dir/PROTOCOL.md"
remove_manifest_name WorkItemBinding
expect_failure "coordinated presentation and TSV deletion" "E_STATE_PRESENTATION_DRIFT"

seed
perl -0pi -e 's/^  StateFamilyDeclaration\|SurfaceRegistry\n  \@protocol_decl\|vNext\|SurfaceRegistry\n  \@state_family\|durable\|Installation\|Installation\(daemon_generation\)\n//m' \
  "$tmp_dir/PROTOCOL.md"
perl -0pi -e 's/    SurfaceRegistry, Surface,/    Surface,/' "$tmp_dir/PROTOCOL.md"
remove_manifest_name SurfaceRegistry
expect_failure "coordinated declaration presentation and TSV deletion" "E_STATE_ORACLE_COUNT"

seed
perl -0pi -e 's/(  \@protocol_decl\|vNext\|LiveSubscriptionRegistry\n)  \@state_family\|ephemeral\|ephemeral\|DaemonGeneration/$1  \@state_family|ephemeral|ephemeral|DaemonGeneration+WrongGeneration/' \
  "$tmp_dir/PROTOCOL.md"
perl -0pi -e 's/LiveSubscriptionRegistry\(owner_key=DaemonGeneration\)/LiveSubscriptionRegistry(owner_key=DaemonGeneration+WrongGeneration)/' \
  "$tmp_dir/PROTOCOL.md"
awk -F '\t' 'BEGIN { OFS="\t" } $2 == "LiveSubscriptionRegistry" { $6="DaemonGeneration+WrongGeneration" } { print }' \
  "$tmp_dir/STATE_FAMILY_MANIFEST_VNEXT.tsv" >"$tmp_dir/manifest.new"
mv -- "$tmp_dir/manifest.new" "$tmp_dir/STATE_FAMILY_MANIFEST_VNEXT.tsv"
expect_failure "coordinated declaration presentation and TSV owner mutation" "E_STATE_ORACLE_DIGEST"

seed
perl -0pi -e 's/    SurfaceRegistry, Surface,/    Surface,/' "$tmp_dir/PROTOCOL.md"
expect_failure "emit refuses presentation drift" "E_STATE_PRESENTATION_DRIFT" run_emit

seed
remove_manifest_name SurfaceRegistry
run_emit >/dev/null
run_gate >/dev/null
if ! awk -F '\t' '$2 == "SurfaceRegistry" { found=1 } END { exit !found }' \
  "$tmp_dir/STATE_FAMILY_MANIFEST_VNEXT.tsv"; then
  echo "E_STATE_EMIT_SOURCE: declaration-derived emit did not restore SurfaceRegistry" >&2
  exit 1
fi

echo "STATE_MANIFEST_MUTATIONS_OK: 19/19"
echo "STATE_MANIFEST_DECLARATION_EMIT_OK"
