#!/usr/bin/env bash
set -euo pipefail

export LC_ALL=C

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
protocol_path=${OPERATION_PROTOCOL_PATH:-"$repo_root/docs/PROTOCOL.md"}
registry_path=${OPERATION_REGISTRY_PATH:-"$repo_root/docs/OPERATION_REGISTRY_CAP105_112_VNEXT.tsv"}
privacy_path=${OPERATION_PRIVACY_PATH:-"$repo_root/docs/PRIVACY.md"}

oracle_row_count=66
oracle_rows_sha256=15ba77aaba7b1116d3c3a152770740551bf95eb156960bad0d47eb4c90dcbbe8

die() {
  code=$1
  shift
  echo "$code: $*" >&2
  exit 1
}

[[ -f "$protocol_path" && ! -L "$protocol_path" ]] ||
  die E_OPERATION_PROTOCOL_MISSING "$protocol_path"
[[ -f "$registry_path" && ! -L "$registry_path" ]] ||
  die E_OPERATION_REGISTRY_MISSING "$registry_path"
[[ -f "$privacy_path" && ! -L "$privacy_path" ]] ||
  die E_OPERATION_PRIVACY_MISSING "$privacy_path"

header=$'schema_version\tcapability_ref\toperation\tdirection\teffect_class\tauthority_class\tstate_streams\tfence_derivation\tidempotency_fingerprint\tlocal_dispatch\tremote_policy\tremote_role_predicate'
[[ "$(head -n 1 "$registry_path")" == "$header" ]] || die E_OPERATION_REGISTRY_HEADER "unexpected header"

tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/turn-operation-registry.XXXXXX")
trap 'rm -rf -- "$tmp_dir"' EXIT

awk -F '\t' '
NR == 1 { next }
NF != 12 {
  printf "E_OPERATION_REGISTRY_COLUMNS: line=%d fields=%d\n", NR, NF > "/dev/stderr"; bad=1; next
}
$1 != "vNext" {
  printf "E_OPERATION_REGISTRY_VERSION: line=%d value=%s\n", NR, $1 > "/dev/stderr"; bad=1
}
$2 !~ /^CAP-(105|106|109|110|111|112)$/ {
  printf "E_OPERATION_REGISTRY_CAPABILITY: line=%d value=%s\n", NR, $2 > "/dev/stderr"; bad=1
}
$3 !~ /^[a-z][a-z0-9_]*$/ {
  printf "E_OPERATION_REGISTRY_NAME: line=%d value=%s\n", NR, $3 > "/dev/stderr"; bad=1
}
$4 != "client_request" {
  printf "E_OPERATION_REGISTRY_DIRECTION: op=%s value=%s\n", $3, $4 > "/dev/stderr"; bad=1
}
$5 !~ /^(pure_read|subscription|navigation|ephemeral_collaboration|cursor_mutation|domain_mutation|input|denied)$/ {
  printf "E_OPERATION_REGISTRY_EFFECT: op=%s value=%s\n", $3, $5 > "/dev/stderr"; bad=1
}
$6 !~ /^(scoped_read|foreground_surface|local_desktop_foreground|endpoint_broker|verifier_or_foreground_surface|authorised_control_surface|local_desktop_or_bulk_restart_dispatch)$/ {
  printf "E_OPERATION_REGISTRY_AUTHORITY: op=%s value=%s\n", $3, $6 > "/dev/stderr"; bad=1
}
$7 == "" || $7 !~ /^(Installation|Workspace\(|ExecutionTarget\()/ {
  printf "E_OPERATION_REGISTRY_STREAMS: op=%s value=%s\n", $3, $7 > "/dev/stderr"; bad=1
}
$8 == "" || $8 !~ /^request:/ {
  printf "E_OPERATION_REGISTRY_FENCE: op=%s value=%s\n", $3, $8 > "/dev/stderr"; bad=1
}
$9 !~ /^(canonical_request_v1|operation_id\+canonical_request_v1)$/ {
  printf "E_OPERATION_REGISTRY_FINGERPRINT: op=%s value=%s\n", $3, $9 > "/dev/stderr"; bad=1
}
$5 == "navigation" && $9 != "operation_id+canonical_request_v1" {
  printf "E_OPERATION_REGISTRY_NAVIGATION_ENVELOPE: op=%s fingerprint=%s\n", $3, $9 > "/dev/stderr"; bad=1
}
$10 !~ /^(native_scoped|native_foreground|native_local_foreground|endpoint_broker|native_or_endpoint_verifier|native_or_bulk_restart_dispatch)$/ {
  printf "E_OPERATION_REGISTRY_DISPATCH: op=%s value=%s\n", $3, $10 > "/dev/stderr"; bad=1
}
$11 !~ /^(allow_if_invited|deny)$/ {
  printf "E_OPERATION_REGISTRY_REMOTE_POLICY: op=%s value=%s\n", $3, $11 > "/dev/stderr"; bad=1
}
$12 !~ /^(full_gui|full_gui_or_headless_status|none)$/ {
  printf "E_OPERATION_REGISTRY_REMOTE_ROLE: op=%s value=%s\n", $3, $12 > "/dev/stderr"; bad=1
}
$6 == "local_desktop_foreground" &&
  ($10 != "native_local_foreground" || $11 != "deny" || $12 != "none") {
  printf "E_OPERATION_REGISTRY_LOCAL_ESCALATION: op=%s dispatch=%s policy=%s role=%s\n", $3, $10, $11, $12 > "/dev/stderr"; bad=1
}
$6 == "local_desktop_or_bulk_restart_dispatch" &&
  ($10 != "native_or_bulk_restart_dispatch" || $11 != "deny" || $12 != "none") {
  printf "E_OPERATION_REGISTRY_POLICY_ESCALATION: op=%s\n", $3 > "/dev/stderr"; bad=1
}
$6 == "endpoint_broker" && $10 != "endpoint_broker" {
  printf "E_OPERATION_REGISTRY_BROKER_UNREACHABLE: op=%s\n", $3 > "/dev/stderr"; bad=1
}
$11 == "deny" && $12 != "none" {
  printf "E_OPERATION_REGISTRY_DENIED_ROLE: op=%s role=%s\n", $3, $12 > "/dev/stderr"; bad=1
}
$11 == "allow_if_invited" && $12 == "none" {
  printf "E_OPERATION_REGISTRY_ALLOWED_ROLE: op=%s\n", $3 > "/dev/stderr"; bad=1
}
$11 == "allow_if_invited" && $5 ~ /^(pure_read|subscription|navigation|cursor_mutation)$/ &&
  $12 != "full_gui_or_headless_status" {
  printf "E_OPERATION_REGISTRY_READ_ROLE: op=%s role=%s\n", $3, $12 > "/dev/stderr"; bad=1
}
$11 == "allow_if_invited" && $5 ~ /^(domain_mutation|input|ephemeral_collaboration)$/ && $12 != "full_gui" {
  printf "E_OPERATION_REGISTRY_MUTATION_ROLE: op=%s role=%s\n", $3, $12 > "/dev/stderr"; bad=1
}
{
  seen[$3]++
  if (seen[$3] > 1) {
    printf "E_OPERATION_REGISTRY_DUPLICATE: %s\n", $3 > "/dev/stderr"; bad=1
  }
}
END {
  if (NR == 1) { print "E_OPERATION_REGISTRY_EMPTY" > "/dev/stderr"; bad=1 }
  exit bad
}
' "$registry_path" || exit 1

require_fence_token() {
  local operation=$1
  local token=$2
  local fences
  fences=$(awk -F '\t' -v operation="$operation" '$3 == operation { print $8 }' "$registry_path")
  [[ -n "$fences" && "$fences" == *"$token"* ]] ||
    die E_OPERATION_REGISTRY_FENCE_SEMANTICS "$operation missing=$token"
}

require_privacy_key() {
  local key=$1
  grep -Fq "| \`$key\` |" "$privacy_path" ||
    die E_OPERATION_REGISTRY_PRIVACY_BOUND "$key"
}

# The registry is an executable fence contract, not a names-only inventory.
require_fence_token prepare_pty_capacity_remediation provider_capability_revision
require_fence_token prepare_pty_capacity_remediation persistent_config_identity_hash_or_absence
require_fence_token prepare_pty_capacity_remediation fixed_helper_provider_identity
require_fence_token apply_pty_capacity_remediation reread_kernel_ceiling
require_fence_token revalidate_runtime_endpoint_continuity endpoint_binding_inventory_revision
require_fence_token rotate_runtime_endpoint_continuity_key preassigned_RuntimeEndpointContinuityReceiptId
require_fence_token rebind_runtime_endpoint_conversation_profile preassigned_ConversationProfileRebindReceiptId
require_fence_token rebind_runtime_endpoint_conversation_profile preassigned_new_RuntimeEndpointBindingId
require_fence_token set_dependency_edge expected_absent_or_current_edge_generation_revision
require_fence_token remove_dependency_edge dependency_graph_revision
require_fence_token preflight_flow_run dependency_graph_policy_revisions
require_fence_token set_private_transcript_search_policy preassigned_PrivateTranscriptSearchOperationId
require_fence_token set_private_transcript_search_policy enabled_or_disabled_current_index_key_descriptor_revisions
require_fence_token query_private_transcript_search PrivateTranscriptSearchQueryBuffer
require_fence_token select_private_transcript_search_hit outbox_chunk_capacity_before_Surface_CAS
require_fence_token attach_runtime_attempt preassigned_RuntimeAttachmentReceiptId
require_fence_token attach_pane preassigned_PaneAttachmentId_AttachmentGeneration_BaselineGeneration
require_fence_token resume_agent_instance preassigned_RuntimeAttemptId_RuntimeLaunchIntentId_RuntimeLaunchReceiptId
require_fence_token resume_agent_instance ownership_registry_revision
require_fence_token restart_runtime_owner preassigned_RuntimeLifecycleIntentId_replacement_RuntimeAttemptId_RuntimeLaunchIntentId
require_fence_token recycle_runtime_owner preassigned_RuntimeLifecycleIntentId_replacement_RuntimeAttemptId_RuntimeLaunchIntentId
require_fence_token terminate_runtime_owner preassigned_RuntimeLifecycleIntentId
require_fence_token kill_runtime_owner preassigned_RuntimeLifecycleIntentId
require_fence_token switch_agent_configuration preassigned_RuntimeAttemptId_RuntimeConfigurationReceiptId
require_fence_token get_container_close_survivor_inventory typed_survivor_counts_roots
require_fence_token get_container_close_survivor_page predecessor_digest

require_privacy_key records.runtime_lifecycle_nonterminal_per_attempt_owner
require_privacy_key records.runtime_lifecycle_nonterminal_installation
require_privacy_key records.runtime_configuration_nonterminal_per_instance
require_privacy_key records.runtime_configuration_nonterminal_installation
require_privacy_key records.private_transcript_search_nonterminal_operations_per_scope
require_privacy_key records.private_transcript_search_rich_operation_receipts
require_privacy_key records.private_transcript_search_minimal_fences
require_privacy_key records.historical_conversation_view_buffers_per_surface
require_privacy_key records.historical_conversation_view_buffers_installation
require_privacy_key records.container_close_survivor_memberships
require_privacy_key records.recovery_inventory_queries_installation

cat >"$tmp_dir/expected-pairs.tsv" <<'EOF'
CAP-105	apply_pty_capacity_remediation
CAP-105	cancel_pty_capacity_remediation
CAP-105	get_pty_capacity
CAP-105	prepare_pty_capacity_remediation
CAP-105	reconcile_pty_capacity_remediation
CAP-106	get_conversation_profile_rebind
CAP-106	get_runtime_continuity
CAP-106	get_runtime_endpoint_continuity_operation
CAP-106	rebind_runtime_endpoint_conversation_profile
CAP-106	reconcile_conversation_profile_rebind
CAP-106	reconcile_runtime_endpoint_continuity_operation
CAP-106	revalidate_runtime_endpoint_continuity
CAP-106	rotate_runtime_endpoint_continuity_key
CAP-109	create_flow_definition
CAP-109	get_flow_run
CAP-109	preflight_flow_run
CAP-109	remove_dependency_edge
CAP-109	retry_flow_step
CAP-109	set_dependency_edge
CAP-109	start_flow_run
CAP-109	start_flow_step
CAP-109	version_flow_definition
CAP-110	delete_private_transcript_search_index
CAP-110	get_historical_conversation_view
CAP-110	get_private_transcript_search_operation
CAP-110	get_private_transcript_search_state
CAP-110	query_private_transcript_search
CAP-110	rebuild_private_transcript_search_index
CAP-110	reconcile_private_transcript_search_operation
CAP-110	select_private_transcript_search_hit
CAP-110	set_private_transcript_search_policy
CAP-111	close_session
CAP-111	close_workspace
CAP-111	delete_session
CAP-111	delete_workspace
CAP-111	get_container_close_survivor_inventory
CAP-111	get_container_close_survivor_page
CAP-111	get_semantic_recovery_inventory
CAP-111	get_semantic_recovery_page
CAP-112	attach_runtime_attempt
CAP-112	attach_pane
CAP-112	adopt_conversation
CAP-112	branch_agent_instance
CAP-112	create_agent_instance
CAP-112	destroy_runtime_owner
CAP-112	detach_runtime_view
CAP-112	get_conversation_adoption
CAP-112	get_runtime_attachment_operation
CAP-112	get_runtime_configuration_operation
CAP-112	get_runtime_interrupt_operation
CAP-112	get_runtime_lifecycle_operation
CAP-112	get_runtime_launch_operation
CAP-112	interrupt_runtime_owner
CAP-112	kill_runtime_owner
CAP-112	reconcile_conversation_adoption
CAP-112	reconcile_runtime_attachment_operation
CAP-112	reconcile_runtime_configuration_operation
CAP-112	reconcile_runtime_interrupt_operation
CAP-112	reconcile_runtime_lifecycle_operation
CAP-112	reconcile_runtime_launch_operation
CAP-112	recycle_runtime_owner
CAP-112	resync_pane
CAP-112	restart_runtime_owner
CAP-112	resume_agent_instance
CAP-112	switch_agent_configuration
CAP-112	terminate_runtime_owner
EOF

tail -n +2 "$registry_path" | cut -f2,3 | sort >"$tmp_dir/actual-pairs.tsv"
sort -o "$tmp_dir/expected-pairs.tsv" "$tmp_dir/expected-pairs.tsv"
if ! diff -u "$tmp_dir/expected-pairs.tsv" "$tmp_dir/actual-pairs.tsv" >"$tmp_dir/pairs.diff"; then
  echo "E_OPERATION_REGISTRY_CLOSURE: CAP-105..112 operation set drift" >&2
  sed -n '1,140p' "$tmp_dir/pairs.diff" >&2
  exit 1
fi

grep -Fq 'CAP-107 adds only automatic presentation/attachment reducers' "$protocol_path" ||
  die E_OPERATION_REGISTRY_CAP107_MARKER "missing internal-only CAP-107 closure"
grep -Fq 'CAP-108 rejects automatic' "$protocol_path" ||
  die E_OPERATION_REGISTRY_CAP108_MARKER "missing rejected/no-operation CAP-108 closure"

while IFS=$'\t' read -r capability operation; do
  table_count=$(awk -F '|' -v token="\`$operation\`" '
    /^\|/ && index($2, token) { count++ }
    END { print count+0 }
  ' "$protocol_path")
  [[ "$table_count" == 1 ]] ||
    die E_OPERATION_REGISTRY_TABLE_BIJECTION "$capability/$operation table_rows=$table_count"
done <"$tmp_dir/expected-pairs.tsv"

awk '
BEGIN { marker=0; block=0; class=""; OFS="\t" }
/`RemoteOperatorSurfaceNonDenied\.vNext` projection/ { marker=1 }
marker && !block && /^```text$/ { block=1; next }
block && /^```$/ { ended=1; exit }
block {
  line=$0
  if (match(line, /^[a-z_]+ = /)) {
    class=substr(line, 1, index(line, " = ")-1)
    sub(/^[a-z_]+ = /, "", line)
  }
  if (class == "") next
  n=split(line, values, ",")
  for (i=1; i<=n; i++) {
    value=values[i]
    gsub(/^[[:space:]]+|[[:space:]]+$/, "", value)
    if (value != "") print value, class
  }
}
END {
  if (!marker || !block || !ended) {
    print "E_OPERATION_REGISTRY_REMOTE_BLOCK" > "/dev/stderr"
    exit 2
  }
}
' "$protocol_path" >"$tmp_dir/remote.tsv" || exit 1

duplicate_remote=$(cut -f1 "$tmp_dir/remote.tsv" | sort | uniq -d | head -1)
[[ -z "$duplicate_remote" ]] || die E_OPERATION_REGISTRY_REMOTE_DUPLICATE "$duplicate_remote"
sort -o "$tmp_dir/remote.tsv" "$tmp_dir/remote.tsv"

tail -n +2 "$registry_path" |
while IFS=$'\t' read -r _schema capability operation _direction effect _authority _streams fences _fingerprint _dispatch remote_policy _remote_role; do
  remote_match=$(awk -F '\t' -v op="$operation" '$1 == op { print $2 }' "$tmp_dir/remote.tsv")
  if [[ "$remote_policy" == allow_if_invited ]]; then
    [[ "$remote_match" == "$effect" ]] ||
      die E_OPERATION_REGISTRY_REMOTE_BIJECTION "$capability/$operation registry=$effect remote=${remote_match:-absent}"
  else
    [[ -z "$remote_match" ]] ||
      die E_OPERATION_REGISTRY_REMOTE_ESCALATION "$capability/$operation appears as $remote_match"
  fi
done

row_count=$(($(wc -l <"$registry_path") - 1))
[[ "$row_count" == "$oracle_row_count" ]] ||
  die E_OPERATION_REGISTRY_ORACLE_COUNT "expected=$oracle_row_count actual=$row_count"
sort -t $'\t' -k2,2 -k3,3 < <(tail -n +2 "$registry_path") >"$tmp_dir/registry.sorted.tsv"
if command -v sha256sum >/dev/null 2>&1; then
  rows_sha256=$(sha256sum "$tmp_dir/registry.sorted.tsv" | awk '{ print $1 }')
else
  rows_sha256=$(shasum -a 256 "$tmp_dir/registry.sorted.tsv" | awk '{ print $1 }')
fi
[[ "$rows_sha256" == "$oracle_rows_sha256" ]] ||
  die E_OPERATION_REGISTRY_ORACLE_DIGEST "expected=$oracle_rows_sha256 actual=$rows_sha256"
echo "OPERATION_REGISTRY_OK: $row_count rows; sha256=$rows_sha256; global registry distinct from remote projection"
