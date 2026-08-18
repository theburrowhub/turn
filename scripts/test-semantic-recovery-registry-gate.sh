#!/usr/bin/env bash
set -euo pipefail

export LC_ALL=C

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
verifier="$repo_root/scripts/verify-semantic-recovery-registry.sh"
source_protocol=${SEMANTIC_SOURCE_PROTOCOL:-"$repo_root/docs/PROTOCOL.md"}
source_subjects=${SEMANTIC_SOURCE_SUBJECTS:-"$repo_root/docs/SEMANTIC_RECOVERY_SUBJECTS_VNEXT.tsv"}
source_classifier=${SEMANTIC_SOURCE_CLASSIFIER:-"$repo_root/docs/SEMANTIC_RECOVERY_FAMILY_CLASSIFICATION_VNEXT.tsv"}
source_state_manifest=${SEMANTIC_SOURCE_STATE_MANIFEST:-"$repo_root/docs/STATE_FAMILY_MANIFEST_VNEXT.tsv"}

tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/turn-semantic-recovery-test.XXXXXX")
trap 'rm -rf -- "$tmp_dir"' EXIT

protocol="$tmp_dir/PROTOCOL.md"
subjects="$tmp_dir/SEMANTIC_RECOVERY_SUBJECTS_VNEXT.tsv"
classifier="$tmp_dir/SEMANTIC_RECOVERY_FAMILY_CLASSIFICATION_VNEXT.tsv"
state_manifest="$tmp_dir/STATE_FAMILY_MANIFEST_VNEXT.tsv"

for source_file in "$source_protocol" "$source_subjects" "$source_classifier" "$source_state_manifest"; do
  [[ -f "$source_file" ]] || {
    printf 'E_SEMANTIC_MUTATION_SOURCE: %s\n' "$source_file" >&2
    exit 1
  }
done

seed() {
  cp -- "$source_protocol" "$protocol"
  cp -- "$source_subjects" "$subjects"
  cp -- "$source_classifier" "$classifier"
  cp -- "$source_state_manifest" "$state_manifest"
}

run_gate() {
  SEMANTIC_PROTOCOL_PATH="$protocol" \
  SEMANTIC_SUBJECTS_PATH="$subjects" \
  SEMANTIC_FAMILY_CLASSIFICATION_PATH="$classifier" \
  SEMANTIC_STATE_MANIFEST_PATH="$state_manifest" \
    "$verifier"
}

expect_failure() {
  local label=$1
  local pattern=$2
  local output
  if output=$(run_gate 2>&1); then
    printf 'E_SEMANTIC_MUTATION_SURVIVED: %s\n' "$label" >&2
    exit 1
  fi
  if [[ "$output" != *"$pattern"* ]]; then
    printf 'E_SEMANTIC_MUTATION_REASON: %s expected=%s\n' "$label" "$pattern" >&2
    printf '%s\n' "$output" >&2
    exit 1
  fi
}

replace_subject_field() {
  local subject=$1
  local field=$2
  local value=$3
  awk -F '\t' -v OFS='\t' -v subject="$subject" -v field="$field" -v value="$value" \
    'NR > 1 && $2 == subject { $field=value } { print }' "$subjects" >"$tmp_dir/subjects.new"
  mv -- "$tmp_dir/subjects.new" "$subjects"
}

append_bundle_family() {
  local subject=$1
  local family=$2
  awk -F '\t' -v OFS='\t' -v subject="$subject" -v family="$family" \
    'NR > 1 && $2 == subject { $5=$5 "," family } { print }' "$subjects" >"$tmp_dir/subjects.new"
  mv -- "$tmp_dir/subjects.new" "$subjects"
}

remove_classifier_family() {
  local family=$1
  awk -F '\t' -v family="$family" 'NR == 1 || $2 != family' "$classifier" >"$tmp_dir/classifier.new"
  mv -- "$tmp_dir/classifier.new" "$classifier"
}

/bin/bash -n "$verifier" "$0"

seed
run_gate >/dev/null

seed
awk -F '\t' 'NR == 1 || $2 != "work_item_projection"' "$subjects" >"$tmp_dir/subjects.new"
mv -- "$tmp_dir/subjects.new" "$subjects"
expect_failure "subject row deletion" E_SEMANTIC_SUBJECT_ORACLE_COUNT

seed
duplicate_row=$(sed -n '2p' "$subjects")
printf '%s\n' "$duplicate_row" >>"$subjects"
expect_failure "subject duplicate" E_SEMANTIC_SUBJECT_DUPLICATE

seed
replace_subject_field runtime_launch 3 RuntimeLaunchIntentId+unreviewed_key_component
expect_failure "subject semantic cell drift" E_SEMANTIC_SUBJECT_ORACLE_DIGEST

seed
perl -0pi -e 's/^  \@semantic_subject\|work_item_projection\n//m' "$protocol"
expect_failure "Protocol union deletion" E_SEMANTIC_PROTOCOL_UNION_DRIFT

seed
perl -0pi -e 's/^  \@semantic_subject\|work_item_projection\n//m' "$protocol"
awk -F '\t' 'NR == 1 || $2 != "work_item_projection"' "$subjects" >"$tmp_dir/subjects.new"
mv -- "$tmp_dir/subjects.new" "$subjects"
expect_failure "coordinated Protocol and subject deletion" E_SEMANTIC_SUBJECT_ORACLE_COUNT

seed
perl -0pi -e 's/\@semantic_subject\|flow_run/\@semantic_subject|relationship/' "$protocol"
awk -F '\t' 'BEGIN { OFS="\t" } $2 == "flow_run" { $2="relationship" } { print }' \
  "$subjects" >"$tmp_dir/subjects.new"
mv -- "$tmp_dir/subjects.new" "$subjects"
awk -F '\t' 'BEGIN { OFS="\t" } $4 == "flow_run" { $4="relationship" } { print }' \
  "$classifier" >"$tmp_dir/classifier.new"
mv -- "$tmp_dir/classifier.new" "$classifier"
expect_failure "invented relationship subject" E_SEMANTIC_SUBJECT_KIND

seed
remove_classifier_family RuntimeAttachmentReceipt
expect_failure "classifier row deletion" E_SEMANTIC_CLASSIFIER_ORACLE_COUNT

seed
duplicate_row=$(sed -n '2p' "$classifier")
printf '%s\n' "$duplicate_row" >>"$classifier"
expect_failure "classifier duplicate" E_SEMANTIC_CLASSIFIER_DUPLICATE

seed
awk -F '\t' 'BEGIN { OFS="\t" } $2 == "RuntimeAttachmentReceipt" { $4="runtime_launch" } { print }' \
  "$classifier" >"$tmp_dir/classifier.new"
mv -- "$tmp_dir/classifier.new" "$classifier"
expect_failure "classifier subject drift" E_SEMANTIC_BUNDLE_CLASSIFIER_DRIFT

seed
awk -F '\t' 'BEGIN { OFS="\t" } $2 == "Node" { $3="deterministic_exclusion"; $4="none" } { print }' \
  "$classifier" >"$tmp_dir/classifier.new"
mv -- "$tmp_dir/classifier.new" "$classifier"
expect_failure "bundle reclassified as exclusion" E_SEMANTIC_CLASSIFIER_ORACLE_COUNT

seed
printf '%s\n' $'vNext\tNewSemanticFamily\tstate_family\tdurable\tWorkspace\tWorkspace(daemon_generation,WorkspaceId)' \
  >>"$state_manifest"
expect_failure "future durable Workspace family" E_SEMANTIC_WORKSPACE_ORACLE_COUNT

seed
awk -F '\t' 'NR == 1 || $2 != "AgentTopologyObservation"' "$state_manifest" >"$tmp_dir/state.new"
mv -- "$tmp_dir/state.new" "$state_manifest"
remove_classifier_family AgentTopologyObservation
expect_failure "coordinated Workspace family deletion" E_SEMANTIC_CLASSIFIER_ORACLE_COUNT

seed
awk -F '\t' 'BEGIN { OFS="\t" } $2 == "commit_proposal" { sub(/,CommitProposalAttempt/, "", $5) } { print }' \
  "$subjects" >"$tmp_dir/subjects.new"
mv -- "$tmp_dir/subjects.new" "$subjects"
expect_failure "cross-owner reference deletion" E_SEMANTIC_CROSS_OWNER_COUNT

seed
append_bundle_family commit_proposal AccountProfile
expect_failure "eleventh cross-owner reference" E_SEMANTIC_CROSS_OWNER_COUNT

seed
append_bundle_family commit_proposal CommitProposalSandboxHelper
expect_failure "ephemeral family in bundle" E_SEMANTIC_BUNDLE_EPHEMERAL

seed
append_bundle_family media_import MediaBlob
expect_failure "deterministic exclusion in bundle" E_SEMANTIC_BUNDLE_CLASSIFIER_DRIFT

seed
replace_subject_field node_aggregate 6 all_nodes
expect_failure "unconditional Node eligibility" E_SEMANTIC_NODE_ELIGIBILITY

seed
replace_subject_field flow_run 8 child_runtime_effects_inherit_flow_coordinator
expect_failure "Flow child inherits coordinator" E_SEMANTIC_INHERIT_TRANSFER_GRAPH

seed
replace_subject_field bulk_idle_restart 8 each_child_inherits_bulk_coordinator
expect_failure "bulk child inherits coordinator" E_SEMANTIC_INHERIT_TRANSFER_GRAPH

seed
replace_subject_field browser_navigation 8 transfer_to_node_aggregate
expect_failure "browser operation transfers to Node" E_SEMANTIC_INHERIT_TRANSFER_GRAPH

seed
replace_subject_field media_import 8 transfer_to_node_aggregate
expect_failure "media import transfers to Node" E_SEMANTIC_INHERIT_TRANSFER_GRAPH

seed
replace_subject_field runtime_launch 8 'created_live_Node_transfers_launch_to_node_aggregate;existing_reserved_Node_transfers_launch_to_node_aggregate;companion_parent_transfers_to_node'
expect_failure "existing reserved Node double transfer" E_SEMANTIC_INHERIT_TRANSFER_GRAPH

seed
replace_subject_field runtime_lifecycle 8 replacement_runtime_launch_transfers_to_node_aggregate
expect_failure "replacement runtime transfers instead of inherits" E_SEMANTIC_INHERIT_TRANSFER_GRAPH

seed
awk -F '\t' 'BEGIN { OFS="\t" } $2 == "RepositoryPublishIntent" { $5="Installation"; $6="Installation(daemon_generation)" } { print }' \
  "$state_manifest" >"$tmp_dir/state.new"
mv -- "$tmp_dir/state.new" "$state_manifest"
expect_failure "cross-owner coordinate drift" E_SEMANTIC_CROSS_OWNER_DRIFT

printf '%s\n' "SEMANTIC_RECOVERY_MUTATIONS_OK: baseline + 24/24 adversarial mutations"
