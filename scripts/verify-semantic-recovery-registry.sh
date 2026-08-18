#!/usr/bin/env bash
set -euo pipefail

export LC_ALL=C

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
protocol_path=${SEMANTIC_PROTOCOL_PATH:-"$repo_root/docs/PROTOCOL.md"}
subjects_path=${SEMANTIC_SUBJECTS_PATH:-"$repo_root/docs/SEMANTIC_RECOVERY_SUBJECTS_VNEXT.tsv"}
classifier_path=${SEMANTIC_FAMILY_CLASSIFICATION_PATH:-"$repo_root/docs/SEMANTIC_RECOVERY_FAMILY_CLASSIFICATION_VNEXT.tsv"}
state_manifest_path=${SEMANTIC_STATE_MANIFEST_PATH:-"$repo_root/docs/STATE_FAMILY_MANIFEST_VNEXT.tsv"}

# Independent reviewed freezes. These are deliberately not derived from the
# protocol, either TSV, or one another at verification time.
oracle_subject_count=26
oracle_subject_rows_sha256=8113f872963c34272b18c289fb7769e9b0e7c0de60d5ae59a94a54bbb5e6d759
oracle_workspace_family_count=108
oracle_classifier_row_count=108
oracle_bundle_count=65
oracle_exclusion_count=43
oracle_classifier_rows_sha256=247202cdad28ea377efb959f8de1dd95acdf8182e2c67341df25592dd65d3b46
oracle_cross_owner_count=10

case "${1:-}" in
  "") ;;
  *) echo "E_SEMANTIC_USAGE: usage: $0" >&2; exit 64 ;;
esac

die() {
  local code=$1
  shift
  printf '%s: %s\n' "$code" "$*" >&2
  exit 1
}

hash_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{ print $1 }'
  else
    die E_SEMANTIC_HASH_TOOL "sha256sum or shasum is required"
  fi
}

require_regular_file() {
  local label=$1
  local path=$2
  [[ -f "$path" && ! -L "$path" ]] || die "E_SEMANTIC_${label}_MISSING" "$path"
}

require_header() {
  local label=$1
  local path=$2
  local expected=$3
  [[ "$(head -n 1 "$path")" == "$expected" ]] || die "E_SEMANTIC_${label}_HEADER" "$path"
}

diff_or_die() {
  local code=$1
  local message=$2
  local expected=$3
  local actual=$4
  local output=$5
  if ! diff -u "$expected" "$actual" >"$output"; then
    printf '%s: %s\n' "$code" "$message" >&2
    sed -n '1,120p' "$output" >&2
    exit 1
  fi
}

require_regular_file PROTOCOL "$protocol_path"
require_regular_file SUBJECTS "$subjects_path"
require_regular_file CLASSIFIER "$classifier_path"
require_regular_file STATE_MANIFEST "$state_manifest_path"

subjects_header=$'schema_version\tsubject_kind\tkey_schema\trequired_fences\tfamily_bundle\teligibility_predicate\treservation_rule\tsuccess_transfer\trelease_proof'
classifier_header=$'schema_version\tfamily\tclassification\tsubject_kind\trationale'
state_manifest_header=$'schema_version\tname\tdeclaration_class\tlifetime\tstream_owner\towner_key'
require_header SUBJECTS "$subjects_path" "$subjects_header"
require_header CLASSIFIER "$classifier_path" "$classifier_header"
require_header STATE_MANIFEST "$state_manifest_path" "$state_manifest_header"

tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/turn-semantic-recovery.XXXXXX")
trap 'rm -rf -- "$tmp_dir"' EXIT

subject_rows="$tmp_dir/subject-rows.tsv"
subject_kinds="$tmp_dir/subject-kinds.txt"
bundle_pairs="$tmp_dir/bundle-pairs.tsv"
graph_rows="$tmp_dir/graph-rows.tsv"

awk -F '\t' -v rows="$subject_rows" -v kinds="$subject_kinds" \
  -v bundles="$bundle_pairs" -v graph="$graph_rows" '
BEGIN { OFS="\t" }
NR == 1 { next }
{
  if (NF != 9) {
    printf "E_SEMANTIC_SUBJECT_COLUMNS: line=%d fields=%d\n", NR, NF > "/dev/stderr"
    bad=1
    next
  }
  if ($1 != "vNext") {
    printf "E_SEMANTIC_SUBJECT_VERSION: line=%d value=%s\n", NR, $1 > "/dev/stderr"
    bad=1
  }
  if ($2 !~ /^[a-z][a-z0-9_]*$/ || $2 ~ /^(default|misc|other|wildcard|relationship|terminal_continuity)$/) {
    printf "E_SEMANTIC_SUBJECT_KIND: line=%d value=%s\n", NR, $2 > "/dev/stderr"
    bad=1
  }
  if (seen_subject[$2]++) {
    printf "E_SEMANTIC_SUBJECT_DUPLICATE: %s\n", $2 > "/dev/stderr"
    bad=1
  }
  for (column=3; column<=9; column++) {
    if ($column == "") {
      printf "E_SEMANTIC_SUBJECT_EMPTY: line=%d column=%d\n", NR, column > "/dev/stderr"
      bad=1
    }
  }
  if ($6 ~ /(^|_)(all|every|unconditional)_?(nodes?|node_aggregates?)(_|$)/) {
    printf "E_SEMANTIC_NODE_ELIGIBILITY: line=%d value=%s\n", NR, $6 > "/dev/stderr"
    bad=1
  }
  family_count=split($5, family, /,/)
  delete row_family
  for (family_index=1; family_index<=family_count; family_index++) {
    if (family[family_index] !~ /^[A-Z][A-Za-z0-9]*$/) {
      printf "E_SEMANTIC_BUNDLE_FAMILY: line=%d value=%s\n", NR, family[family_index] > "/dev/stderr"
      bad=1
    }
    if (row_family[family[family_index]]++) {
      printf "E_SEMANTIC_BUNDLE_DUPLICATE: subject=%s family=%s\n", $2, family[family_index] > "/dev/stderr"
      bad=1
    }
    print family[family_index], $2 > bundles
  }
  print $0 > rows
  print $2 > kinds
  print $2, $7, $8 > graph
}
END {
  if (NR == 1) {
    print "E_SEMANTIC_SUBJECT_EMPTY: no subject rows" > "/dev/stderr"
    bad=1
  }
  exit bad
}
' "$subjects_path"

sort -t $'\t' -k2,2 "$subject_rows" -o "$subject_rows"
sort "$subject_kinds" -o "$subject_kinds"
sort -t $'\t' -k1,1 -k2,2 "$bundle_pairs" -o "$bundle_pairs"
sort -t $'\t' -k1,1 "$graph_rows" -o "$graph_rows"

actual_subject_count=$(wc -l <"$subject_rows" | tr -d '[:space:]')
[[ "$actual_subject_count" == "$oracle_subject_count" ]] || \
  die E_SEMANTIC_SUBJECT_ORACLE_COUNT "expected=$oracle_subject_count actual=$actual_subject_count"

protocol_subjects="$tmp_dir/protocol-subjects.txt"
awk '
function fail(code, line, detail) {
  printf "%s: line=%d %s\n", code, line, detail > "/dev/stderr"
  exit 2
}
/^SemanticRecoverySubjectRegistry\.vNext = \{$/ {
  if (started++) fail("E_SEMANTIC_PROTOCOL_UNION_SHAPE", NR, "duplicate registry")
  in_registry=1
  next
}
in_registry && /^}$/ {
  in_registry=0
  ended++
  next
}
in_registry {
  if ($0 !~ /^  @semantic_subject\|[a-z][a-z0-9_]*$/) {
    fail("E_SEMANTIC_PROTOCOL_UNION_TOKEN", NR, $0)
  }
  name=$0
  sub(/^  @semantic_subject\|/, "", name)
  if (seen[name]++) fail("E_SEMANTIC_PROTOCOL_UNION_DUPLICATE", NR, name)
  print name
  next
}
!in_registry && /^  @semantic_subject\|/ {
  fail("E_SEMANTIC_PROTOCOL_UNION_ORPHAN", NR, $0)
}
END {
  if (started != 1 || ended != 1 || in_registry) {
    printf "E_SEMANTIC_PROTOCOL_UNION_SHAPE: starts=%d ends=%d open=%d\n", started+0, ended+0, in_registry+0 > "/dev/stderr"
    exit 2
  }
}
' "$protocol_path" | sort >"$protocol_subjects"

diff_or_die E_SEMANTIC_PROTOCOL_UNION_DRIFT \
  "Protocol union and subject TSV are not bijective" \
  "$subject_kinds" "$protocol_subjects" "$tmp_dir/protocol-union.diff"

classifier_rows="$tmp_dir/classifier-rows.tsv"
classifier_families="$tmp_dir/classifier-families.txt"
classifier_bundle_pairs="$tmp_dir/classifier-bundle-pairs.tsv"
classifier_exclusions="$tmp_dir/classifier-exclusions.txt"

awk -F '\t' -v rows="$classifier_rows" -v families="$classifier_families" \
  -v pairs="$classifier_bundle_pairs" -v exclusions="$classifier_exclusions" '
BEGIN { OFS="\t" }
NR == 1 { next }
{
  if (NF != 5) {
    printf "E_SEMANTIC_CLASSIFIER_COLUMNS: line=%d fields=%d\n", NR, NF > "/dev/stderr"
    bad=1
    next
  }
  if ($1 != "vNext") {
    printf "E_SEMANTIC_CLASSIFIER_VERSION: line=%d value=%s\n", NR, $1 > "/dev/stderr"
    bad=1
  }
  if ($2 !~ /^[A-Z][A-Za-z0-9]*$/) {
    printf "E_SEMANTIC_CLASSIFIER_FAMILY: line=%d value=%s\n", NR, $2 > "/dev/stderr"
    bad=1
  }
  if (seen_family[$2]++) {
    printf "E_SEMANTIC_CLASSIFIER_DUPLICATE: %s\n", $2 > "/dev/stderr"
    bad=1
  }
  if ($3 == "bundle") {
    bundle_count++
    if ($4 !~ /^[a-z][a-z0-9_]*$/ || $4 == "none") {
      printf "E_SEMANTIC_CLASSIFIER_SUBJECT: line=%d value=%s\n", NR, $4 > "/dev/stderr"
      bad=1
    }
    print $2, $4 > pairs
  } else if ($3 == "deterministic_exclusion") {
    exclusion_count++
    if ($4 != "none") {
      printf "E_SEMANTIC_CLASSIFIER_EXCLUSION: line=%d value=%s\n", NR, $4 > "/dev/stderr"
      bad=1
    }
    print $2 > exclusions
  } else {
    printf "E_SEMANTIC_CLASSIFIER_CLASS: line=%d value=%s\n", NR, $3 > "/dev/stderr"
    bad=1
  }
  if ($5 == "") {
    printf "E_SEMANTIC_CLASSIFIER_RATIONALE: line=%d\n", NR > "/dev/stderr"
    bad=1
  }
  print $0 > rows
  print $2 > families
}
END {
  if (NR == 1) {
    print "E_SEMANTIC_CLASSIFIER_EMPTY: no classifier rows" > "/dev/stderr"
    bad=1
  }
  printf "%d\t%d\n", bundle_count+0, exclusion_count+0 > (rows ".counts")
  exit bad
}
' "$classifier_path"

sort -t $'\t' -k2,2 "$classifier_rows" -o "$classifier_rows"
sort "$classifier_families" -o "$classifier_families"
sort -t $'\t' -k1,1 -k2,2 "$classifier_bundle_pairs" -o "$classifier_bundle_pairs"
sort "$classifier_exclusions" -o "$classifier_exclusions"

actual_classifier_count=$(wc -l <"$classifier_rows" | tr -d '[:space:]')
IFS=$'\t' read -r actual_bundle_count actual_exclusion_count <"$classifier_rows.counts"
[[ "$actual_classifier_count" == "$oracle_classifier_row_count" && \
   "$actual_bundle_count" == "$oracle_bundle_count" && \
   "$actual_exclusion_count" == "$oracle_exclusion_count" ]] || \
  die E_SEMANTIC_CLASSIFIER_ORACLE_COUNT \
    "expected=$oracle_classifier_row_count/$oracle_bundle_count/$oracle_exclusion_count actual=$actual_classifier_count/$actual_bundle_count/$actual_exclusion_count"

manifest_coordinates="$tmp_dir/manifest-coordinates.tsv"
workspace_families="$tmp_dir/workspace-families.txt"
awk -F '\t' -v coordinates="$manifest_coordinates" -v workspace="$workspace_families" '
BEGIN { OFS="\t" }
NR == 1 { next }
{
  if (NF != 6) {
    printf "E_SEMANTIC_STATE_MANIFEST_COLUMNS: line=%d fields=%d\n", NR, NF > "/dev/stderr"
    bad=1
    next
  }
  if ($1 != "vNext" || $2 !~ /^[A-Z][A-Za-z0-9]*$/ ||
      $3 !~ /^(state_family|request_value)$/) {
    printf "E_SEMANTIC_STATE_MANIFEST_ROW: line=%d\n", NR > "/dev/stderr"
    bad=1
  }
  if (seen[$2]++) {
    printf "E_SEMANTIC_STATE_MANIFEST_DUPLICATE: %s\n", $2 > "/dev/stderr"
    bad=1
  }
  if ($3 == "state_family") {
    if ($4 !~ /^(durable|ephemeral)$/ ||
        $5 !~ /^(Installation|Workspace|ExecutionTarget|TaggedOwner|ephemeral)$/) {
      printf "E_SEMANTIC_STATE_MANIFEST_COORDINATE: line=%d\n", NR > "/dev/stderr"
      bad=1
    }
    if (($4 == "durable" && $5 == "ephemeral") ||
        ($4 == "ephemeral" && $5 != "ephemeral")) {
      printf "E_SEMANTIC_STATE_MANIFEST_COORDINATE: line=%d\n", NR > "/dev/stderr"
      bad=1
    }
    print $2, $4, $5 > coordinates
    if ($4 == "durable" && $5 == "Workspace") print $2 > workspace
  }
}
END { exit bad }
' "$state_manifest_path"
sort -t $'\t' -k1,1 "$manifest_coordinates" -o "$manifest_coordinates"
sort "$workspace_families" -o "$workspace_families"

actual_workspace_family_count=$(wc -l <"$workspace_families" | tr -d '[:space:]')
[[ "$actual_workspace_family_count" == "$oracle_workspace_family_count" ]] || \
  die E_SEMANTIC_WORKSPACE_ORACLE_COUNT \
    "expected=$oracle_workspace_family_count actual=$actual_workspace_family_count"

diff_or_die E_SEMANTIC_WORKSPACE_CLASSIFIER_DRIFT \
  "durable Workspace manifest and classifier are not bijective" \
  "$workspace_families" "$classifier_families" "$tmp_dir/workspace-classifier.diff"

workspace_bundle_pairs="$tmp_dir/workspace-bundle-pairs.tsv"
cross_owner_rows="$tmp_dir/cross-owner.tsv"
awk -F '\t' -v workspace="$workspace_bundle_pairs" -v cross="$cross_owner_rows" '
NR == FNR {
  lifetime[$1]=$2
  owner[$1]=$3
  next
}
{
  family=$1
  subject=$2
  if (!(family in lifetime)) {
    printf "E_SEMANTIC_BUNDLE_UNDECLARED: family=%s subject=%s\n", family, subject > "/dev/stderr"
    bad=1
    next
  }
  if (lifetime[family] == "ephemeral") {
    printf "E_SEMANTIC_BUNDLE_EPHEMERAL: family=%s subject=%s\n", family, subject > "/dev/stderr"
    bad=1
  } else if (owner[family] == "Workspace") {
    print family "\t" subject > workspace
  } else {
    print family "\t" subject "\t" owner[family] > cross
  }
}
END { exit bad }
' "$manifest_coordinates" "$bundle_pairs"
sort -t $'\t' -k1,1 -k2,2 "$workspace_bundle_pairs" -o "$workspace_bundle_pairs"
sort -t $'\t' -k1,1 -k2,2 "$cross_owner_rows" -o "$cross_owner_rows"

diff_or_die E_SEMANTIC_BUNDLE_CLASSIFIER_DRIFT \
  "Workspace bundle edges and bundle classifications are not bijective" \
  "$classifier_bundle_pairs" "$workspace_bundle_pairs" "$tmp_dir/bundle-classifier.diff"

if comm -12 "$classifier_exclusions" <(cut -f1 "$workspace_bundle_pairs" | sort -u) | grep -q .; then
  die E_SEMANTIC_EXCLUSION_BUNDLED "a deterministic exclusion appears in a subject bundle"
fi

expected_cross_owner="$tmp_dir/expected-cross-owner.tsv"
cat >"$expected_cross_owner" <<'CROSS_OWNER'
CommitProposalAttempt	commit_proposal	Installation
NativeJob	native_job_projection	ExecutionTarget
NativeJobCreateIntent	native_job_create	ExecutionTarget
NativeJobInvocationReceipt	native_job_mutation	ExecutionTarget
NativeJobIteration	native_job_projection	ExecutionTarget
NativeJobMutationIntent	native_job_mutation	ExecutionTarget
PortableImportIntent	portable_import_destination	Installation
PortableImportReceipt	portable_import_destination	Installation
RepositoryPublishIntent	repository_publish	ExecutionTarget
RepositoryPublishReceipt	repository_publish	ExecutionTarget
CROSS_OWNER

actual_cross_owner_count=$(wc -l <"$cross_owner_rows" | tr -d '[:space:]')
[[ "$actual_cross_owner_count" == "$oracle_cross_owner_count" ]] || \
  die E_SEMANTIC_CROSS_OWNER_COUNT \
    "expected=$oracle_cross_owner_count actual=$actual_cross_owner_count"
diff_or_die E_SEMANTIC_CROSS_OWNER_DRIFT \
  "cross-owner bundle references differ from the reviewed closed set" \
  "$expected_cross_owner" "$cross_owner_rows" "$tmp_dir/cross-owner.diff"

if ! awk -F '\t' '$2 == "node_aggregate" {
    found=1
    if ($6 != "live_unknown_or_orphaned_attempt_or_current_external_conversation") exit 2
  }
  END { if (!found) exit 3 }
' "$subjects_path"; then
  die E_SEMANTIC_NODE_ELIGIBILITY "node_aggregate must remain predicate-bounded"
fi

# This table is a second, structural oracle. It makes allocation, inheritance,
# transfer, and the absence of a transfer explicit instead of relying only on a
# whole-row digest.
expected_graph="$tmp_dir/expected-graph.tsv"
cat >"$expected_graph" <<'GRAPH'
agent_browser_action	allocate_before_canonical_action	child_browser_operation_inherits_agent_browser_action
browser_download_quarantine	allocate_before_body_byte_one	transfer_to_transfer_on_ticket_handoff
browser_navigation	allocate_before_load_or_inherit(agent_browser_action)	none
browser_node_creation	allocate_before_graph_or_renderer_effect_or_inherit(agent_browser_action)	none
bulk_idle_restart	allocate_before_first_stop	each_child_uses_canonical_runtime_lifecycle_rule_and_never_inherits_bulk_coordinator
commit_proposal	allocate_before_helper_or_broker_effect	workspace_delete_moves_same_reservation
companion_agent_launch	allocate_before_checkout_process_or_graph	runtime_launch_inherits_then_created_live_transfers_to_node_aggregate
document_print	allocate_before_spool_or_native_print	none
eco_hibernate	inherit(node_aggregate)_before_exit_or_wake	none
effect_delivery	allocate_before_first_possible_write	none
flow_run	allocate_before_first_run_effect	child_runtime_effects_use_canonical_node_or_launch_rule_and_never_inherit_flow_coordinator
media_import	allocate_before_source_read_or_chunk	none
native_job_create	allocate_before_provider_effect	bound_or_retained_creation_view_transfers_to_native_job_projection
native_job_mutation	inherit(native_job_projection)_before_provider_effect	none
native_job_projection	allocate_before_adopt_or_publish_or_transfer(native_job_create)	none
node_aggregate	allocate_before_existing_inert_node_becomes_eligible_or_transfer(runtime_launch_or_companion_agent_launch)	none
portable_export	allocate_before_commit_or_write	none
portable_import_destination	allocate_at_fresh_destination_commit_before_remint_never_during_End	none
repository_publish	allocate_before_create_remote_push_or_config	none
runtime_launch	if_destination_Node_already_owns_node_aggregate_inherit;otherwise_allocate_before_identity_or_spawn;companion_child_inherit(companion_agent_launch)	created_live_Node_transfers_launch_to_node_aggregate;existing_reserved_Node_no_transfer;companion_parent_transfers_to_node
runtime_lifecycle	inherit(node_aggregate)_before_signal	replacement_runtime_launch_inherits_same_node_aggregate_and_never_transfers
transfer	allocate_before_source_IO_or_transfer(browser_download_quarantine)	none
web_preview_load	allocate_before_DNS_or_request	none
work_item_create	allocate_before_provider_effect	bound_or_retained_creation_projection_transfers_to_work_item_projection
work_item_mutation	inherit(work_item_projection)_before_provider_effect	none
work_item_projection	allocate_before_import_adopt_publish_or_transfer(work_item_create)	none
GRAPH
sort -t $'\t' -k1,1 "$expected_graph" -o "$expected_graph"
diff_or_die E_SEMANTIC_INHERIT_TRANSFER_GRAPH \
  "allocate/inherit/transfer graph differs from the reviewed closed graph" \
  "$expected_graph" "$graph_rows" "$tmp_dir/graph.diff"

actual_subject_rows_sha256=$(hash_file "$subject_rows")
[[ "$actual_subject_rows_sha256" == "$oracle_subject_rows_sha256" ]] || \
  die E_SEMANTIC_SUBJECT_ORACLE_DIGEST \
    "expected=$oracle_subject_rows_sha256 actual=$actual_subject_rows_sha256"

actual_classifier_rows_sha256=$(hash_file "$classifier_rows")
[[ "$actual_classifier_rows_sha256" == "$oracle_classifier_rows_sha256" ]] || \
  die E_SEMANTIC_CLASSIFIER_ORACLE_DIGEST \
    "expected=$oracle_classifier_rows_sha256 actual=$actual_classifier_rows_sha256"

printf '%s\n' \
  "SEMANTIC_RECOVERY_REGISTRY_OK: 26 subjects; 108 durable Workspace families (65 bundled, 43 excluded); 10 durable cross-owner references"
