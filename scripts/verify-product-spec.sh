#!/usr/bin/env bash
set -euo pipefail

export LC_ALL=C

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
requirements="$repo_root/docs/PRODUCT_REQUIREMENTS.md"
acceptance="$repo_root/docs/CONTROL_PLANE_ACCEPTANCE.md"
contract="$repo_root/docs/OPERATOR_CONTROL_PLANE.md"
gap_audit="$repo_root/docs/CONTROL_PLANE_GAP_AUDIT.md"
coverage="$repo_root/docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv"
census="$repo_root/docs/PRODUCT_CAPABILITY_SOURCE_CENSUS_V1.tsv"
mapping="$repo_root/docs/PRODUCT_CAPABILITY_SOURCE_MAPPING_V1.tsv"
manifest="$repo_root/docs/PRODUCT_REQUIREMENTS_V1.manifest"
authority="$repo_root/docs/PRODUCT_SPEC_V1.authority"
authority_pin="$repo_root/docs/PRODUCT_SPEC_V1.sha256"
decisions="$repo_root/DECISIONS.md"
mode=${1:-verify}

expected_requirement_count=185
expected_acceptance_count=185
expected_coverage_count=112
expected_coverage_snapshot=130cdc24bb493349d9b3f3c531198b7bb5fec3df
expected_coverage_tree_sha256=721e0b9538fb8225f8c773b904b1e98c388ea3e518ad818e9ce3e5f0d8dde3ce

die() {
  code=$1
  shift
  echo "product-spec-acceptance: $code: $*" >&2
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

hash_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    die E_HASH_TOOL "sha256sum or shasum is required"
  fi
}

normative_paths() {
  printf '%s\n' \
    README.md \
    PRODUCT.md \
    ARCHITECTURE.md \
    DECISIONS.md \
    Makefile \
    ROADMAP.md \
    .github/workflows/ci.yml \
    docs/ACCESSIBILITY_ACCEPTANCE.md \
    docs/ADAPTER_ACCEPTANCE.md \
    docs/AGENT_NODE_VIEWS_AND_CONTEXT.md \
    docs/ATTENTION_ACCEPTANCE.md \
    docs/CONTROL_PLANE_ACCEPTANCE.md \
    docs/CONTROL_PLANE_GAP_AUDIT.md \
    docs/INSPECTOR_ACCEPTANCE.md \
    docs/LIFECYCLE_ACCEPTANCE.md \
    docs/LOCAL_VOICE_INPUT.md \
    docs/MVP_ACCEPTANCE.md \
    docs/OPERATION_REGISTRY_CAP105_112_VNEXT.tsv \
    docs/OPERATOR_CONTROL_PLANE.md \
    docs/PERFORMANCE.md \
    docs/PRIVACY.md \
    docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv \
    docs/PRODUCT_CAPABILITY_SOURCE_CENSUS_V1.tsv \
    docs/PRODUCT_CAPABILITY_SOURCE_MAPPING_V1.tsv \
    docs/PRODUCT_IMPLEMENTATION_EVIDENCE.md \
    docs/PRODUCT_REQUIREMENTS.md \
    docs/PROTOCOL.md \
    docs/RELEASE.md \
    docs/REVIEWER_ACCEPTANCE.md \
    docs/SECURITY.md \
    docs/SEMANTIC_RECOVERY_FAMILY_CLASSIFICATION_VNEXT.tsv \
    docs/SEMANTIC_RECOVERY_SUBJECTS_VNEXT.tsv \
    docs/STATE_FAMILY_MANIFEST_VNEXT.tsv \
    docs/TEMPLATE_ACCEPTANCE.md \
    docs/TERMINAL_ACCEPTANCE.md \
    docs/UNIFIED_HIERARCHY_UPGRADE.md \
    scripts/test-operation-registry-gate.sh \
    scripts/test-product-spec-gate.sh \
    scripts/test-semantic-recovery-registry-gate.sh \
    scripts/test-state-family-manifest-gate.sh \
    scripts/verify-operation-registry.sh \
    scripts/verify-product-capability-source.sh \
    scripts/verify-product-completion.sh \
    scripts/verify-product-spec.sh \
    scripts/verify-semantic-recovery-registry.sh \
    scripts/verify-state-family-manifest.sh
}

origin_for_id() {
  case "$1" in
    PRD-VIE-015|PRD-CRE-011|PRD-ADP-014|PRD-LIF-010|PRD-LIF-011|PRD-RUN-017|PRD-RUN-018|PRD-RUN-019|PRD-RUN-020|PRD-RUN-021|PRD-RUN-022|PRD-OBS-012|PRD-ATT-013|PRD-SAF-016|PRD-SAF-017|PRD-SAF-018|PRD-SAF-019|PRD-SAF-020|PRD-SAF-021|PRD-SCL-011|PRD-SCL-012|PRD-SCL-013)
      printf '%s\n' ADR-067
      ;;
    PRD-VIE-013|PRD-VIE-014|PRD-CRE-009|PRD-CRE-010|PRD-RUN-012|PRD-RUN-013|PRD-RUN-014|PRD-RUN-015|PRD-RUN-016|PRD-SAF-015)
      printf '%s\n' ADR-066
      ;;
    PRD-HIE-009|PRD-HIE-010|PRD-ATT-012|PRD-OBS-010|PRD-ADP-012|PRD-SAF-014|PRD-ADP-013|PRD-CRE-008|PRD-OBS-011)
      printf '%s\n' ADR-065
      ;;
    PRD-VIE-012|PRD-ADP-011|PRD-LIF-009|PRD-RUN-011|PRD-CTX-013|PRD-OBS-009|PRD-ATT-011|PRD-SCL-010)
      printf '%s\n' ADR-064
      ;;
    PRD-VIE-011|PRD-FLW-012|PRD-ADP-010|PRD-LIF-008|PRD-RUN-010|PRD-CTX-012|PRD-OBS-008|PRD-SCL-009)
      printf '%s\n' ADR-063
      ;;
    PRD-VOI-*) printf '%s\n' ADR-060 ;;
    PRD-OUT-*|PRD-CRE-*|PRD-FLW-*|PRD-SCL-*) printf '%s\n' ADR-061 ;;
    PRD-TOP-*|PRD-ADP-*) printf '%s\n' ADR-062 ;;
    PRD-HIE-*|PRD-VIE-*|PRD-LIF-*|PRD-RUN-*|PRD-CTX-*|PRD-OBS-*|PRD-ATT-*|PRD-SAF-*)
      printf '%s\n' ADR-059
      ;;
    *) die E_ORIGIN "no originating decision rule for $1" ;;
  esac
}

redacted_requirement_hash() {
  awk -F '|' 'BEGIN { OFS="|" }
    /^\| `PRD-[A-Z]+-[0-9][0-9][0-9]` / {
      if (NF != 7) exit 91
      $5=" <STATUS> "
    }
    { print }
  ' "$1" | hash_stream
}

decision_section_hash() {
  decision=$1
  awk -v heading="## $decision " '
    index($0, heading) == 1 { active=1 }
    active && seen && /^## ADR-[0-9][0-9][0-9] / { exit }
    active { print; seen=1 }
  ' "$decisions" | hash_stream
}

scratch=$(mktemp -d "${TMPDIR:-/tmp}/turn-product-spec.XXXXXX")
trap 'rm -rf "$scratch"' EXIT

if [[ "$mode" == verify || "$mode" == --verify-local ]]; then
  [[ -f "$authority" && ! -L "$authority" ]] || die E_AUTHORITY_MISSING "authority root is missing or not a regular file"
  [[ -f "$authority_pin" && ! -L "$authority_pin" ]] || die E_AUTHORITY_PIN "authority pin is missing or not a regular file"
  expected_authority_sha256=$(tr -d '[:space:]' <"$authority_pin")
  actual_authority_sha256=$(hash_file "$authority")
  [[ "$expected_authority_sha256" =~ ^[0-9a-f]{64}$ ]] || die E_AUTHORITY_PIN "verifier has no valid frozen authority hash"
  [[ "$actual_authority_sha256" == "$expected_authority_sha256" ]] || die E_AUTHORITY_HASH "authority root hash differs from the frozen verifier pin"
  if [[ "$mode" == verify ]]; then
    [[ "${TURN_EXPECTED_PRODUCT_SPEC_AUTHORITY_SHA256:-}" =~ ^[0-9a-f]{64}$ ]] ||
      die E_AUTHORITY_CI_PIN "a protected external authority pin is required"
    [[ "$TURN_EXPECTED_PRODUCT_SPEC_AUTHORITY_SHA256" == "$expected_authority_sha256" ]] ||
      die E_AUTHORITY_CI_PIN "external authority pin differs from repository pin"
  elif [[ -n "${TURN_EXPECTED_PRODUCT_SPEC_AUTHORITY_SHA256:-}" &&
          "$TURN_EXPECTED_PRODUCT_SPEC_AUTHORITY_SHA256" != "$expected_authority_sha256" ]]; then
    die E_AUTHORITY_CI_PIN "supplied external authority pin differs from repository pin"
  fi
fi

for required_file in "$requirements" "$acceptance" "$contract" "$gap_audit" "$coverage" "$census" "$mapping" "$manifest" "$decisions"; do
  [[ -s "$required_file" && ! -L "$required_file" ]] || die E_REQUIRED_FILE "missing, empty or symlinked ${required_file#$repo_root/}"
done

if ! awk -F '|' -v pairs="$scratch/requirement-pairs" -v statuses="$scratch/statuses" \
  -v canonical="$scratch/requirement-canonical" -v ids="$scratch/requirements" '
  /^\| Requirement \| Normative outcome \| Contract \| Current \| Acceptance \|[[:space:]]*$/ {
    in_table=1; tables++; next
  }
  in_table && /^\| --- \| --- \| --- \| --- \| --- \|[[:space:]]*$/ { next }
  in_table && /^\|/ {
    if ($0 ~ /\\\|/) {
      print "product-spec-acceptance: E_TABLE_ESCAPE: escaped pipe in requirement row at line " FNR > "/dev/stderr"
      bad=1; next
    }
    if (NF != 7 || $2 !~ /^ `PRD-[A-Z]+-[0-9][0-9][0-9]` $/) {
      print "product-spec-acceptance: E_TABLE_PARSE: malformed or unrecognised requirement row at line " FNR > "/dev/stderr"
      bad=1; next
    }
    id=$2; outcome=$3; contract=$4; status=$5; acceptance=$6
    gsub(/[ `]/, "", id); gsub(/[ `]/, "", acceptance)
    gsub(/^ +| +$/, "", outcome); gsub(/^ +| +$/, "", contract); gsub(/^ +| +$/, "", status)
    if (outcome == "" || contract == "" || acceptance == "") {
      print "product-spec-acceptance: E_TABLE_PARSE: incomplete requirement " id > "/dev/stderr"; bad=1
    }
    if (status != "baseline" && status != "partial" && status != "target" && status != "conflict" && status != "implemented") {
      print "product-spec-acceptance: E_STATUS: invalid current status for " id > "/dev/stderr"; bad=1
    }
    print id > ids
    print id " " acceptance > pairs
    print status > statuses
    print id "\t" acceptance "\t" outcome "\t" contract > canonical
    next
  }
  in_table { in_table=0 }
  END {
    if (tables == 0) {
      print "product-spec-acceptance: E_TABLE_PARSE: no requirement tables found" > "/dev/stderr"; bad=1
    }
    exit bad
  }
' "$requirements"; then
  exit 1
fi

if ! awk -F '|' -v canonical="$scratch/acceptance-canonical" -v pairs="$scratch/acceptance-pairs" '
  /^\| Acceptance \| Requirement \| Evidence \| Passing oracle \|[[:space:]]*$/ {
    in_table=1; tables++; next
  }
  in_table && /^\| --- \| --- \| --- \| --- \|[[:space:]]*$/ { next }
  in_table && /^\|/ {
    if ($0 ~ /\\\|/) {
      print "product-spec-acceptance: E_TABLE_ESCAPE: escaped pipe in acceptance row at line " FNR > "/dev/stderr"
      bad=1; next
    }
    if (NF != 6 || $2 !~ /^ `ACP-[A-Z]+-[0-9][0-9][0-9]` $/ ||
        $3 !~ /^ `PRD-[A-Z]+-[0-9][0-9][0-9]` $/) {
      print "product-spec-acceptance: E_TABLE_PARSE: malformed or unrecognised acceptance row at line " FNR > "/dev/stderr"
      bad=1; next
    }
    acceptance=$2; requirement=$3; evidence=$4; oracle=$5
    gsub(/[ `]/, "", acceptance); gsub(/[ `]/, "", requirement)
    gsub(/^ +| +$/, "", evidence); gsub(/^ +| +$/, "", oracle)
    if (evidence == "" || oracle == "") {
      print "product-spec-acceptance: E_TABLE_PARSE: incomplete acceptance " acceptance > "/dev/stderr"; bad=1
    }
    print acceptance " " requirement > pairs
    print acceptance "\t" requirement "\t" evidence "\t" oracle > canonical
    next
  }
  in_table { in_table=0 }
  END {
    if (tables == 0) {
      print "product-spec-acceptance: E_TABLE_PARSE: no acceptance tables found" > "/dev/stderr"; bad=1
    }
    exit bad
  }
' "$acceptance"; then
  exit 1
fi

[[ -s "$scratch/requirements" && -s "$scratch/acceptance-pairs" ]] || die E_EMPTY_INVENTORY "requirement or acceptance inventory is empty"

sort "$scratch/requirements" | uniq -d >"$scratch/duplicate-requirements"
cut -d ' ' -f1 "$scratch/acceptance-pairs" | sort | uniq -d >"$scratch/duplicate-acceptance"
cut -d ' ' -f2 "$scratch/acceptance-pairs" | sort | uniq -d >"$scratch/duplicate-mappings"
for duplicate_file in duplicate-requirements duplicate-acceptance duplicate-mappings; do
  [[ ! -s "$scratch/$duplicate_file" ]] || die E_DUPLICATE "duplicate ids in $duplicate_file"
done

sort -u "$scratch/requirements" >"$scratch/requirements-sorted"
cut -d ' ' -f2 "$scratch/acceptance-pairs" | sort -u >"$scratch/mapped-sorted"
diff -u "$scratch/requirements-sorted" "$scratch/mapped-sorted" >"$scratch/unmatched" ||
  die E_PAIR_SET "requirement and acceptance sets differ"

awk '
  {
    acceptance=$1; requirement=$2
    sub(/^ACP-/, "", acceptance); sub(/^PRD-/, "", requirement)
    if (acceptance != requirement) {
      print "product-spec-acceptance: E_PAIR_ID: mismatched pair " $1 " -> " $2 > "/dev/stderr"; bad=1
    }
  }
  END { exit bad }
' "$scratch/acceptance-pairs"

awk '
  {
    requirement=$1; acceptance=$2
    sub(/^PRD-/, "", requirement); sub(/^ACP-/, "", acceptance)
    if (requirement != acceptance) {
      print "product-spec-acceptance: E_PAIR_ID: inventory points to mismatched proof " $1 " -> " $2 > "/dev/stderr"; bad=1
    }
  }
  END { exit bad }
' "$scratch/requirement-pairs"

cut -d ' ' -f2 "$scratch/requirement-pairs" | sort -u >"$scratch/inventory-acceptance"
cut -d ' ' -f1 "$scratch/acceptance-pairs" | sort -u >"$scratch/acceptance-ids"
diff -u "$scratch/inventory-acceptance" "$scratch/acceptance-ids" >/dev/null ||
  die E_PAIR_SET "inventory acceptance links and proof ids differ"

requirement_count=$(wc -l <"$scratch/requirements" | tr -d ' ')
acceptance_count=$(wc -l <"$scratch/acceptance-pairs" | tr -d ' ')
[[ "$requirement_count" == "$expected_requirement_count" ]] || die E_REQUIREMENT_COUNT "expected $expected_requirement_count requirements, found $requirement_count"
[[ "$acceptance_count" == "$expected_acceptance_count" ]] || die E_ACCEPTANCE_COUNT "expected $expected_acceptance_count acceptances, found $acceptance_count"

cut -d '-' -f2 "$scratch/requirements" | sort -u >"$scratch/categories"
cat >"$scratch/expected-categories" <<'EOF'
ADP
ATT
CRE
CTX
FLW
HIE
LIF
OBS
OUT
RUN
SAF
SCL
TOP
VIE
VOI
EOF
diff -u "$scratch/expected-categories" "$scratch/categories" >/dev/null ||
  die E_CATEGORY_SET "capability category set changed"

[[ "$(sed -n '1p' "$coverage")" == '# product-capability-coverage-version: 1' ]] ||
  die E_COVERAGE_VERSION "unsupported or missing capability coverage version"
[[ "$(sed -n '2p' "$coverage")" == "# source-snapshot: $expected_coverage_snapshot" ]] ||
  die E_COVERAGE_SNAPSHOT "capability coverage source snapshot differs from the frozen snapshot"
[[ "$(sed -n '3p' "$coverage")" == "# source-tree-sha256: $expected_coverage_tree_sha256" ]] ||
  die E_COVERAGE_TREE "capability coverage source tree digest differs from the frozen snapshot"
[[ "$(sed -n '4p' "$coverage")" == '# source-tree-digest-algorithm: sha256(git ls-tree -r --full-tree source-snapshot)' ]] ||
  die E_COVERAGE_SCHEMA "capability source tree digest algorithm changed"
[[ "$(sed -n '5p' "$coverage")" == '# digest-algorithm: sha256(raw bytes of evidence_locator at source-snapshot)' ]] ||
  die E_COVERAGE_SCHEMA "capability evidence digest algorithm changed"
[[ "$(sed -n '6p' "$coverage")" == '# dispositions: adopted adapted rejected irrelevant' ]] ||
  die E_COVERAGE_SCHEMA "capability disposition vocabulary changed"
[[ "$(sed -n '7p' "$coverage")" == "# expected-feature-count: $expected_coverage_count" ]] ||
  die E_COVERAGE_COUNT "capability coverage metadata count differs from $expected_coverage_count"
[[ "$(sed -n '8p' "$coverage")" == $'feature_id\tcapability_key\tdescription\tdisposition\trequirement\tacceptance\tdecision\tevidence_locator\tevidence_sha256\trationale' ]] ||
  die E_COVERAGE_SCHEMA "capability coverage header changed"

if ! awk -F '\t' -v ids="$scratch/coverage-ids" -v keys="$scratch/coverage-keys" \
  -v descriptions="$scratch/coverage-descriptions" -v links="$scratch/coverage-links" '
  NR <= 8 { next }
  {
    if ($0 == "" || substr($0, 1, 1) == "#" || index($0, "\r") != 0) {
      print "product-spec-acceptance: E_COVERAGE_PARSE: blank, comment or carriage return in feature rows at line " FNR > "/dev/stderr"
      bad=1; next
    }
    if (NF != 10 || $1 !~ /^CAP-[0-9][0-9][0-9]$/ || $2 !~ /^[a-z][a-z0-9_]*$/ ||
        $5 !~ /^PRD-[A-Z]+-[0-9][0-9][0-9]$/ || $6 !~ /^ACP-[A-Z]+-[0-9][0-9][0-9]$/ ||
        $7 !~ /^ADR-[0-9][0-9][0-9]$/ || $8 !~ /^[A-Za-z0-9][A-Za-z0-9_.\/-]*$/ ||
        index($8, "..") != 0 || substr($8, 1, 1) == "/") {
      print "product-spec-acceptance: E_COVERAGE_PARSE: malformed feature row at line " FNR > "/dev/stderr"
      bad=1; next
    }
    if ($3 == "" || $10 == "") {
      print "product-spec-acceptance: E_COVERAGE_PARSE: incomplete feature row " $1 > "/dev/stderr"
      bad=1
    }
    if ($4 != "adopted" && $4 != "adapted" && $4 != "rejected" && $4 != "irrelevant") {
      print "product-spec-acceptance: E_COVERAGE_DISPOSITION: invalid disposition for " $1 > "/dev/stderr"
      bad=1
    }
    if (length($9) != 64 || $9 !~ /^[0-9a-f]+$/) {
      print "product-spec-acceptance: E_COVERAGE_DIGEST: invalid evidence digest for " $1 > "/dev/stderr"
      bad=1
    }
    print $1 > ids
    print $2 > keys
    print $3 > descriptions
    print $5 "\t" $6 "\t" $7 > links
  }
  END {
    if (NR < 9) {
      print "product-spec-acceptance: E_COVERAGE_COUNT: capability coverage has no feature rows" > "/dev/stderr"
      bad=1
    }
    exit bad
  }
' "$coverage"; then
  exit 1
fi

coverage_count=$(wc -l <"$scratch/coverage-ids" | tr -d ' ')
[[ "$coverage_count" == "$expected_coverage_count" ]] ||
  die E_COVERAGE_COUNT "expected $expected_coverage_count covered features, found $coverage_count"

sort "$scratch/coverage-ids" | uniq -d >"$scratch/coverage-duplicate-ids"
sort "$scratch/coverage-keys" | uniq -d >"$scratch/coverage-duplicate-keys"
sort "$scratch/coverage-descriptions" | uniq -d >"$scratch/coverage-duplicate-descriptions"
for duplicate_file in coverage-duplicate-ids coverage-duplicate-keys coverage-duplicate-descriptions; do
  [[ ! -s "$scratch/$duplicate_file" ]] ||
    die E_COVERAGE_DUPLICATE "duplicate capability identity in $duplicate_file"
done

awk '
  {
    expected=sprintf("CAP-%03d", NR)
    if ($0 != expected) {
      print "product-spec-acceptance: E_COVERAGE_SEQUENCE: expected " expected ", found " $0 > "/dev/stderr"
      bad=1
    }
  }
  END { exit bad }
' "$scratch/coverage-ids"

while IFS=$'\t' read -r requirement_id acceptance_id decision_id; do
  grep -Fxq "$requirement_id" "$scratch/requirements-sorted" ||
    die E_COVERAGE_REQUIREMENT "coverage links unknown requirement $requirement_id"
  grep -Fxq "$acceptance_id" "$scratch/acceptance-ids" ||
    die E_COVERAGE_ACCEPTANCE "coverage links unknown acceptance $acceptance_id"
  grep -Fxq "$acceptance_id $requirement_id" "$scratch/acceptance-pairs" ||
    die E_COVERAGE_PAIR "coverage link $acceptance_id does not prove $requirement_id"
  expected_decision=$(origin_for_id "$requirement_id")
  [[ "$decision_id" == "$expected_decision" ]] ||
    die E_COVERAGE_DECISION "coverage link $requirement_id declares $decision_id, expected $expected_decision"
done <"$scratch/coverage-links"

while IFS=$'\t' read -r id acceptance_id outcome contract_ref; do
  digest=$(printf '%s' "$id|$outcome|$contract_ref|$acceptance_id" | hash_stream)
  printf '%s\t%s\t%s\n' "$id" "$acceptance_id" "$digest"
done <"$scratch/requirement-canonical" >"$scratch/requirement-hashes"

while IFS=$'\t' read -r acceptance_id requirement_id evidence oracle; do
  digest=$(printf '%s' "$acceptance_id|$requirement_id|$evidence|$oracle" | hash_stream)
  printf '%s\t%s\t%s\n' "$acceptance_id" "$requirement_id" "$digest"
done <"$scratch/acceptance-canonical" >"$scratch/acceptance-hashes"

awk -F '\t' '
  NR == FNR { oracle_hash[$1]=$3; next }
  {
    if (!($2 in oracle_hash)) {
      print "product-spec-acceptance: E_MANIFEST_CONTENT: no oracle hash for " $2 > "/dev/stderr"; bad=1
    }
    print "v1|" $1 "|" $2 "|" $3 "|" oracle_hash[$2]
  }
  END { exit bad }
' "$scratch/acceptance-hashes" "$scratch/requirement-hashes" | sort >"$scratch/generated-manifest"

emit_manifest() {
  printf '%s\n' '# manifest-version: 1'
  printf '%s\n' '# Format: version|requirement|acceptance|requirement-sha256|oracle-sha256|originating-decision'
  printf '%s\n' '# Hash inputs exclude mutable implementation status but include ids, normative outcome/contract and evidence/oracle.'
  while IFS='|' read -r version id acceptance_id requirement_hash oracle_hash; do
    origin=$(origin_for_id "$id")
    printf '%s|%s|%s|%s|%s|%s\n' "$version" "$id" "$acceptance_id" "$requirement_hash" "$oracle_hash" "$origin"
  done <"$scratch/generated-manifest"
}

if [[ "$mode" == --emit-manifest ]]; then
  emit_manifest
  exit 0
fi

grep -Fxq '# manifest-version: 1' "$manifest" || die E_MANIFEST_VERSION "unsupported or missing manifest version"
if ! awk -F '|' -v projected="$scratch/manifest-projected" -v origins="$scratch/manifest-origins" '
  /^#/ || /^$/ { next }
  {
    if (NF != 6 || $1 != "v1" || $2 !~ /^PRD-[A-Z]+-[0-9][0-9][0-9]$/ ||
        $3 !~ /^ACP-[A-Z]+-[0-9][0-9][0-9]$/ || length($4) != 64 || $4 !~ /^[0-9a-f]+$/ ||
        length($5) != 64 || $5 !~ /^[0-9a-f]+$/ || $6 !~ /^ADR-[0-9][0-9][0-9]$/) {
      print "product-spec-acceptance: E_MANIFEST_PARSE: malformed manifest row: " $0 > "/dev/stderr"; bad=1
    }
    print $1 "|" $2 "|" $3 "|" $4 "|" $5 > projected
    print $2 "\t" $6 > origins
  }
  END { exit bad }
' "$manifest"; then
  exit 1
fi

cut -f1 "$scratch/manifest-origins" | sort | uniq -d >"$scratch/manifest-duplicate-requirements"
[[ ! -s "$scratch/manifest-duplicate-requirements" ]] || die E_MANIFEST_DUPLICATE "duplicate manifest requirement identity"
sort -u "$scratch/manifest-projected" -o "$scratch/manifest-projected"
diff -u "$scratch/manifest-projected" "$scratch/generated-manifest" >/dev/null ||
  die E_MANIFEST_CONTENT "frozen requirement/oracle manifest differs"

while IFS=$'\t' read -r id decision; do
  expected_decision=$(origin_for_id "$id")
  [[ "$decision" == "$expected_decision" ]] ||
    die E_ORIGIN "$id declares $decision but the frozen origin rule requires $expected_decision"
done <"$scratch/manifest-origins"

emit_authority() {
  printf 'schema\t1\n'
  printf 'count\trequirements\t%s\n' "$expected_requirement_count"
  printf 'count\tacceptance\t%s\n' "$expected_acceptance_count"
  printf 'count\tcoverage\t%s\n' "$expected_coverage_count"
  printf 'file\traw\tdocs/PRODUCT_REQUIREMENTS_V1.manifest\t%s\n' "$(hash_file "$manifest")"
  while IFS= read -r path; do
    if [[ "$path" == docs/PRODUCT_REQUIREMENTS.md ]]; then
      digest=$(redacted_requirement_hash "$repo_root/$path")
      format=status-redacted-v1
    else
      digest=$(hash_file "$repo_root/$path")
      format=raw
    fi
    printf 'file\t%s\t%s\t%s\n' "$format" "$path" "$digest"
  done < <(normative_paths)
  for decision in ADR-059 ADR-060 ADR-061 ADR-062 ADR-063 ADR-064 ADR-065 ADR-066 ADR-067; do
    printf 'section\t%s\tDECISIONS.md\t%s\n' "$decision" "$(decision_section_hash "$decision")"
  done
  sort "$scratch/manifest-origins" | while IFS=$'\t' read -r id decision; do
    printf 'origin\t%s\t%s\n' "$id" "$decision"
  done
}

if [[ "$mode" == --emit-authority ]]; then
  emit_authority
  exit 0
fi

[[ "$mode" == verify || "$mode" == --verify-local ]] ||
  die E_USAGE "usage: verify-product-spec.sh [verify|--verify-local|--emit-manifest|--emit-authority]"

if ! git -C "$repo_root" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  die E_GIT "repository identity is unavailable"
fi

{
  normative_paths
  printf '%s\n' \
    DECISIONS.md \
    docs/PRODUCT_REQUIREMENTS_V1.manifest \
    docs/PRODUCT_SPEC_V1.authority \
    docs/PRODUCT_SPEC_V1.sha256
} | sort -u >"$scratch/tracked-authority-paths"

while IFS= read -r path; do
  [[ -f "$repo_root/$path" && ! -L "$repo_root/$path" ]] || die E_AUTHORITY_FILE "authority path is missing, non-regular or symlinked: $path"
  git -C "$repo_root" ls-files --error-unmatch -- "$path" >/dev/null 2>&1 || die E_UNTRACKED_AUTHORITY "authority path is not tracked: $path"
  git -C "$repo_root" diff --quiet HEAD -- "$path" || die E_DIRTY_AUTHORITY "authority path differs from HEAD: $path"
done <"$scratch/tracked-authority-paths"

emit_authority >"$scratch/generated-authority"
diff -u "$authority" "$scratch/generated-authority" >/dev/null ||
  die E_AUTHORITY_CONTENT "authority root does not match current normative files"

awk -F '\t' '$1 == "origin" { print $2 "\t" $3 }' "$authority" | sort >"$scratch/authority-origins"
sort "$scratch/manifest-origins" >"$scratch/sorted-manifest-origins"
diff -u "$scratch/authority-origins" "$scratch/sorted-manifest-origins" >/dev/null ||
  die E_ORIGIN "manifest decisions differ from frozen requirement origins"

for decision in ADR-059 ADR-060 ADR-061 ADR-062 ADR-063 ADR-064 ADR-065 ADR-066 ADR-067; do
  section="$scratch/$decision.section"
  awk -v heading="## $decision " '
    index($0, heading) == 1 { active=1 }
    active && seen && /^## ADR-[0-9][0-9][0-9] / { exit }
    active { print; seen=1 }
  ' "$decisions" >"$section"
  [[ -s "$section" ]] || die E_DECISION "missing $decision"
  grep -Fq '**Status:** Accepted' "$section" || die E_DECISION "decision is not accepted: $decision"
done

awk -F '\t' '$1 == "file" { print $3 }' "$authority" | sort -u >"$scratch/authority-file-paths"
grep -o '`[^`]*\.md`' "$scratch/requirement-canonical" | tr -d '`' | sort -u >"$scratch/contract-doc-refs" || true
while IFS= read -r ref; do
  [[ -n "$ref" ]] || continue
  if [[ -f "$repo_root/$ref" ]]; then
    resolved=$ref
  elif [[ -f "$repo_root/docs/$ref" ]]; then
    resolved="docs/$ref"
  else
    die E_CONTRACT_REFERENCE "missing referenced contract $ref"
  fi
  grep -Fxq "$resolved" "$scratch/authority-file-paths" || die E_AUTHORITY_CLOSURE "referenced contract is outside authority root: $resolved"
done <"$scratch/contract-doc-refs"

grep -Fq '**Status:** accepted post-v0.1 product contract' "$contract" ||
  die E_CONTRACT_STATUS "master contract must state accepted target status"
grep -Fq '**Implementation status:** mixed; this file is not a claim that the target is built' "$requirements" ||
  die E_INVENTORY_STATUS "inventory must distinguish specification from implementation"
if grep -En '(^|[^[:alnum:]_])(TODO|TBD|FIXME)([^[:alnum:]_]|$)' "$contract" "$requirements" "$acceptance" "$gap_audit" "$coverage"; then
  die E_PLACEHOLDER "unresolved placeholder in normative product specification"
fi

category_count=$(wc -l <"$scratch/categories" | tr -d ' ')
status_summary=$(sort "$scratch/statuses" | uniq -c | awk '{printf "%s%s=%s", separator, $2, $1; separator=", "}')
echo "product-spec-acceptance: authority $expected_authority_sha256, manifest v1, ${requirement_count} requirements, ${acceptance_count} proof obligations, ${coverage_count} covered source features, ${category_count} categories (${status_summary})"
