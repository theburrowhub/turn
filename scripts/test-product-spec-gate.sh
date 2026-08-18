#!/usr/bin/env bash
set -euo pipefail

export LC_ALL=C

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
scratch=$(mktemp -d "${TMPDIR:-/tmp}/turn-product-spec-mutations.XXXXXX")
trap 'rm -rf "$scratch"' EXIT

hash_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

hash_stream() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum | awk '{print $1}'
  else
    shasum -a 256 | awk '{print $1}'
  fi
}

configure_repo() {
  fixture=$1
  git -C "$fixture" config user.name 'Turn gate fixture'
  git -C "$fixture" config user.email 'turn-gate-fixture@invalid'
  git -C "$fixture" config commit.gpgSign false
  git -C "$fixture" config core.hooksPath /dev/null
}

seed_case() {
  case_dir=$1
  object_format=${2:-sha1}
  mkdir -p "$case_dir/docs" "$case_dir/scripts" "$case_dir/.github/workflows"
  cp "$repo_root/README.md" "$repo_root/PRODUCT.md" "$repo_root/ARCHITECTURE.md" \
    "$repo_root/ROADMAP.md" "$repo_root/DECISIONS.md" "$repo_root/Makefile" "$case_dir/"
  [[ ! -f "$repo_root/.gitignore" ]] || cp "$repo_root/.gitignore" "$case_dir/"
  cp -R "$repo_root/docs/." "$case_dir/docs/"
  cp "$repo_root/scripts/verify-product-spec.sh" \
    "$repo_root/scripts/verify-product-capability-source.sh" \
    "$repo_root/scripts/verify-product-completion.sh" \
    "$repo_root/scripts/verify-operation-registry.sh" \
    "$repo_root/scripts/test-operation-registry-gate.sh" \
    "$repo_root/scripts/verify-semantic-recovery-registry.sh" \
    "$repo_root/scripts/test-semantic-recovery-registry-gate.sh" \
    "$repo_root/scripts/verify-state-family-manifest.sh" \
    "$repo_root/scripts/test-state-family-manifest-gate.sh" \
    "$repo_root/scripts/test-product-spec-gate.sh" "$case_dir/scripts/"
  cp "$repo_root/.github/workflows/ci.yml" "$case_dir/.github/workflows/"
  git -C "$case_dir" init -q --object-format="$object_format"
  configure_repo "$case_dir"
  git -C "$case_dir" add .
  git -C "$case_dir" commit -qm 'frozen specification fixture'
}

clone_case() {
  source_dir=$1
  case_dir=$2
  git clone -q "$source_dir" "$case_dir"
  configure_repo "$case_dir"
}

commit_case() {
  case_dir=$1
  message=$2
  git -C "$case_dir" add -A
  git -C "$case_dir" commit -qm "$message"
}

run_spec() {
  case_dir=$1
  TURN_EXPECTED_PRODUCT_SPEC_AUTHORITY_SHA256="$base_pin" \
    /bin/bash "$case_dir/scripts/verify-product-spec.sh" verify
}

run_completion() {
  case_dir=$1
  TURN_EXPECTED_PRODUCT_SPEC_AUTHORITY_SHA256="$base_pin" \
    /bin/bash "$case_dir/scripts/verify-product-completion.sh"
}

expect_rejected() {
  name=$1
  expected_code=$2
  shift 2
  if output=$("$@" 2>&1); then
    echo "product-spec-mutations: $name was incorrectly accepted" >&2
    exit 1
  fi
  if ! printf '%s\n' "$output" | grep -Fq ": $expected_code:"; then
    echo "product-spec-mutations: $name failed for the wrong reason; expected $expected_code" >&2
    printf '%s\n' "$output" >&2
    exit 1
  fi
}

replace_file() {
  source_file=$1
  replacement_file=$2
  mv "$replacement_file" "$source_file"
}

refreeze_case() {
  case_dir=$1
  /bin/bash "$case_dir/scripts/verify-product-spec.sh" --emit-manifest >"$case_dir/docs/manifest.new"
  replace_file "$case_dir/docs/PRODUCT_REQUIREMENTS_V1.manifest" "$case_dir/docs/manifest.new"
  /bin/bash "$case_dir/scripts/verify-product-spec.sh" --emit-authority >"$case_dir/docs/authority.new"
  replace_file "$case_dir/docs/PRODUCT_SPEC_V1.authority" "$case_dir/docs/authority.new"
  hash_file "$case_dir/docs/PRODUCT_SPEC_V1.authority" >"$case_dir/docs/PRODUCT_SPEC_V1.sha256"
}

seed_source_verifier_fixture() {
  case_dir=$1
  source_dir=$2
  mkdir -p "$case_dir/scripts" "$case_dir/docs" "$source_dir/src/core" \
    "$source_dir/src/renderer/lib" "$source_dir/src/shared/agents"
  cp "$repo_root/scripts/verify-product-capability-source.sh" "$case_dir/scripts/"

  git -C "$source_dir" init -q --object-format=sha1
  configure_repo "$source_dir"
  printf '%s\n' 'source fixture root' >"$source_dir/README.md"
  printf '%s\n' 'export const evidence = true' >"$source_dir/src/evidence.ts"
  printf '%s\n' 'export const PTY_DEVICE_HEADROOM = 4' >"$source_dir/src/core/pty-devices.ts"
  printf '%s\n' \
    'export const HIDEABLE_MENU_ITEMS = [' \
    "  { id: 'group' }," \
    "  { id: 'remove-from-group' }," \
    "  { id: 'colors' }," \
    "  { id: 'duplicate' }," \
    "  { id: 'collapse' }," \
    "  { id: 'markdown-view' }," \
    "  { id: 'refresh-terminal' }" \
    ']' \
    'export const HIDEABLE_HEADER_BUTTONS = [' \
    "  { id: 'refresh' }," \
    "  { id: 'mic' }," \
    "  { id: 'ai-name' }," \
    "  { id: 'comments' }" \
    ']' >"$source_dir/src/renderer/lib/ui-visibility.ts"
  printf '%s\n' '.fixture-runtime-surface { display: block; }' >"$source_dir/src/renderer/styles.css"
  printf '%s\n' 'export const IPC = {' "  ping: 'fixture:ping'" '}' >"$source_dir/src/shared/ipc.ts"
  printf '%s\n' "export type NodeKind = 'fixture'" \
    'export interface Settings {' \
    '  fontSize: number' \
    '}' \
    "export type WorkspaceMigrationKind = 'exec' | 'v2'" >"$source_dir/src/shared/types.ts"
  printf '%s\n' "export type BuiltinAgentId = 'fixture_agent'" \
    "export const SUBAGENT_CAPABLE = ['fixture_agent'] as const" \
    >"$source_dir/src/shared/agents/config.ts"
  git -C "$source_dir" add .
  git -C "$source_dir" commit -qm 'source evidence fixture'

  source_snapshot=$(git -C "$source_dir" rev-parse HEAD)
  source_tree=$(git -C "$source_dir" ls-tree -r --full-tree "$source_snapshot" | hash_stream)
  root_digest=$(hash_file "$source_dir/README.md")
  evidence_digest=$(hash_file "$source_dir/src/evidence.ts")
  pty_devices_digest=$(hash_file "$source_dir/src/core/pty-devices.ts")
  ui_visibility_digest=$(hash_file "$source_dir/src/renderer/lib/ui-visibility.ts")
  renderer_css_digest=$(hash_file "$source_dir/src/renderer/styles.css")
  ipc_digest=$(hash_file "$source_dir/src/shared/ipc.ts")
  types_digest=$(hash_file "$source_dir/src/shared/types.ts")
  agents_digest=$(hash_file "$source_dir/src/shared/agents/config.ts")
  {
    printf '%s\n' '# product-capability-coverage-version: 1'
    printf '# source-snapshot: %s\n' "$source_snapshot"
    printf '# source-tree-sha256: %s\n' "$source_tree"
    printf '%s\n' '# source-tree-digest-algorithm: sha256(git ls-tree -r --full-tree source-snapshot)'
    printf '%s\n' '# digest-algorithm: sha256(raw bytes of evidence_locator at source-snapshot)'
    printf '%s\n' '# dispositions: adopted adapted rejected irrelevant'
    printf '%s\n' '# expected-feature-count: 2'
    printf '%s\n' $'feature_id\tcapability_key\tdescription\tdisposition\trequirement\tacceptance\tdecision\tevidence_locator\tevidence_sha256\trationale'
    printf 'CAP-001\tfixture_root\tRoot evidence.\tadopted\tPRD-OUT-001\tACP-OUT-001\tADR-059\tREADME.md\t%s\tFixture root is verified.\n' "$root_digest"
    printf 'CAP-002\tfixture_blob\tSecond responsibility in shared root evidence.\tadapted\tPRD-HIE-001\tACP-HIE-001\tADR-059\tREADME.md\t%s\tThe shared evidence module must retain both normalized relations.\n' "$root_digest"
  } >"$case_dir/docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv"
  {
    printf '%s\n' '# product-capability-source-census-version: 1'
    printf '# source-snapshot: %s\n' "$source_snapshot"
    printf '%s\n' '# candidate-selector-version: 1'
    printf '%s\n' '# expected-module-count: 8'
    printf '%s\n' '# expected-registry-count: 19'
    printf '%s\n' '# expected-candidate-count: 27'
    printf '%s\n' '# selector-v1 fixture'
    printf '%s\n' $'candidate_id\tcandidate_kind\tsource_locator\tsurface_key\tsource_blob_sha256'
    printf 'CEN-0001\tmodule\tREADME.md\t-\t%s\n' "$root_digest"
    printf 'CEN-0002\tmodule\tsrc/core/pty-devices.ts\t-\t%s\n' "$pty_devices_digest"
    printf 'CEN-0003\tmodule\tsrc/evidence.ts\t-\t%s\n' "$evidence_digest"
    printf 'CEN-0004\tmodule\tsrc/renderer/lib/ui-visibility.ts\t-\t%s\n' "$ui_visibility_digest"
    printf 'CEN-0005\tmodule\tsrc/renderer/styles.css\t-\t%s\n' "$renderer_css_digest"
    printf 'CEN-0006\tmodule\tsrc/shared/agents/config.ts\t-\t%s\n' "$agents_digest"
    printf 'CEN-0007\tmodule\tsrc/shared/ipc.ts\t-\t%s\n' "$ipc_digest"
    printf 'CEN-0008\tmodule\tsrc/shared/types.ts\t-\t%s\n' "$types_digest"
    printf 'CEN-0009\tregistry_agent_adapter\tsrc/shared/agents/config.ts\tfixture_agent\t%s\n' "$agents_digest"
    printf 'CEN-0010\tregistry_closed_set\tsrc/core/pty-devices.ts\tPTY_DEVICE_HEADROOM=4\t%s\n' "$pty_devices_digest"
    printf 'CEN-0011\tregistry_closed_set\tsrc/renderer/lib/ui-visibility.ts\tHIDEABLE_HEADER_BUTTONS=header:ai-name->generate_name\t%s\n' "$ui_visibility_digest"
    printf 'CEN-0012\tregistry_closed_set\tsrc/renderer/lib/ui-visibility.ts\tHIDEABLE_HEADER_BUTTONS=header:comments->comments\t%s\n' "$ui_visibility_digest"
    printf 'CEN-0013\tregistry_closed_set\tsrc/renderer/lib/ui-visibility.ts\tHIDEABLE_HEADER_BUTTONS=header:mic->voice_dictation\t%s\n' "$ui_visibility_digest"
    printf 'CEN-0014\tregistry_closed_set\tsrc/renderer/lib/ui-visibility.ts\tHIDEABLE_HEADER_BUTTONS=header:refresh->header_refresh\t%s\n' "$ui_visibility_digest"
    printf 'CEN-0015\tregistry_closed_set\tsrc/renderer/lib/ui-visibility.ts\tHIDEABLE_MENU_ITEMS=menu:collapse->collapse_expand\t%s\n' "$ui_visibility_digest"
    printf 'CEN-0016\tregistry_closed_set\tsrc/renderer/lib/ui-visibility.ts\tHIDEABLE_MENU_ITEMS=menu:colors->colour\t%s\n' "$ui_visibility_digest"
    printf 'CEN-0017\tregistry_closed_set\tsrc/renderer/lib/ui-visibility.ts\tHIDEABLE_MENU_ITEMS=menu:duplicate->duplicate\t%s\n' "$ui_visibility_digest"
    printf 'CEN-0018\tregistry_closed_set\tsrc/renderer/lib/ui-visibility.ts\tHIDEABLE_MENU_ITEMS=menu:group->group_selection\t%s\n' "$ui_visibility_digest"
    printf 'CEN-0019\tregistry_closed_set\tsrc/renderer/lib/ui-visibility.ts\tHIDEABLE_MENU_ITEMS=menu:markdown-view->markdown_projection\t%s\n' "$ui_visibility_digest"
    printf 'CEN-0020\tregistry_closed_set\tsrc/renderer/lib/ui-visibility.ts\tHIDEABLE_MENU_ITEMS=menu:refresh-terminal->refresh_terminal\t%s\n' "$ui_visibility_digest"
    printf 'CEN-0021\tregistry_closed_set\tsrc/renderer/lib/ui-visibility.ts\tHIDEABLE_MENU_ITEMS=menu:remove-from-group->remove_from_group\t%s\n' "$ui_visibility_digest"
    printf 'CEN-0022\tregistry_closed_set\tsrc/shared/agents/config.ts\tSUBAGENT_CAPABLE=fixture_agent\t%s\n' "$agents_digest"
    printf 'CEN-0023\tregistry_closed_union\tsrc/shared/types.ts\tWorkspaceMigrationKind=exec\t%s\n' "$types_digest"
    printf 'CEN-0024\tregistry_closed_union\tsrc/shared/types.ts\tWorkspaceMigrationKind=v2\t%s\n' "$types_digest"
    printf 'CEN-0025\tregistry_ipc\tsrc/shared/ipc.ts\tping\t%s\n' "$ipc_digest"
    printf 'CEN-0026\tregistry_node_kind\tsrc/shared/types.ts\tfixture\t%s\n' "$types_digest"
    printf 'CEN-0027\tregistry_setting_key\tsrc/shared/types.ts\tfontSize\t%s\n' "$types_digest"
  } >"$case_dir/docs/PRODUCT_CAPABILITY_SOURCE_CENSUS_V1.tsv"
  {
    printf '%s\n' '# product-capability-source-mapping-version: 1'
    printf '# source-snapshot: %s\n' "$source_snapshot"
    printf '%s\n' '# expected-candidate-count: 27'
    printf '%s\n' '# expected-mapping-count: 28'
    printf '%s\n' '# resolutions: mapped supporting'
    printf '%s\n' '# mapping-bases: ledger_evidence closed_registry manual_source_audit supporting_module_audit'
    printf '%s\n' $'mapping_id\tcandidate_id\tresolution\tfeature_id\tfeature_disposition\tmapping_basis\tcandidate_rationale'
    printf '%s\n' $'MAP-0001\tCEN-0001\tmapped\tCAP-001\tadopted\tledger_evidence\tREADME.md is the evidence anchor for CAP-001 (fixture_root).'
    printf '%s\n' $'MAP-0002\tCEN-0001\tmapped\tCAP-002\tadapted\tledger_evidence\tREADME.md is the shared evidence anchor for CAP-002 (fixture_blob).'
    printf '%s\n' $'MAP-0003\tCEN-0002\tsupporting\tCAP-001\tadopted\tsupporting_module_audit\tsrc/core/pty-devices.ts supports CAP-001 (fixture_root) and adds no independent capability beyond the closed constant registry.'
    printf '%s\n' $'MAP-0004\tCEN-0003\tsupporting\tCAP-002\tadapted\tsupporting_module_audit\tsrc/evidence.ts supports CAP-002 (fixture_blob) and adds no independent capability beyond the evidence root.'
    printf '%s\n' $'MAP-0005\tCEN-0004\tsupporting\tCAP-001\tadopted\tsupporting_module_audit\tsrc/renderer/lib/ui-visibility.ts supports CAP-001 (fixture_root) and adds no independent capability beyond the closed visibility registry.'
    printf '%s\n' $'MAP-0006\tCEN-0005\tsupporting\tCAP-001\tadopted\tsupporting_module_audit\tsrc/renderer/styles.css supports CAP-001 (fixture_root) and adds no independent capability beyond the runtime presentation surface.'
    printf '%s\n' $'MAP-0007\tCEN-0006\tsupporting\tCAP-002\tadapted\tsupporting_module_audit\tsrc/shared/agents/config.ts supports CAP-002 (fixture_blob) and adds no independent capability beyond the closed adapter registries.'
    printf '%s\n' $'MAP-0008\tCEN-0007\tsupporting\tCAP-001\tadopted\tsupporting_module_audit\tsrc/shared/ipc.ts supports CAP-001 (fixture_root) and adds no independent capability beyond the closed IPC registry.'
    printf '%s\n' $'MAP-0009\tCEN-0008\tsupporting\tCAP-001\tadopted\tsupporting_module_audit\tsrc/shared/types.ts supports CAP-001 (fixture_root) and adds no independent capability beyond its closed type registries.'
    printf '%s\n' $'MAP-0010\tCEN-0009\tmapped\tCAP-002\tadapted\tclosed_registry\tfixture_agent is a closed adapter surface mapped to CAP-002 (fixture_blob).'
    printf '%s\n' $'MAP-0011\tCEN-0010\tmapped\tCAP-001\tadopted\tclosed_registry\tPTY_DEVICE_HEADROOM=4 is a closed constant surface mapped to CAP-001 (fixture_root).'
    printf '%s\n' $'MAP-0012\tCEN-0011\tmapped\tCAP-001\tadopted\tclosed_registry\tHIDEABLE_HEADER_BUTTONS=header:ai-name->generate_name is a closed visibility relation mapped to CAP-001 (fixture_root).'
    printf '%s\n' $'MAP-0013\tCEN-0012\tmapped\tCAP-001\tadopted\tclosed_registry\tHIDEABLE_HEADER_BUTTONS=header:comments->comments is a closed visibility relation mapped to CAP-001 (fixture_root).'
    printf '%s\n' $'MAP-0014\tCEN-0013\tmapped\tCAP-001\tadopted\tclosed_registry\tHIDEABLE_HEADER_BUTTONS=header:mic->voice_dictation is a closed visibility relation mapped to CAP-001 (fixture_root).'
    printf '%s\n' $'MAP-0015\tCEN-0014\tmapped\tCAP-001\tadopted\tclosed_registry\tHIDEABLE_HEADER_BUTTONS=header:refresh->header_refresh is a closed visibility relation mapped to CAP-001 (fixture_root).'
    printf '%s\n' $'MAP-0016\tCEN-0015\tmapped\tCAP-001\tadopted\tclosed_registry\tHIDEABLE_MENU_ITEMS=menu:collapse->collapse_expand is a closed visibility relation mapped to CAP-001 (fixture_root).'
    printf '%s\n' $'MAP-0017\tCEN-0016\tmapped\tCAP-001\tadopted\tclosed_registry\tHIDEABLE_MENU_ITEMS=menu:colors->colour is a closed visibility relation mapped to CAP-001 (fixture_root).'
    printf '%s\n' $'MAP-0018\tCEN-0017\tmapped\tCAP-001\tadopted\tclosed_registry\tHIDEABLE_MENU_ITEMS=menu:duplicate->duplicate is a closed visibility relation mapped to CAP-001 (fixture_root).'
    printf '%s\n' $'MAP-0019\tCEN-0018\tmapped\tCAP-001\tadopted\tclosed_registry\tHIDEABLE_MENU_ITEMS=menu:group->group_selection is a closed visibility relation mapped to CAP-001 (fixture_root).'
    printf '%s\n' $'MAP-0020\tCEN-0019\tmapped\tCAP-001\tadopted\tclosed_registry\tHIDEABLE_MENU_ITEMS=menu:markdown-view->markdown_projection is a closed visibility relation mapped to CAP-001 (fixture_root).'
    printf '%s\n' $'MAP-0021\tCEN-0020\tmapped\tCAP-001\tadopted\tclosed_registry\tHIDEABLE_MENU_ITEMS=menu:refresh-terminal->refresh_terminal is a closed visibility relation mapped to CAP-001 (fixture_root).'
    printf '%s\n' $'MAP-0022\tCEN-0021\tmapped\tCAP-001\tadopted\tclosed_registry\tHIDEABLE_MENU_ITEMS=menu:remove-from-group->remove_from_group is a closed visibility relation mapped to CAP-001 (fixture_root).'
    printf '%s\n' $'MAP-0023\tCEN-0022\tmapped\tCAP-002\tadapted\tclosed_registry\tSUBAGENT_CAPABLE=fixture_agent is a closed capability set member mapped to CAP-002 (fixture_blob).'
    printf '%s\n' $'MAP-0024\tCEN-0023\tmapped\tCAP-001\tadopted\tclosed_registry\tWorkspaceMigrationKind=exec is a closed migration surface mapped to CAP-001 (fixture_root).'
    printf '%s\n' $'MAP-0025\tCEN-0024\tmapped\tCAP-001\tadopted\tclosed_registry\tWorkspaceMigrationKind=v2 is a closed migration surface mapped to CAP-001 (fixture_root).'
    printf '%s\n' $'MAP-0026\tCEN-0025\tmapped\tCAP-001\tadopted\tclosed_registry\tping is a closed IPC surface mapped to CAP-001 (fixture_root).'
    printf '%s\n' $'MAP-0027\tCEN-0026\tmapped\tCAP-001\tadopted\tclosed_registry\tfixture is a closed NodeKind surface mapped to CAP-001 (fixture_root).'
    printf '%s\n' $'MAP-0028\tCEN-0027\tmapped\tCAP-001\tadopted\tclosed_registry\tfontSize is a closed Settings key mapped to CAP-001 (fixture_root).'
  } >"$case_dir/docs/PRODUCT_CAPABILITY_SOURCE_MAPPING_V1.tsv"
}

clone_source_verifier_case() {
  source_dir=$1
  case_dir=$2
  cp -R "$source_dir" "$case_dir"
}

run_source_verifier() {
  case_dir=$1
  source_dir=$2
  /bin/bash "$case_dir/scripts/verify-product-capability-source.sh" "$source_dir"
}

normalize_source_mapping() {
  mapping_file=$1
  mapping_count=$(awk -F '\t' '$1 ~ /^MAP-[0-9][0-9][0-9][0-9]$/ { count++ } END { print count+0 }' "$mapping_file")
  awk -F '\t' -v count="$mapping_count" '
    BEGIN { OFS="\t" }
    NR==4 { print "# expected-mapping-count: " count; next }
    $1 ~ /^MAP-[0-9][0-9][0-9][0-9]$/ { sequence++; $1=sprintf("MAP-%04d", sequence) }
    { print }
  ' "$mapping_file" >"$mapping_file.normalized"
  replace_file "$mapping_file" "$mapping_file.normalized"
}

retarget_source_verifier_case() {
  case_dir=$1
  source_dir=$2
  next_snapshot=$(git -C "$source_dir" rev-parse HEAD)
  next_tree=$(git -C "$source_dir" ls-tree -r --full-tree "$next_snapshot" | hash_stream)
  awk -v snapshot="$next_snapshot" -v tree="$next_tree" '
    NR==2 { print "# source-snapshot: " snapshot; next }
    NR==3 { print "# source-tree-sha256: " tree; next }
    { print }
  ' "$case_dir/docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv" >"$case_dir/docs/coverage.new"
  replace_file "$case_dir/docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv" "$case_dir/docs/coverage.new"
  for artifact in PRODUCT_CAPABILITY_SOURCE_CENSUS_V1.tsv PRODUCT_CAPABILITY_SOURCE_MAPPING_V1.tsv; do
    awk -v snapshot="$next_snapshot" '
      NR==2 { print "# source-snapshot: " snapshot; next }
      { print }
    ' "$case_dir/docs/$artifact" >"$case_dir/docs/$artifact.new"
    replace_file "$case_dir/docs/$artifact" "$case_dir/docs/$artifact.new"
  done
}

/bin/bash -n "$repo_root/scripts/verify-product-spec.sh" \
  "$repo_root/scripts/verify-product-capability-source.sh" \
  "$repo_root/scripts/verify-product-completion.sh" \
  "$repo_root/scripts/verify-operation-registry.sh" \
  "$repo_root/scripts/test-operation-registry-gate.sh" \
  "$repo_root/scripts/verify-semantic-recovery-registry.sh" \
  "$repo_root/scripts/test-semantic-recovery-registry-gate.sh" \
  "$repo_root/scripts/verify-state-family-manifest.sh" \
  "$repo_root/scripts/test-state-family-manifest-gate.sh" \
  "$repo_root/scripts/test-product-spec-gate.sh"

source_fixture_repository="$scratch/source-repository"
source_fixture_base="$scratch/source-verifier-base"
seed_source_verifier_fixture "$source_fixture_base" "$source_fixture_repository"
run_source_verifier "$source_fixture_base" "$source_fixture_repository" >/dev/null
expect_rejected 'source verifier extra argument' E_USAGE \
  /bin/bash "$source_fixture_base/scripts/verify-product-capability-source.sh" \
  "$source_fixture_repository" unexpected
expect_rejected 'ambient source Git directory override' E_GIT_ENV \
  env GIT_DIR="$source_fixture_repository/.git" \
  /bin/bash "$source_fixture_base/scripts/verify-product-capability-source.sh" "$scratch"
expect_rejected 'ambient source object directory override' E_GIT_ENV \
  env GIT_OBJECT_DIRECTORY="$source_fixture_repository/.git/objects" \
  /bin/bash "$source_fixture_base/scripts/verify-product-capability-source.sh" "$source_fixture_repository"

source_fixture_bare="$scratch/source-repository-bare"
git clone -q --bare "$source_fixture_repository" "$source_fixture_bare"
run_source_verifier "$source_fixture_base" "$source_fixture_bare" >/dev/null

source_bad_snapshot="$scratch/source-bad-snapshot"
clone_source_verifier_case "$source_fixture_base" "$source_bad_snapshot"
awk 'NR==2 { print "# source-snapshot: 0000000000000000000000000000000000000000"; next } { print }' \
  "$source_bad_snapshot/docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv" >"$source_bad_snapshot/docs/coverage.new"
replace_file "$source_bad_snapshot/docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv" "$source_bad_snapshot/docs/coverage.new"
for artifact in PRODUCT_CAPABILITY_SOURCE_CENSUS_V1.tsv PRODUCT_CAPABILITY_SOURCE_MAPPING_V1.tsv; do
  awk 'NR==2 { print "# source-snapshot: 0000000000000000000000000000000000000000"; next } { print }' \
    "$source_bad_snapshot/docs/$artifact" >"$source_bad_snapshot/docs/$artifact.new"
  replace_file "$source_bad_snapshot/docs/$artifact" "$source_bad_snapshot/docs/$artifact.new"
done
expect_rejected 'absent frozen source snapshot' E_SNAPSHOT run_source_verifier \
  "$source_bad_snapshot" "$source_fixture_repository"

source_bad_tree="$scratch/source-bad-tree"
clone_source_verifier_case "$source_fixture_base" "$source_bad_tree"
awk 'NR==3 { print "# source-tree-sha256: 0000000000000000000000000000000000000000000000000000000000000000"; next } { print }' \
  "$source_bad_tree/docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv" >"$source_bad_tree/docs/coverage.new"
replace_file "$source_bad_tree/docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv" "$source_bad_tree/docs/coverage.new"
expect_rejected 'wrong frozen source tree' E_TREE run_source_verifier \
  "$source_bad_tree" "$source_fixture_repository"

source_bad_locator="$scratch/source-bad-locator"
clone_source_verifier_case "$source_fixture_base" "$source_bad_locator"
awk -F '\t' 'BEGIN { OFS="\t" } $1=="CAP-002" { $8="src/missing.txt" } { print }' \
  "$source_bad_locator/docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv" >"$source_bad_locator/docs/coverage.new"
replace_file "$source_bad_locator/docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv" "$source_bad_locator/docs/coverage.new"
expect_rejected 'missing source evidence locator' E_LOCATOR run_source_verifier \
  "$source_bad_locator" "$source_fixture_repository"

source_bad_digest="$scratch/source-bad-digest"
clone_source_verifier_case "$source_fixture_base" "$source_bad_digest"
awk -F '\t' 'BEGIN { OFS="\t" } $1=="CAP-002" { $9="0000000000000000000000000000000000000000000000000000000000000000" } { print }' \
  "$source_bad_digest/docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv" >"$source_bad_digest/docs/coverage.new"
replace_file "$source_bad_digest/docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv" "$source_bad_digest/docs/coverage.new"
expect_rejected 'wrong source evidence digest' E_DIGEST run_source_verifier \
  "$source_bad_digest" "$source_fixture_repository"

source_unknown_feature_mapping="$scratch/source-unknown-feature-mapping"
clone_source_verifier_case "$source_fixture_base" "$source_unknown_feature_mapping"
awk -F '\t' 'BEGIN { OFS="\t" } $1=="MAP-0003" { $4="CAP-999"; $7="src/core/pty-devices.ts anchors CAP-999 (unknown_fixture) for this mutation and adds no independent capability." } { print }' \
  "$source_unknown_feature_mapping/docs/PRODUCT_CAPABILITY_SOURCE_MAPPING_V1.tsv" \
  >"$source_unknown_feature_mapping/docs/mapping.new"
replace_file "$source_unknown_feature_mapping/docs/PRODUCT_CAPABILITY_SOURCE_MAPPING_V1.tsv" \
  "$source_unknown_feature_mapping/docs/mapping.new"
expect_rejected 'unknown source-mapping capability' E_MAPPING_FEATURE run_source_verifier \
  "$source_unknown_feature_mapping" "$source_fixture_repository"

source_unknown_candidate_mapping="$scratch/source-unknown-candidate-mapping"
clone_source_verifier_case "$source_fixture_base" "$source_unknown_candidate_mapping"
awk -F '\t' 'BEGIN { OFS="\t" } $1=="MAP-0011" { $2="CEN-9999" } { print }' \
  "$source_unknown_candidate_mapping/docs/PRODUCT_CAPABILITY_SOURCE_MAPPING_V1.tsv" \
  >"$source_unknown_candidate_mapping/docs/mapping.new"
replace_file "$source_unknown_candidate_mapping/docs/PRODUCT_CAPABILITY_SOURCE_MAPPING_V1.tsv" \
  "$source_unknown_candidate_mapping/docs/mapping.new"
expect_rejected 'unknown source-mapping candidate' E_MAPPING_CANDIDATE run_source_verifier \
  "$source_unknown_candidate_mapping" "$source_fixture_repository"

source_registry_supporting="$scratch/source-registry-supporting"
clone_source_verifier_case "$source_fixture_base" "$source_registry_supporting"
awk -F '\t' 'BEGIN { OFS="\t" } $1=="MAP-0024" { $3="supporting"; $6="supporting_module_audit"; $7="WorkspaceMigrationKind=exec supports CAP-001 (fixture_root) and adds no independent capability." } { print }' \
  "$source_registry_supporting/docs/PRODUCT_CAPABILITY_SOURCE_MAPPING_V1.tsv" \
  >"$source_registry_supporting/docs/mapping.new"
replace_file "$source_registry_supporting/docs/PRODUCT_CAPABILITY_SOURCE_MAPPING_V1.tsv" \
  "$source_registry_supporting/docs/mapping.new"
expect_rejected 'closed registry weakened to supporting' E_MAPPING_RESOLUTION run_source_verifier \
  "$source_registry_supporting" "$source_fixture_repository"

source_missing_mapping_anchor="$scratch/source-missing-mapping-anchor"
clone_source_verifier_case "$source_fixture_base" "$source_missing_mapping_anchor"
awk -F '\t' 'BEGIN { OFS="\t" } $1=="MAP-0003" { $7="CAP-002 (fixture_blob) has audited evidence without its source anchor." } { print }' \
  "$source_missing_mapping_anchor/docs/PRODUCT_CAPABILITY_SOURCE_MAPPING_V1.tsv" \
  >"$source_missing_mapping_anchor/docs/mapping.new"
replace_file "$source_missing_mapping_anchor/docs/PRODUCT_CAPABILITY_SOURCE_MAPPING_V1.tsv" \
  "$source_missing_mapping_anchor/docs/mapping.new"
expect_rejected 'mapping rationale missing exact source anchor' E_MAPPING_RATIONALE run_source_verifier \
  "$source_missing_mapping_anchor" "$source_fixture_repository"

source_wrong_mapping_disposition="$scratch/source-wrong-mapping-disposition"
clone_source_verifier_case "$source_fixture_base" "$source_wrong_mapping_disposition"
awk -F '\t' 'BEGIN { OFS="\t" } $1=="MAP-0004" { $5="adopted" } { print }' \
  "$source_wrong_mapping_disposition/docs/PRODUCT_CAPABILITY_SOURCE_MAPPING_V1.tsv" \
  >"$source_wrong_mapping_disposition/docs/mapping.new"
replace_file "$source_wrong_mapping_disposition/docs/PRODUCT_CAPABILITY_SOURCE_MAPPING_V1.tsv" \
  "$source_wrong_mapping_disposition/docs/mapping.new"
expect_rejected 'mapping disposition differs from ledger' E_MAPPING_DISPOSITION run_source_verifier \
  "$source_wrong_mapping_disposition" "$source_fixture_repository"

source_missing_evidence_relation="$scratch/source-missing-evidence-relation"
clone_source_verifier_case "$source_fixture_base" "$source_missing_evidence_relation"
awk -F '\t' '$1!="MAP-0002"' \
  "$source_missing_evidence_relation/docs/PRODUCT_CAPABILITY_SOURCE_MAPPING_V1.tsv" \
  >"$source_missing_evidence_relation/docs/mapping.new"
replace_file "$source_missing_evidence_relation/docs/PRODUCT_CAPABILITY_SOURCE_MAPPING_V1.tsv" \
  "$source_missing_evidence_relation/docs/mapping.new"
normalize_source_mapping "$source_missing_evidence_relation/docs/PRODUCT_CAPABILITY_SOURCE_MAPPING_V1.tsv"
expect_rejected 'missing shared ledger-evidence relation' E_MAPPING_EVIDENCE run_source_verifier \
  "$source_missing_evidence_relation" "$source_fixture_repository"

source_unmapped_candidate="$scratch/source-unmapped-candidate"
clone_source_verifier_case "$source_fixture_base" "$source_unmapped_candidate"
awk -F '\t' '$1!="MAP-0003"' \
  "$source_unmapped_candidate/docs/PRODUCT_CAPABILITY_SOURCE_MAPPING_V1.tsv" \
  >"$source_unmapped_candidate/docs/mapping.new"
replace_file "$source_unmapped_candidate/docs/PRODUCT_CAPABILITY_SOURCE_MAPPING_V1.tsv" \
  "$source_unmapped_candidate/docs/mapping.new"
normalize_source_mapping "$source_unmapped_candidate/docs/PRODUCT_CAPABILITY_SOURCE_MAPPING_V1.tsv"
expect_rejected 'selected source candidate has no mapping' E_MAPPING_COVERAGE run_source_verifier \
  "$source_unmapped_candidate" "$source_fixture_repository"

source_duplicate_mapping="$scratch/source-duplicate-mapping"
clone_source_verifier_case "$source_fixture_base" "$source_duplicate_mapping"
awk -F '\t' 'BEGIN { OFS="\t" } { print } $1=="MAP-0001" { $1="MAP-9999"; print }' \
  "$source_duplicate_mapping/docs/PRODUCT_CAPABILITY_SOURCE_MAPPING_V1.tsv" \
  >"$source_duplicate_mapping/docs/mapping.new"
replace_file "$source_duplicate_mapping/docs/PRODUCT_CAPABILITY_SOURCE_MAPPING_V1.tsv" \
  "$source_duplicate_mapping/docs/mapping.new"
normalize_source_mapping "$source_duplicate_mapping/docs/PRODUCT_CAPABILITY_SOURCE_MAPPING_V1.tsv"
expect_rejected 'duplicate candidate-capability mapping' E_MAPPING_DUPLICATE run_source_verifier \
  "$source_duplicate_mapping" "$source_fixture_repository"

source_duplicate_candidate="$scratch/source-duplicate-candidate"
clone_source_verifier_case "$source_fixture_base" "$source_duplicate_candidate"
awk -F '\t' 'BEGIN { OFS="\t" } $1=="CEN-0024" { $4="WorkspaceMigrationKind=exec" } { print }' \
  "$source_duplicate_candidate/docs/PRODUCT_CAPABILITY_SOURCE_CENSUS_V1.tsv" \
  >"$source_duplicate_candidate/docs/census.new"
replace_file "$source_duplicate_candidate/docs/PRODUCT_CAPABILITY_SOURCE_CENSUS_V1.tsv" \
  "$source_duplicate_candidate/docs/census.new"
expect_rejected 'duplicate selected source candidate' E_CENSUS_DUPLICATE run_source_verifier \
  "$source_duplicate_candidate" "$source_fixture_repository"

source_with_omitted_candidate="$scratch/source-with-omitted-candidate"
clone_source_verifier_case "$source_fixture_base" "$source_with_omitted_candidate"
source_repository_with_extra="$scratch/source-repository-with-extra"
clone_source_verifier_case "$source_fixture_repository" "$source_repository_with_extra"
printf '%s\n' 'export const newlyExposedSurface = true' >"$source_repository_with_extra/src/new-surface.ts"
git -C "$source_repository_with_extra" add src/new-surface.ts
git -C "$source_repository_with_extra" commit -qm 'add an unaccounted production surface'
extra_snapshot=$(git -C "$source_repository_with_extra" rev-parse HEAD)
extra_tree=$(git -C "$source_repository_with_extra" ls-tree -r --full-tree "$extra_snapshot" | hash_stream)
awk -v snapshot="$extra_snapshot" -v tree="$extra_tree" '
  NR==2 { print "# source-snapshot: " snapshot; next }
  NR==3 { print "# source-tree-sha256: " tree; next }
  { print }
' "$source_with_omitted_candidate/docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv" \
  >"$source_with_omitted_candidate/docs/coverage.new"
replace_file "$source_with_omitted_candidate/docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv" \
  "$source_with_omitted_candidate/docs/coverage.new"
awk -v snapshot="$extra_snapshot" '
  NR==2 { print "# source-snapshot: " snapshot; next }
  { print }
' "$source_with_omitted_candidate/docs/PRODUCT_CAPABILITY_SOURCE_CENSUS_V1.tsv" \
  >"$source_with_omitted_candidate/docs/census.new"
replace_file "$source_with_omitted_candidate/docs/PRODUCT_CAPABILITY_SOURCE_CENSUS_V1.tsv" \
  "$source_with_omitted_candidate/docs/census.new"
awk -v snapshot="$extra_snapshot" '
  NR==2 { print "# source-snapshot: " snapshot; next }
  { print }
' "$source_with_omitted_candidate/docs/PRODUCT_CAPABILITY_SOURCE_MAPPING_V1.tsv" \
  >"$source_with_omitted_candidate/docs/mapping.new"
replace_file "$source_with_omitted_candidate/docs/PRODUCT_CAPABILITY_SOURCE_MAPPING_V1.tsv" \
  "$source_with_omitted_candidate/docs/mapping.new"
expect_rejected 'new source candidate omitted from census' E_CENSUS_SOURCE_SET run_source_verifier \
  "$source_with_omitted_candidate" "$source_repository_with_extra"

source_changed_closed_registry="$scratch/source-changed-closed-registry"
clone_source_verifier_case "$source_fixture_base" "$source_changed_closed_registry"
source_repository_changed_registry="$scratch/source-repository-changed-registry"
clone_source_verifier_case "$source_fixture_repository" "$source_repository_changed_registry"
awk '{ gsub(/ \| '\''v2'\''/, ""); print }' \
  "$source_repository_changed_registry/src/shared/types.ts" \
  >"$source_repository_changed_registry/src/shared/types.new"
replace_file "$source_repository_changed_registry/src/shared/types.ts" \
  "$source_repository_changed_registry/src/shared/types.new"
git -C "$source_repository_changed_registry" add src/shared/types.ts
git -C "$source_repository_changed_registry" commit -qm 'remove one closed registry member'
changed_registry_snapshot=$(git -C "$source_repository_changed_registry" rev-parse HEAD)
changed_registry_tree=$(git -C "$source_repository_changed_registry" ls-tree -r --full-tree \
  "$changed_registry_snapshot" | hash_stream)
awk -v snapshot="$changed_registry_snapshot" -v tree="$changed_registry_tree" '
  NR==2 { print "# source-snapshot: " snapshot; next }
  NR==3 { print "# source-tree-sha256: " tree; next }
  { print }
' "$source_changed_closed_registry/docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv" \
  >"$source_changed_closed_registry/docs/coverage.new"
replace_file "$source_changed_closed_registry/docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv" \
  "$source_changed_closed_registry/docs/coverage.new"
for artifact in PRODUCT_CAPABILITY_SOURCE_CENSUS_V1.tsv PRODUCT_CAPABILITY_SOURCE_MAPPING_V1.tsv; do
  awk -v snapshot="$changed_registry_snapshot" '
    NR==2 { print "# source-snapshot: " snapshot; next }
    { print }
  ' "$source_changed_closed_registry/docs/$artifact" >"$source_changed_closed_registry/docs/$artifact.new"
  replace_file "$source_changed_closed_registry/docs/$artifact" \
    "$source_changed_closed_registry/docs/$artifact.new"
done
expect_rejected 'closed registry member removed from source' E_CENSUS_SOURCE_SET run_source_verifier \
  "$source_changed_closed_registry" "$source_repository_changed_registry"

source_changed_capability_set="$scratch/source-changed-capability-set"
clone_source_verifier_case "$source_fixture_base" "$source_changed_capability_set"
source_repository_changed_capability_set="$scratch/source-repository-changed-capability-set"
clone_source_verifier_case "$source_fixture_repository" "$source_repository_changed_capability_set"
awk '!/SUBAGENT_CAPABLE/' \
  "$source_repository_changed_capability_set/src/shared/agents/config.ts" \
  >"$source_repository_changed_capability_set/src/shared/agents/config.new"
replace_file "$source_repository_changed_capability_set/src/shared/agents/config.ts" \
  "$source_repository_changed_capability_set/src/shared/agents/config.new"
commit_case "$source_repository_changed_capability_set" 'remove one adapter capability-set member'
retarget_source_verifier_case "$source_changed_capability_set" \
  "$source_repository_changed_capability_set"
expect_rejected 'adapter capability-set member removed from source' E_CENSUS_SOURCE_SET \
  run_source_verifier "$source_changed_capability_set" "$source_repository_changed_capability_set"

source_deleted_renderer_css="$scratch/source-deleted-renderer-css"
clone_source_verifier_case "$source_fixture_base" "$source_deleted_renderer_css"
source_repository_deleted_renderer_css="$scratch/source-repository-deleted-renderer-css"
clone_source_verifier_case "$source_fixture_repository" "$source_repository_deleted_renderer_css"
git -C "$source_repository_deleted_renderer_css" rm -q -- src/renderer/styles.css
commit_case "$source_repository_deleted_renderer_css" 'delete a selected renderer runtime stylesheet'
retarget_source_verifier_case "$source_deleted_renderer_css" "$source_repository_deleted_renderer_css"
expect_rejected 'renderer runtime stylesheet removed from source' E_CENSUS_SOURCE_SET \
  run_source_verifier "$source_deleted_renderer_css" "$source_repository_deleted_renderer_css"

source_moved_hideable_control="$scratch/source-moved-hideable-control"
clone_source_verifier_case "$source_fixture_base" "$source_moved_hideable_control"
source_repository_moved_hideable_control="$scratch/source-repository-moved-hideable-control"
clone_source_verifier_case "$source_fixture_repository" "$source_repository_moved_hideable_control"
awk '
  /group/ { sub(/group/, "__slot_swap__") }
  /mic/ { sub(/mic/, "group") }
  { sub(/__slot_swap__/, "mic"); print }
' "$source_repository_moved_hideable_control/src/renderer/lib/ui-visibility.ts" \
  >"$source_repository_moved_hideable_control/src/renderer/lib/ui-visibility.new"
replace_file "$source_repository_moved_hideable_control/src/renderer/lib/ui-visibility.ts" \
  "$source_repository_moved_hideable_control/src/renderer/lib/ui-visibility.new"
commit_case "$source_repository_moved_hideable_control" 'move hideable controls between menu and header'
retarget_source_verifier_case "$source_moved_hideable_control" \
  "$source_repository_moved_hideable_control"
expect_rejected 'hideable control moved between menu and header' E_CENSUS_SELECTOR \
  run_source_verifier "$source_moved_hideable_control" "$source_repository_moved_hideable_control"

source_changed_pty_constant="$scratch/source-changed-pty-constant"
clone_source_verifier_case "$source_fixture_base" "$source_changed_pty_constant"
source_repository_changed_pty_constant="$scratch/source-repository-changed-pty-constant"
clone_source_verifier_case "$source_fixture_repository" "$source_repository_changed_pty_constant"
awk '{ gsub(/PTY_DEVICE_HEADROOM = 4/, "PTY_DEVICE_HEADROOM = 5"); print }' \
  "$source_repository_changed_pty_constant/src/core/pty-devices.ts" \
  >"$source_repository_changed_pty_constant/src/core/pty-devices.new"
replace_file "$source_repository_changed_pty_constant/src/core/pty-devices.ts" \
  "$source_repository_changed_pty_constant/src/core/pty-devices.new"
commit_case "$source_repository_changed_pty_constant" 'change an audited PTY safety constant'
retarget_source_verifier_case "$source_changed_pty_constant" \
  "$source_repository_changed_pty_constant"
expect_rejected 'PTY safety constant value changed in source' E_CENSUS_SOURCE_SET \
  run_source_verifier "$source_changed_pty_constant" "$source_repository_changed_pty_constant"

source_deleted_setting_key="$scratch/source-deleted-setting-key"
clone_source_verifier_case "$source_fixture_base" "$source_deleted_setting_key"
source_repository_deleted_setting_key="$scratch/source-repository-deleted-setting-key"
clone_source_verifier_case "$source_fixture_repository" "$source_repository_deleted_setting_key"
awk '!/^[[:space:]]*fontSize: number/' \
  "$source_repository_deleted_setting_key/src/shared/types.ts" \
  >"$source_repository_deleted_setting_key/src/shared/types.new"
replace_file "$source_repository_deleted_setting_key/src/shared/types.ts" \
  "$source_repository_deleted_setting_key/src/shared/types.new"
commit_case "$source_repository_deleted_setting_key" 'delete an audited Settings key'
retarget_source_verifier_case "$source_deleted_setting_key" "$source_repository_deleted_setting_key"
expect_rejected 'Settings key removed from source' E_CENSUS_SOURCE_SET \
  run_source_verifier "$source_deleted_setting_key" "$source_repository_deleted_setting_key"

baseline="$scratch/baseline"
seed_case "$baseline"
base_pin=$(tr -d '[:space:]' <"$baseline/docs/PRODUCT_SPEC_V1.sha256")
run_spec "$baseline" >/dev/null

coverage_deleted="$scratch/coverage-deleted"
clone_case "$baseline" "$coverage_deleted"
awk -F '\t' '$1!="CAP-001"' "$coverage_deleted/docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv" \
  >"$coverage_deleted/docs/coverage.new"
replace_file "$coverage_deleted/docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv" "$coverage_deleted/docs/coverage.new"
commit_case "$coverage_deleted" 'delete one capability coverage row'
expect_rejected 'deleted capability coverage row' E_COVERAGE_COUNT run_spec "$coverage_deleted"

coverage_unknown="$scratch/coverage-unknown"
clone_case "$baseline" "$coverage_unknown"
awk -F '\t' 'BEGIN { OFS="\t" } $1=="CAP-001" { $1="CAP-999" } { print }' \
  "$coverage_unknown/docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv" >"$coverage_unknown/docs/coverage.new"
replace_file "$coverage_unknown/docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv" "$coverage_unknown/docs/coverage.new"
commit_case "$coverage_unknown" 'replace one known capability identity'
expect_rejected 'unknown capability coverage identity' E_COVERAGE_SEQUENCE run_spec "$coverage_unknown"

coverage_weakened="$scratch/coverage-weakened"
clone_case "$baseline" "$coverage_weakened"
awk -F '\t' 'BEGIN { OFS="\t" } $1=="CAP-003" { $4="irrelevant" } { print }' \
  "$coverage_weakened/docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv" >"$coverage_weakened/docs/coverage.new"
replace_file "$coverage_weakened/docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv" "$coverage_weakened/docs/coverage.new"
commit_case "$coverage_weakened" 'weaken one adopted capability disposition'
expect_rejected 'weakened capability disposition' E_AUTHORITY_CONTENT run_spec "$coverage_weakened"

coverage_broken_link="$scratch/coverage-broken-link"
clone_case "$baseline" "$coverage_broken_link"
awk -F '\t' 'BEGIN { OFS="\t" } $1=="CAP-001" { $5="PRD-HIE-999"; $6="ACP-HIE-999" } { print }' \
  "$coverage_broken_link/docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv" >"$coverage_broken_link/docs/coverage.new"
replace_file "$coverage_broken_link/docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv" "$coverage_broken_link/docs/coverage.new"
commit_case "$coverage_broken_link" 'break one capability requirement link'
expect_rejected 'broken capability requirement link' E_COVERAGE_REQUIREMENT run_spec "$coverage_broken_link"

coverage_bad_digest="$scratch/coverage-bad-digest"
clone_case "$baseline" "$coverage_bad_digest"
awk -F '\t' 'BEGIN { OFS="\t" } $1=="CAP-001" { $9="deadbeef" } { print }' \
  "$coverage_bad_digest/docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv" >"$coverage_bad_digest/docs/coverage.new"
replace_file "$coverage_bad_digest/docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv" "$coverage_bad_digest/docs/coverage.new"
commit_case "$coverage_bad_digest" 'corrupt one capability evidence digest'
expect_rejected 'corrupt capability evidence digest' E_COVERAGE_DIGEST run_spec "$coverage_bad_digest"

coverage_changed_digest="$scratch/coverage-changed-digest"
clone_case "$baseline" "$coverage_changed_digest"
awk -F '\t' 'BEGIN { OFS="\t" } $1=="CAP-001" { $9="0000000000000000000000000000000000000000000000000000000000000000" } { print }' \
  "$coverage_changed_digest/docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv" >"$coverage_changed_digest/docs/coverage.new"
replace_file "$coverage_changed_digest/docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv" "$coverage_changed_digest/docs/coverage.new"
commit_case "$coverage_changed_digest" 'replace one capability evidence digest'
expect_rejected 'changed capability evidence digest' E_AUTHORITY_CONTENT run_spec "$coverage_changed_digest"

coverage_bad_snapshot="$scratch/coverage-bad-snapshot"
clone_case "$baseline" "$coverage_bad_snapshot"
awk 'NR==2 { print "# source-snapshot: 0000000000000000000000000000000000000000"; next } { print }' \
  "$coverage_bad_snapshot/docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv" >"$coverage_bad_snapshot/docs/coverage.new"
replace_file "$coverage_bad_snapshot/docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv" "$coverage_bad_snapshot/docs/coverage.new"
commit_case "$coverage_bad_snapshot" 'replace the frozen capability source snapshot'
expect_rejected 'changed capability source snapshot' E_COVERAGE_SNAPSHOT run_spec "$coverage_bad_snapshot"

coverage_bad_tree="$scratch/coverage-bad-tree"
clone_case "$baseline" "$coverage_bad_tree"
awk 'NR==3 { print "# source-tree-sha256: 0000000000000000000000000000000000000000000000000000000000000000"; next } { print }' \
  "$coverage_bad_tree/docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv" >"$coverage_bad_tree/docs/coverage.new"
replace_file "$coverage_bad_tree/docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv" "$coverage_bad_tree/docs/coverage.new"
commit_case "$coverage_bad_tree" 'replace the frozen capability source tree digest'
expect_rejected 'changed capability source tree digest' E_COVERAGE_TREE run_spec "$coverage_bad_tree"

paired_delete="$scratch/paired-delete"
clone_case "$baseline" "$paired_delete"
awk '!/`PRD-OUT-001`/' "$paired_delete/docs/PRODUCT_REQUIREMENTS.md" >"$paired_delete/docs/requirements.new"
replace_file "$paired_delete/docs/PRODUCT_REQUIREMENTS.md" "$paired_delete/docs/requirements.new"
awk '!/`ACP-OUT-001`/' "$paired_delete/docs/CONTROL_PLANE_ACCEPTANCE.md" >"$paired_delete/docs/acceptance.new"
replace_file "$paired_delete/docs/CONTROL_PLANE_ACCEPTANCE.md" "$paired_delete/docs/acceptance.new"
commit_case "$paired_delete" 'delete a paired requirement and oracle'
expect_rejected 'paired requirement/oracle deletion' E_REQUIREMENT_COUNT run_spec "$paired_delete"

weakened_requirement="$scratch/weakened-requirement"
clone_case "$baseline" "$weakened_requirement"
awk -F '|' 'BEGIN { OFS="|" } /`PRD-OUT-001`/ { $3=" trivially available " } { print }' \
  "$weakened_requirement/docs/PRODUCT_REQUIREMENTS.md" >"$weakened_requirement/docs/requirements.new"
replace_file "$weakened_requirement/docs/PRODUCT_REQUIREMENTS.md" "$weakened_requirement/docs/requirements.new"
commit_case "$weakened_requirement" 'weaken a normative outcome'
expect_rejected 'weakened normative outcome' E_MANIFEST_CONTENT run_spec "$weakened_requirement"

trivial_oracle="$scratch/trivial-oracle"
clone_case "$baseline" "$trivial_oracle"
awk -F '|' 'BEGIN { OFS="|" } /`ACP-OUT-001`/ { $5=" passes " } { print }' \
  "$trivial_oracle/docs/CONTROL_PLANE_ACCEPTANCE.md" >"$trivial_oracle/docs/acceptance.new"
replace_file "$trivial_oracle/docs/CONTROL_PLANE_ACCEPTANCE.md" "$trivial_oracle/docs/acceptance.new"
commit_case "$trivial_oracle" 'replace an oracle with a tautology'
expect_rejected 'trivial acceptance oracle' E_MANIFEST_CONTENT run_spec "$trivial_oracle"

normative_mutation="$scratch/normative-mutation"
clone_case "$baseline" "$normative_mutation"
printf '\nA weakened untracked promise.\n' >>"$normative_mutation/PRODUCT.md"
commit_case "$normative_mutation" 'mutate normative prose'
expect_rejected 'normative prose mutation' E_AUTHORITY_CONTENT run_spec "$normative_mutation"

semantic_registry_mutation="$scratch/semantic-registry-mutation"
clone_case "$baseline" "$semantic_registry_mutation"
awk -F '\t' 'BEGIN { OFS="\t" } $2 == "runtime_launch" { $3="unreviewed_key" } { print }' \
  "$semantic_registry_mutation/docs/SEMANTIC_RECOVERY_SUBJECTS_VNEXT.tsv" \
  >"$semantic_registry_mutation/docs/semantic-subjects.new"
replace_file "$semantic_registry_mutation/docs/SEMANTIC_RECOVERY_SUBJECTS_VNEXT.tsv" \
  "$semantic_registry_mutation/docs/semantic-subjects.new"
commit_case "$semantic_registry_mutation" 'mutate the semantic recovery registry'
expect_rejected 'semantic recovery registry mutation' E_AUTHORITY_CONTENT run_spec "$semantic_registry_mutation"

decision_mutation="$scratch/decision-mutation"
clone_case "$baseline" "$decision_mutation"
awk '/## ADR-066 / { print; print "\nUnfrozen weakening."; next } { print }' \
  "$decision_mutation/DECISIONS.md" >"$decision_mutation/DECISIONS.new"
replace_file "$decision_mutation/DECISIONS.md" "$decision_mutation/DECISIONS.new"
commit_case "$decision_mutation" 'mutate an originating decision'
expect_rejected 'originating decision mutation' E_AUTHORITY_CONTENT run_spec "$decision_mutation"

transitive_decision_mutation="$scratch/transitive-decision-mutation"
clone_case "$baseline" "$transitive_decision_mutation"
awk '/## ADR-040 / { print; print "\nUnfrozen transitive weakening."; next } { print }' \
  "$transitive_decision_mutation/DECISIONS.md" >"$transitive_decision_mutation/DECISIONS.new"
replace_file "$transitive_decision_mutation/DECISIONS.md" "$transitive_decision_mutation/DECISIONS.new"
commit_case "$transitive_decision_mutation" 'mutate an earlier transitive decision'
expect_rejected 'transitive decision mutation' E_AUTHORITY_CONTENT run_spec "$transitive_decision_mutation"

origin_swap="$scratch/origin-swap"
clone_case "$baseline" "$origin_swap"
awk -F '|' 'BEGIN { OFS="|" } $2=="PRD-ADP-001" { $6="ADR-059" } { print }' \
  "$origin_swap/docs/PRODUCT_REQUIREMENTS_V1.manifest" >"$origin_swap/docs/manifest.new"
replace_file "$origin_swap/docs/PRODUCT_REQUIREMENTS_V1.manifest" "$origin_swap/docs/manifest.new"
commit_case "$origin_swap" 'move a requirement to another decision'
expect_rejected 'origin decision swap' E_ORIGIN run_spec "$origin_swap"

escaped_pipe="$scratch/escaped-pipe"
clone_case "$baseline" "$escaped_pipe"
awk '/`PRD-OUT-001`/ { sub(/without polling panes/, "without polling \\\\| panes") } { print }' \
  "$escaped_pipe/docs/PRODUCT_REQUIREMENTS.md" >"$escaped_pipe/docs/requirements.new"
replace_file "$escaped_pipe/docs/PRODUCT_REQUIREMENTS.md" "$escaped_pipe/docs/requirements.new"
commit_case "$escaped_pipe" 'insert an escaped table pipe'
expect_rejected 'escaped table pipe' E_TABLE_ESCAPE run_spec "$escaped_pipe"

raw_pipe="$scratch/raw-pipe"
clone_case "$baseline" "$raw_pipe"
awk '/`PRD-OUT-001`/ { sub(/without polling panes/, "without polling | panes") } { print }' \
  "$raw_pipe/docs/PRODUCT_REQUIREMENTS.md" >"$raw_pipe/docs/requirements.new"
replace_file "$raw_pipe/docs/PRODUCT_REQUIREMENTS.md" "$raw_pipe/docs/requirements.new"
commit_case "$raw_pipe" 'insert a raw table pipe'
expect_rejected 'raw table pipe' E_TABLE_PARSE run_spec "$raw_pipe"

duplicate_id="$scratch/duplicate-id"
clone_case "$baseline" "$duplicate_id"
awk '{ print; if (/`PRD-OUT-001`/) print }' "$duplicate_id/docs/PRODUCT_REQUIREMENTS.md" \
  >"$duplicate_id/docs/requirements.new"
replace_file "$duplicate_id/docs/PRODUCT_REQUIREMENTS.md" "$duplicate_id/docs/requirements.new"
commit_case "$duplicate_id" 'duplicate one requirement id'
expect_rejected 'duplicate requirement id' E_DUPLICATE run_spec "$duplicate_id"

short_hash="$scratch/short-hash"
clone_case "$baseline" "$short_hash"
awk -F '|' 'BEGIN { OFS="|" } $2=="PRD-OUT-001" { $4="abc" } { print }' \
  "$short_hash/docs/PRODUCT_REQUIREMENTS_V1.manifest" >"$short_hash/docs/manifest.new"
replace_file "$short_hash/docs/PRODUCT_REQUIREMENTS_V1.manifest" "$short_hash/docs/manifest.new"
commit_case "$short_hash" 'shorten a manifest digest'
expect_rejected 'short manifest digest' E_MANIFEST_PARSE run_spec "$short_hash"

dirty_authority="$scratch/dirty-authority"
clone_case "$baseline" "$dirty_authority"
printf '\ndirty\n' >>"$dirty_authority/README.md"
expect_rejected 'dirty authority input' E_DIRTY_AUTHORITY run_spec "$dirty_authority"

untracked_authority="$scratch/untracked-authority"
clone_case "$baseline" "$untracked_authority"
git -C "$untracked_authority" rm --cached -q docs/PRODUCT_SPEC_V1.authority
git -C "$untracked_authority" commit -qm 'remove authority from the index'
git -C "$untracked_authority" show HEAD^:docs/PRODUCT_SPEC_V1.authority >"$untracked_authority/docs/PRODUCT_SPEC_V1.authority"
expect_rejected 'untracked authority root' E_UNTRACKED_AUTHORITY run_spec "$untracked_authority"

symlink_authority="$scratch/symlink-authority"
clone_case "$baseline" "$symlink_authority"
mv "$symlink_authority/docs/PRODUCT_SPEC_V1.authority" "$symlink_authority/docs/PRODUCT_SPEC_V1.authority.real"
ln -s PRODUCT_SPEC_V1.authority.real "$symlink_authority/docs/PRODUCT_SPEC_V1.authority"
commit_case "$symlink_authority" 'replace authority with a symlink'
expect_rejected 'symlink authority root' E_AUTHORITY_MISSING run_spec "$symlink_authority"

coedited_delete="$scratch/coedited-delete"
clone_case "$baseline" "$coedited_delete"
awk '!/`PRD-OUT-001`/' "$coedited_delete/docs/PRODUCT_REQUIREMENTS.md" >"$coedited_delete/docs/requirements.new"
replace_file "$coedited_delete/docs/PRODUCT_REQUIREMENTS.md" "$coedited_delete/docs/requirements.new"
awk '!/`ACP-OUT-001`/' "$coedited_delete/docs/CONTROL_PLANE_ACCEPTANCE.md" >"$coedited_delete/docs/acceptance.new"
replace_file "$coedited_delete/docs/CONTROL_PLANE_ACCEPTANCE.md" "$coedited_delete/docs/acceptance.new"
awk '{ gsub(/expected_requirement_count=185/, "expected_requirement_count=184"); gsub(/expected_acceptance_count=185/, "expected_acceptance_count=184"); print }' \
  "$coedited_delete/scripts/verify-product-spec.sh" >"$coedited_delete/scripts/verifier.new"
replace_file "$coedited_delete/scripts/verify-product-spec.sh" "$coedited_delete/scripts/verifier.new"
refreeze_case "$coedited_delete"
commit_case "$coedited_delete" 'coedit every repository trust root after deletion'
expect_rejected 'fully coedited deletion against external pin' E_AUTHORITY_CI_PIN run_spec "$coedited_delete"

coedited_weakening="$scratch/coedited-weakening"
clone_case "$baseline" "$coedited_weakening"
awk -F '|' 'BEGIN { OFS="|" } /`PRD-OUT-001`/ { $3=" trivially available " } { print }' \
  "$coedited_weakening/docs/PRODUCT_REQUIREMENTS.md" >"$coedited_weakening/docs/requirements.new"
replace_file "$coedited_weakening/docs/PRODUCT_REQUIREMENTS.md" "$coedited_weakening/docs/requirements.new"
refreeze_case "$coedited_weakening"
commit_case "$coedited_weakening" 'coedit every repository trust root after weakening'
expect_rejected 'fully coedited weakening against external pin' E_AUTHORITY_CI_PIN run_spec "$coedited_weakening"

build_completion_fixture() {
  specification_repo=$1
  completion_repo=$2
  clone_case "$specification_repo" "$completion_repo"

  awk -F '|' 'BEGIN { OFS="|" } /^\| `PRD-[A-Z]+-[0-9][0-9][0-9]` / { $5=" implemented " } { print }' \
    "$completion_repo/docs/PRODUCT_REQUIREMENTS.md" >"$completion_repo/docs/requirements.new"
  replace_file "$completion_repo/docs/PRODUCT_REQUIREMENTS.md" "$completion_repo/docs/requirements.new"

  mkdir -p "$completion_repo/scripts/product-acceptance" \
    "$completion_repo/tests/product-acceptance/descriptors" \
    "$completion_repo/crates/product-fixture/src"
  printf '%s\n' 'pub fn product_fixture_is_real() -> bool { true }' \
    >"$completion_repo/crates/product-fixture/src/lib.rs"
  implementation_hash=$(hash_file "$completion_repo/crates/product-fixture/src/lib.rs")
  proof_hash=$(printf 'verified\n' | { if command -v sha256sum >/dev/null 2>&1; then sha256sum; else shasum -a 256; fi; } | awk '{print $1}')

  awk -F '|' '/^\| `PRD-[A-Z]+-[0-9][0-9][0-9]` / { id=$2; gsub(/[ `]/, "", id); print id }' \
    "$completion_repo/docs/PRODUCT_REQUIREMENTS.md" | sort >"$completion_repo/requirement-ids.fixture"
  while IFS= read -r id; do
    target=$(printf '%s' "$id" | tr '[:upper:]' '[:lower:]' | sed 's/^prd-/acp-/')
    entrypoint="scripts/product-acceptance/$target.sh"
    descriptor="tests/product-acceptance/descriptors/$target.tsv"
    {
      printf '%s\n' '#!/usr/bin/env bash' 'set -euo pipefail'
      printf "expected_target='%s'\n" "$target"
      cat <<'ENTRYPOINT'
: "${TURN_PRODUCT_ACCEPTANCE_ROOT:?}"
: "${TURN_PRODUCT_ACCEPTANCE_TOKEN:?}"
: "${TURN_PRODUCT_ACCEPTANCE_TARGET:?}"
[[ "$TURN_PRODUCT_ACCEPTANCE_TARGET" == "$expected_target" ]]
[[ -f crates/product-fixture/src/lib.rs ]]
mkdir -p "$TURN_PRODUCT_ACCEPTANCE_ROOT/.oracle-invocations" \
  "$TURN_PRODUCT_ACCEPTANCE_ROOT/$TURN_PRODUCT_ACCEPTANCE_TARGET"
printf '%s' "$TURN_PRODUCT_ACCEPTANCE_TOKEN" \
  >"$TURN_PRODUCT_ACCEPTANCE_ROOT/.oracle-invocations/$TURN_PRODUCT_ACCEPTANCE_TARGET"
printf 'verified\n' >"$TURN_PRODUCT_ACCEPTANCE_ROOT/$TURN_PRODUCT_ACCEPTANCE_TARGET/proof.txt"
ENTRYPOINT
    } >"$completion_repo/$entrypoint"
    chmod 755 "$completion_repo/$entrypoint"
    entrypoint_hash=$(hash_file "$completion_repo/$entrypoint")
    {
      printf 'schema\t1\n'
      printf 'requirement\t%s\n' "$id"
      printf 'target\t%s\n' "$target"
      printf 'entrypoint\t%s\t%s\n' "$entrypoint" "$entrypoint_hash"
      printf 'implementation\tcrates/product-fixture/src/lib.rs\t%s\n' "$implementation_hash"
      printf 'artifact\t%s/proof.txt\t%s\n' "$target" "$proof_hash"
    } >"$completion_repo/$descriptor"
  done <"$completion_repo/requirement-ids.fixture"
  mv "$completion_repo/requirement-ids.fixture" "$completion_repo/tests/product-acceptance/requirement-ids.fixture"

  commit_case "$completion_repo" 'fixture product implementation and oracles'
  implementation_commit=$(git -C "$completion_repo" rev-parse HEAD)
  {
    printf '%s\n' '# schema: requirement<TAB>implementation-commit<TAB>oracle-target<TAB>descriptor-path<TAB>descriptor-sha256'
    printf '%s\n' '# Complete synthetic fixture for the mutation suite.'
    while IFS= read -r id; do
      target=$(printf '%s' "$id" | tr '[:upper:]' '[:lower:]' | sed 's/^prd-/acp-/')
      descriptor="tests/product-acceptance/descriptors/$target.tsv"
      descriptor_hash=$(hash_file "$completion_repo/$descriptor")
      printf '%s\t%s\t%s\t%s\t%s\n' "$id" "$implementation_commit" "$target" "$descriptor" "$descriptor_hash"
    done <"$completion_repo/tests/product-acceptance/requirement-ids.fixture"
  } >"$completion_repo/docs/PRODUCT_IMPLEMENTATION_EVIDENCE_V1.tsv"
  commit_case "$completion_repo" 'fixture implementation evidence ledger'
}

rebind_first_requirement() {
  case_dir=$1
  id=PRD-ADP-001
  target=acp-adp-001
  entrypoint="scripts/product-acceptance/$target.sh"
  descriptor="tests/product-acceptance/descriptors/$target.tsv"
  entrypoint_hash=$(hash_file "$case_dir/$entrypoint")
  awk -F '\t' -v OFS='\t' -v digest="$entrypoint_hash" \
    '$1=="entrypoint" { $3=digest } { print }' "$case_dir/$descriptor" >"$case_dir/descriptor.new"
  replace_file "$case_dir/$descriptor" "$case_dir/descriptor.new"
  commit_case "$case_dir" 'mutated first implementation oracle'
  implementation_commit=$(git -C "$case_dir" rev-parse HEAD)
  descriptor_hash=$(hash_file "$case_dir/$descriptor")
  awk -F '\t' -v OFS='\t' -v id="$id" -v commit="$implementation_commit" -v digest="$descriptor_hash" \
    '$1==id { $2=commit; $5=digest } { print }' \
    "$case_dir/docs/PRODUCT_IMPLEMENTATION_EVIDENCE_V1.tsv" >"$case_dir/docs/evidence.new"
  replace_file "$case_dir/docs/PRODUCT_IMPLEMENTATION_EVIDENCE_V1.tsv" "$case_dir/docs/evidence.new"
  commit_case "$case_dir" 'rebind first implementation evidence'
}

rebind_first_descriptor() {
  case_dir=$1
  id=PRD-ADP-001
  target=acp-adp-001
  descriptor="tests/product-acceptance/descriptors/$target.tsv"
  commit_case "$case_dir" 'mutated first evidence descriptor'
  implementation_commit=$(git -C "$case_dir" rev-parse HEAD)
  descriptor_hash=$(hash_file "$case_dir/$descriptor")
  awk -F '\t' -v OFS='\t' -v id="$id" -v commit="$implementation_commit" -v digest="$descriptor_hash" \
    '$1==id { $2=commit; $5=digest } { print }' \
    "$case_dir/docs/PRODUCT_IMPLEMENTATION_EVIDENCE_V1.tsv" >"$case_dir/docs/evidence.new"
  replace_file "$case_dir/docs/PRODUCT_IMPLEMENTATION_EVIDENCE_V1.tsv" "$case_dir/docs/evidence.new"
  commit_case "$case_dir" 'rebind first descriptor evidence'
}

completion_good="$scratch/completion-good"
build_completion_fixture "$baseline" "$completion_good"
run_completion "$completion_good" >/dev/null

completion_partial="$scratch/completion-partial"
clone_case "$completion_good" "$completion_partial"
awk -F '|' 'BEGIN { OFS="|" } /`PRD-ADP-001`/ { $5=" partial " } { print }' \
  "$completion_partial/docs/PRODUCT_REQUIREMENTS.md" >"$completion_partial/docs/requirements.new"
replace_file "$completion_partial/docs/PRODUCT_REQUIREMENTS.md" "$completion_partial/docs/requirements.new"
commit_case "$completion_partial" 'restore one incomplete status'
expect_rejected 'non-implemented requirement' E_NOT_IMPLEMENTED run_completion "$completion_partial"

completion_missing="$scratch/completion-missing"
clone_case "$completion_good" "$completion_missing"
awk -F '\t' '$1!="PRD-ADP-001"' "$completion_missing/docs/PRODUCT_IMPLEMENTATION_EVIDENCE_V1.tsv" \
  >"$completion_missing/docs/evidence.new"
replace_file "$completion_missing/docs/PRODUCT_IMPLEMENTATION_EVIDENCE_V1.tsv" "$completion_missing/docs/evidence.new"
commit_case "$completion_missing" 'remove one evidence row'
expect_rejected 'missing evidence row' E_EVIDENCE_SET run_completion "$completion_missing"

completion_duplicate="$scratch/completion-duplicate"
clone_case "$completion_good" "$completion_duplicate"
awk '{ print; if ($1=="PRD-ADP-001") print }' "$completion_duplicate/docs/PRODUCT_IMPLEMENTATION_EVIDENCE_V1.tsv" \
  >"$completion_duplicate/docs/evidence.new"
replace_file "$completion_duplicate/docs/PRODUCT_IMPLEMENTATION_EVIDENCE_V1.tsv" "$completion_duplicate/docs/evidence.new"
commit_case "$completion_duplicate" 'duplicate one evidence row'
expect_rejected 'duplicate evidence row' E_EVIDENCE_DUPLICATE run_completion "$completion_duplicate"

completion_bad_target="$scratch/completion-bad-target"
clone_case "$completion_good" "$completion_bad_target"
awk -F '\t' -v OFS='\t' '$1=="PRD-ADP-001" { $3="acp-adp-999" } { print }' \
  "$completion_bad_target/docs/PRODUCT_IMPLEMENTATION_EVIDENCE_V1.tsv" >"$completion_bad_target/docs/evidence.new"
replace_file "$completion_bad_target/docs/PRODUCT_IMPLEMENTATION_EVIDENCE_V1.tsv" "$completion_bad_target/docs/evidence.new"
commit_case "$completion_bad_target" 'change one evidence target'
expect_rejected 'wrong evidence target' E_TARGET run_completion "$completion_bad_target"

completion_old_commit="$scratch/completion-old-commit"
clone_case "$completion_good" "$completion_old_commit"
root_commit=$(git -C "$completion_old_commit" rev-list --max-parents=0 HEAD)
awk -F '\t' -v OFS='\t' -v commit="$root_commit" '$1=="PRD-ADP-001" { $2=commit } { print }' \
  "$completion_old_commit/docs/PRODUCT_IMPLEMENTATION_EVIDENCE_V1.tsv" >"$completion_old_commit/docs/evidence.new"
replace_file "$completion_old_commit/docs/PRODUCT_IMPLEMENTATION_EVIDENCE_V1.tsv" "$completion_old_commit/docs/evidence.new"
commit_case "$completion_old_commit" 'point evidence before its oracle'
expect_rejected 'implementation commit predates oracle' E_ORACLE_NOT_AT_COMMIT run_completion "$completion_old_commit"

completion_source_changed="$scratch/completion-source-changed"
clone_case "$completion_good" "$completion_source_changed"
printf '\n# changed after evidence\n' >>"$completion_source_changed/scripts/product-acceptance/acp-adp-001.sh"
commit_case "$completion_source_changed" 'change an oracle after evidence'
expect_rejected 'source changed after implementation commit' E_SOURCE_HASH run_completion "$completion_source_changed"

completion_noop="$scratch/completion-noop"
clone_case "$completion_good" "$completion_noop"
printf '%s\n' '#!/usr/bin/env bash' 'set -euo pipefail' ':' \
  >"$completion_noop/scripts/product-acceptance/acp-adp-001.sh"
chmod 755 "$completion_noop/scripts/product-acceptance/acp-adp-001.sh"
rebind_first_requirement "$completion_noop"
expect_rejected 'no-op oracle entrypoint' E_ORACLE_NOT_INVOKED run_completion "$completion_noop"

completion_wrong_hash="$scratch/completion-wrong-hash"
clone_case "$completion_good" "$completion_wrong_hash"
awk -F '\t' -v OFS='\t' '$1=="artifact" { $3="0000000000000000000000000000000000000000000000000000000000000000" } { print }' \
  "$completion_wrong_hash/tests/product-acceptance/descriptors/acp-adp-001.tsv" >"$completion_wrong_hash/descriptor.new"
replace_file "$completion_wrong_hash/tests/product-acceptance/descriptors/acp-adp-001.tsv" "$completion_wrong_hash/descriptor.new"
rebind_first_descriptor "$completion_wrong_hash"
expect_rejected 'wrong fresh artifact hash' E_ARTIFACT_HASH run_completion "$completion_wrong_hash"

completion_traversal="$scratch/completion-traversal"
clone_case "$completion_good" "$completion_traversal"
awk -F '\t' -v OFS='\t' '$1=="artifact" { $2="../escape" } { print }' \
  "$completion_traversal/tests/product-acceptance/descriptors/acp-adp-001.tsv" >"$completion_traversal/descriptor.new"
replace_file "$completion_traversal/tests/product-acceptance/descriptors/acp-adp-001.tsv" "$completion_traversal/descriptor.new"
rebind_first_descriptor "$completion_traversal"
expect_rejected 'artifact path traversal' E_ARTIFACT_PATH run_completion "$completion_traversal"

completion_extra_file="$scratch/completion-extra-file"
clone_case "$completion_good" "$completion_extra_file"
printf '%s\n' 'printf extra >"$TURN_PRODUCT_ACCEPTANCE_ROOT/$TURN_PRODUCT_ACCEPTANCE_TARGET/extra.txt"' \
  >>"$completion_extra_file/scripts/product-acceptance/acp-adp-001.sh"
rebind_first_requirement "$completion_extra_file"
expect_rejected 'undeclared regular artifact' E_ARTIFACT_SET run_completion "$completion_extra_file"

completion_fifo="$scratch/completion-fifo"
clone_case "$completion_good" "$completion_fifo"
printf '%s\n' 'mkfifo "$TURN_PRODUCT_ACCEPTANCE_ROOT/$TURN_PRODUCT_ACCEPTANCE_TARGET/extra.fifo"' \
  >>"$completion_fifo/scripts/product-acceptance/acp-adp-001.sh"
rebind_first_requirement "$completion_fifo"
expect_rejected 'undeclared FIFO artifact' E_ARTIFACT_NODE run_completion "$completion_fifo"

completion_ignored_input="$scratch/completion-ignored-input"
clone_case "$completion_good" "$completion_ignored_input"
cat >"$completion_ignored_input/scripts/product-acceptance/acp-adp-001.sh" <<'IGNORED'
#!/usr/bin/env bash
set -euo pipefail
expected_target='acp-adp-001'
if [[ ! -f influence.flag ]]; then
  exit 0
fi
: "${TURN_PRODUCT_ACCEPTANCE_ROOT:?}"
: "${TURN_PRODUCT_ACCEPTANCE_TOKEN:?}"
: "${TURN_PRODUCT_ACCEPTANCE_TARGET:?}"
[[ "$TURN_PRODUCT_ACCEPTANCE_TARGET" == "$expected_target" ]]
mkdir -p "$TURN_PRODUCT_ACCEPTANCE_ROOT/.oracle-invocations" \
  "$TURN_PRODUCT_ACCEPTANCE_ROOT/$TURN_PRODUCT_ACCEPTANCE_TARGET"
printf '%s' "$TURN_PRODUCT_ACCEPTANCE_TOKEN" \
  >"$TURN_PRODUCT_ACCEPTANCE_ROOT/.oracle-invocations/$TURN_PRODUCT_ACCEPTANCE_TARGET"
printf 'verified\n' >"$TURN_PRODUCT_ACCEPTANCE_ROOT/$TURN_PRODUCT_ACCEPTANCE_TARGET/proof.txt"
IGNORED
chmod 755 "$completion_ignored_input/scripts/product-acceptance/acp-adp-001.sh"
rebind_first_requirement "$completion_ignored_input"
printf '%s\n' 'influence.flag' >>"$completion_ignored_input/.git/info/exclude"
printf '%s\n' 'caller-only ignored input' >"$completion_ignored_input/influence.flag"
expect_rejected 'caller ignored input isolation' E_ORACLE_NOT_INVOKED run_completion "$completion_ignored_input"

completion_revision_switch="$scratch/completion-revision-switch"
clone_case "$completion_good" "$completion_revision_switch"
printf '%s\n' 'git checkout --quiet --detach "$(git rev-list --max-parents=0 HEAD)"' \
  >>"$completion_revision_switch/scripts/product-acceptance/acp-adp-001.sh"
rebind_first_requirement "$completion_revision_switch"
expect_rejected 'oracle checkout revision switch' E_ORACLE_REVISION run_completion "$completion_revision_switch"

completion_dirty="$scratch/completion-dirty"
clone_case "$completion_good" "$completion_dirty"
printf '%s\n' dirty >"$completion_dirty/untracked.txt"
expect_rejected 'dirty completion checkout' E_DIRTY_CHECKOUT run_completion "$completion_dirty"

sha256_probe="$scratch/sha256-probe"
mkdir -p "$sha256_probe"
if git -C "$sha256_probe" init -q --object-format=sha256 >/dev/null 2>&1; then
  sha256_spec="$scratch/sha256-spec"
  seed_case "$sha256_spec" sha256
  run_spec "$sha256_spec" >/dev/null
  sha256_completion="$scratch/sha256-completion"
  build_completion_fixture "$sha256_spec" "$sha256_completion"
  run_completion "$sha256_completion" >/dev/null
fi

echo "product-spec-mutations: frozen spec and executable completion gates passed SHA-aware positive fixtures and rejected all adversarial mutations"
