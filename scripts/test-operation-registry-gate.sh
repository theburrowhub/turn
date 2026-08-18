#!/usr/bin/env bash
set -euo pipefail

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
verifier="$repo_root/scripts/verify-operation-registry.sh"
source_protocol="$repo_root/docs/PROTOCOL.md"
source_registry="$repo_root/docs/OPERATION_REGISTRY_CAP105_112_VNEXT.tsv"
source_privacy="$repo_root/docs/PRIVACY.md"
tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/turn-operation-registry-test.XXXXXX")
trap 'rm -rf -- "$tmp_dir"' EXIT

seed() {
  cp -- "$source_protocol" "$tmp_dir/PROTOCOL.md"
  cp -- "$source_registry" "$tmp_dir/REGISTRY.tsv"
  cp -- "$source_privacy" "$tmp_dir/PRIVACY.md"
}

run_gate() {
  OPERATION_PROTOCOL_PATH="$tmp_dir/PROTOCOL.md" \
  OPERATION_REGISTRY_PATH="$tmp_dir/REGISTRY.tsv" \
  OPERATION_PRIVACY_PATH="$tmp_dir/PRIVACY.md" \
  bash "$verifier"
}

expect_failure() {
  local label=$1
  local pattern=$2
  local output
  if output=$(run_gate 2>&1); then
    echo "E_OPERATION_MUTATION_SURVIVED: $label" >&2
    exit 1
  fi
  if [[ "$output" != *"$pattern"* ]]; then
    echo "E_OPERATION_MUTATION_REASON: $label expected=$pattern" >&2
    printf '%s\n' "$output" >&2
    exit 1
  fi
}

seed
run_gate >/dev/null

seed
awk -F '\t' '$3 != "get_pty_capacity"' "$tmp_dir/REGISTRY.tsv" >"$tmp_dir/registry.new"
mv -- "$tmp_dir/registry.new" "$tmp_dir/REGISTRY.tsv"
expect_failure "registry row deletion" "E_OPERATION_REGISTRY_CLOSURE"

seed
duplicate_row=$(sed -n '2p' "$tmp_dir/REGISTRY.tsv")
printf '%s\n' "$duplicate_row" >>"$tmp_dir/REGISTRY.tsv"
expect_failure "duplicate operation" "E_OPERATION_REGISTRY_DUPLICATE"

seed
awk -F '\t' 'BEGIN { OFS="\t" } $3 == "get_pty_capacity" { $5="subscription" } { print }' \
  "$tmp_dir/REGISTRY.tsv" >"$tmp_dir/registry.new"
mv -- "$tmp_dir/registry.new" "$tmp_dir/REGISTRY.tsv"
expect_failure "remote effect-class drift" "E_OPERATION_REGISTRY_REMOTE_BIJECTION"

seed
awk -F '\t' 'BEGIN { OFS="\t" } $3 == "prepare_pty_capacity_remediation" { $11="allow_if_invited"; $12="full_gui" } { print }' \
  "$tmp_dir/REGISTRY.tsv" >"$tmp_dir/registry.new"
mv -- "$tmp_dir/registry.new" "$tmp_dir/REGISTRY.tsv"
expect_failure "LocalDesktop remote escalation" "E_OPERATION_REGISTRY_LOCAL_ESCALATION"

seed
awk '$0 !~ /^\| `get_historical_conversation_view` /' "$tmp_dir/PROTOCOL.md" >"$tmp_dir/protocol.new"
mv -- "$tmp_dir/protocol.new" "$tmp_dir/PROTOCOL.md"
expect_failure "operation table deletion" "E_OPERATION_REGISTRY_TABLE_BIJECTION"

seed
awk -F '\t' '$3 != "close_workspace"' "$tmp_dir/REGISTRY.tsv" >"$tmp_dir/registry.new"
mv -- "$tmp_dir/registry.new" "$tmp_dir/REGISTRY.tsv"
expect_failure "CAP-111 Workspace operation deletion" "E_OPERATION_REGISTRY_CLOSURE"

seed
awk -F '\t' '$3 != "get_container_close_survivor_page"' "$tmp_dir/REGISTRY.tsv" >"$tmp_dir/registry.new"
mv -- "$tmp_dir/registry.new" "$tmp_dir/REGISTRY.tsv"
expect_failure "CAP-111 survivor page deletion" "E_OPERATION_REGISTRY_FENCE_SEMANTICS"

seed
awk -F '\t' '$3 != "get_runtime_launch_operation"' "$tmp_dir/REGISTRY.tsv" >"$tmp_dir/registry.new"
mv -- "$tmp_dir/registry.new" "$tmp_dir/REGISTRY.tsv"
expect_failure "CAP-112 launch recovery deletion" "E_OPERATION_REGISTRY_CLOSURE"

seed
awk -F '\t' 'BEGIN { OFS="\t" } $3 == "get_runtime_continuity" { $10="none" } { print }' \
  "$tmp_dir/REGISTRY.tsv" >"$tmp_dir/registry.new"
mv -- "$tmp_dir/registry.new" "$tmp_dir/REGISTRY.tsv"
expect_failure "unreachable dispatch" "E_OPERATION_REGISTRY_DISPATCH"

seed
awk -F '\t' 'BEGIN { OFS="\t" } $3 == "get_runtime_continuity" { $9="" } { print }' \
  "$tmp_dir/REGISTRY.tsv" >"$tmp_dir/registry.new"
mv -- "$tmp_dir/registry.new" "$tmp_dir/REGISTRY.tsv"
expect_failure "missing fingerprint" "E_OPERATION_REGISTRY_FINGERPRINT"

seed
printf '%s\n' $'vNext\tCAP-107\tpark_terminal_view\tclient_request\tnavigation\tforeground_surface\tWorkspace(subject_workspace)\trequest:surface+view\tcanonical_request_v1\tnative_foreground\tdeny\tnone' \
  >>"$tmp_dir/REGISTRY.tsv"
expect_failure "invented CAP-107 operation" "E_OPERATION_REGISTRY_CAPABILITY"

seed
sed 's/persistent_config_identity_hash_or_absence/persistent_config_identity_unfenced/' \
  "$tmp_dir/REGISTRY.tsv" >"$tmp_dir/registry.new"
mv -- "$tmp_dir/registry.new" "$tmp_dir/REGISTRY.tsv"
expect_failure "CAP-105 persistent-config fence drift" "E_OPERATION_REGISTRY_FENCE_SEMANTICS"

seed
sed 's/endpoint_binding_inventory_revision/endpoint_binding_inventory_unfenced/' \
  "$tmp_dir/REGISTRY.tsv" >"$tmp_dir/registry.new"
mv -- "$tmp_dir/registry.new" "$tmp_dir/REGISTRY.tsv"
expect_failure "CAP-106 binding-inventory fence drift" "E_OPERATION_REGISTRY_FENCE_SEMANTICS"

seed
sed 's/dependency_graph_policy_revisions/dependency_graph_unpinned/' \
  "$tmp_dir/REGISTRY.tsv" >"$tmp_dir/registry.new"
mv -- "$tmp_dir/registry.new" "$tmp_dir/REGISTRY.tsv"
expect_failure "CAP-109 preflight policy drift" "E_OPERATION_REGISTRY_FENCE_SEMANTICS"

seed
sed 's/preassigned_PrivateTranscriptSearchOperationId/preassigned_search_id/' \
  "$tmp_dir/REGISTRY.tsv" >"$tmp_dir/registry.new"
mv -- "$tmp_dir/registry.new" "$tmp_dir/REGISTRY.tsv"
expect_failure "CAP-110 search operation provenance drift" "E_OPERATION_REGISTRY_FENCE_SEMANTICS"

seed
sed 's/outbox_chunk_capacity_before_Surface_CAS/outbox_capacity_after_Surface_CAS/' \
  "$tmp_dir/REGISTRY.tsv" >"$tmp_dir/registry.new"
mv -- "$tmp_dir/registry.new" "$tmp_dir/REGISTRY.tsv"
expect_failure "CAP-110 view buffer ordering drift" "E_OPERATION_REGISTRY_FENCE_SEMANTICS"

seed
sed 's/preassigned_RuntimeAttemptId_RuntimeLaunchIntentId_RuntimeLaunchReceiptId/preassigned_RuntimeAttemptId/' \
  "$tmp_dir/REGISTRY.tsv" >"$tmp_dir/registry.new"
mv -- "$tmp_dir/registry.new" "$tmp_dir/REGISTRY.tsv"
expect_failure "CAP-112 Resume identity drift" "E_OPERATION_REGISTRY_FENCE_SEMANTICS"

seed
sed 's/preassigned_RuntimeLifecycleIntentId_replacement_RuntimeAttemptId_RuntimeLaunchIntentId/preassigned_replacement_ids/' \
  "$tmp_dir/REGISTRY.tsv" >"$tmp_dir/registry.new"
mv -- "$tmp_dir/registry.new" "$tmp_dir/REGISTRY.tsv"
expect_failure "CAP-112 lifecycle identity drift" "E_OPERATION_REGISTRY_FENCE_SEMANTICS"

seed
sed 's/typed_survivor_counts_roots/survivor_summary/' "$tmp_dir/REGISTRY.tsv" >"$tmp_dir/registry.new"
mv -- "$tmp_dir/registry.new" "$tmp_dir/REGISTRY.tsv"
expect_failure "CAP-111 survivor-root drift" "E_OPERATION_REGISTRY_FENCE_SEMANTICS"

seed
awk '$0 !~ /`records.runtime_lifecycle_nonterminal_per_attempt_owner`/' \
  "$tmp_dir/PRIVACY.md" >"$tmp_dir/privacy.new"
mv -- "$tmp_dir/privacy.new" "$tmp_dir/PRIVACY.md"
expect_failure "runtime per-owner concurrency bound deletion" "E_OPERATION_REGISTRY_PRIVACY_BOUND"

seed
sed 's/portable_schema/portable_schema_unreviewed/' "$tmp_dir/REGISTRY.tsv" >"$tmp_dir/registry.new"
mv -- "$tmp_dir/registry.new" "$tmp_dir/REGISTRY.tsv"
expect_failure "coordinated unasserted registry drift" "E_OPERATION_REGISTRY_ORACLE_DIGEST"

echo "OPERATION_REGISTRY_MUTATIONS_OK: 21/21"
