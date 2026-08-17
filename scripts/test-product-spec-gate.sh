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
    "$repo_root/scripts/verify-product-completion.sh" \
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

/bin/bash -n "$repo_root/scripts/verify-product-spec.sh" \
  "$repo_root/scripts/verify-product-completion.sh" \
  "$repo_root/scripts/test-product-spec-gate.sh"

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

decision_mutation="$scratch/decision-mutation"
clone_case "$baseline" "$decision_mutation"
awk '/## ADR-064 / { print; print "\nUnfrozen weakening."; next } { print }' \
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
awk '{ gsub(/expected_requirement_count=144/, "expected_requirement_count=143"); gsub(/expected_acceptance_count=144/, "expected_acceptance_count=143"); print }' \
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
