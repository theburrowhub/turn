#!/usr/bin/env bash
set -euo pipefail

export LC_ALL=C

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
protocol_path=${STATE_PROTOCOL_PATH:-"$repo_root/docs/PROTOCOL.md"}
manifest_path=${STATE_MANIFEST_PATH:-"$repo_root/docs/STATE_FAMILY_MANIFEST_VNEXT.tsv"}
mode=verify

# Independent reviewed freeze over the declaration-derived canonical rows.
oracle_row_count=345
oracle_state_family_count=328
oracle_request_value_count=17
oracle_rows_sha256=933b64896998fa700cdb984178a13c5f3e1685c910ced92a7052add2ab47c5b1

case "${1:-}" in
  "") ;;
  --emit) mode=emit ;;
  *) echo "usage: $0 [--emit]" >&2; exit 64 ;;
esac

if [[ ! -f "$protocol_path" ]]; then
  echo "E_STATE_PROTOCOL_MISSING: $protocol_path" >&2
  exit 1
fi

hash_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{ print $1 }'
  else
    echo "E_STATE_HASH_TOOL: sha256sum or shasum is required" >&2
    exit 1
  fi
}

tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/turn-state-manifest.XXXXXX")
trap 'rm -rf -- "$tmp_dir"' EXIT
declaration_rows="$tmp_dir/declarations.tsv"
sorted_declarations="$tmp_dir/declarations.sorted.tsv"
presentation_rows="$tmp_dir/presentation.tsv"
sorted_presentation="$tmp_dir/presentation.sorted.tsv"
manifest_rows="$tmp_dir/manifest.sorted.tsv"

census_starts=$(awk '/^StateDeclarationCensus\.vNext = \{$/ { count++ } END { print count+0 }' "$protocol_path")
manifest_starts=$(awk '/^StateFamilyManifest\.vNext = \{$/ { count++ } END { print count+0 }' "$protocol_path")
request_markers=$(awk '/^The following named wire projections are explicitly annotated `request_value`/ { count++ } END { print count+0 }' "$protocol_path")
if [[ "$census_starts" != 1 || "$manifest_starts" != 1 || "$request_markers" != 1 ]]; then
  echo "E_STATE_PROTOCOL_SHAPE: census_starts=$census_starts manifest_starts=$manifest_starts request_markers=$request_markers" >&2
  exit 1
fi

# Authoritative input: every explicit schema/reducer marker must have exactly
# one matching protocol declaration followed by exactly one classification.
# The parser scans the whole document, not the presentation manifest block.
awk -F '|' '
BEGIN { OFS="\t" }
function trim(value) {
  sub(/^[[:space:]]+/, "", value)
  sub(/[[:space:]]+$/, "", value)
  return value
}
function fail(code, line, detail) {
  printf "%s: line=%d %s\n", code, line, detail > "/dev/stderr"
  exit 2
}
{
  current=trim($0)
  if (current ~ /^(StateFamilyDeclaration|RequestValueDeclaration)\|/) {
    marker_line=NR
    marker_count=split(current, marker, /\|/)
    if (marker_count != 2 || marker[2] !~ /^[A-Z][A-Za-z0-9]*$/) {
      fail("E_STATE_DECLARATION_MARKER", marker_line, current)
    }
    marker_kind=marker[1]
    name=marker[2]
    if (seen[name]++) {
      fail("E_STATE_DECLARATION_DUPLICATE", marker_line, name)
    }

    if ((getline protocol_line) <= 0) {
      fail("E_STATE_DECLARATION_UNANNOTATED", marker_line, name)
    }
    protocol_line=trim(protocol_line)
    protocol_count=split(protocol_line, protocol, /\|/)
    if (protocol_count != 3 || protocol[1] != "@protocol_decl" ||
        protocol[2] != "vNext" || protocol[3] != name) {
      fail("E_STATE_DECLARATION_UNANNOTATED", marker_line, name)
    }

    if ((getline class_line) <= 0) {
      fail("E_STATE_DECLARATION_UNCLASSIFIED", marker_line, name)
    }
    class_line=trim(class_line)
    class_count=split(class_line, classification, /\|/)

    if (marker_kind == "StateFamilyDeclaration") {
      if (class_count != 4 || classification[1] != "@state_family" ||
          classification[2] !~ /^(durable|ephemeral)$/ ||
          classification[3] !~ /^(Installation|Workspace|ExecutionTarget|TaggedOwner|ephemeral)$/ ||
          classification[4] == "") {
        fail("E_STATE_DECLARATION_UNCLASSIFIED", marker_line, name)
      }
      if ((classification[2] == "durable" && classification[3] == "ephemeral") ||
          (classification[2] == "ephemeral" && classification[3] != "ephemeral")) {
        fail("E_STATE_DECLARATION_COORDINATE", marker_line, name)
      }
      print "vNext", name, "state_family", classification[2], classification[3], classification[4]
    } else {
      if (class_count != 4 || classification[1] != "@request_value" ||
          classification[2] != "request_value" || classification[3] != "request" ||
          classification[4] != "none") {
        fail("E_STATE_DECLARATION_UNCLASSIFIED", marker_line, name)
      }
      print "vNext", name, "request_value", "request_value", "request", "none"
    }
    next
  }
  if (current ~ /^@(protocol_decl|state_family|request_value)\|/) {
    fail("E_STATE_DECLARATION_ORPHAN", NR, current)
  }
}
END {
  if (NR == 0) {
    print "E_STATE_DECLARATION_EMPTY" > "/dev/stderr"
    exit 2
  }
}
' "$protocol_path" >"$declaration_rows"

if [[ ! -s "$declaration_rows" ]]; then
  echo "E_STATE_DECLARATION_EMPTY" >&2
  exit 1
fi
sort -t $'\t' -k2,2 "$declaration_rows" >"$sorted_declarations"

# Presentation mirror: parsed separately and never used as declaration input.
awk '
BEGIN { in_manifest=0; section=""; section_count=0; OFS="\t" }
/^StateFamilyManifest\.vNext = \{$/ { in_manifest=1; next }
in_manifest && /^  durable\.Installation = \{$/ { section="Installation"; life="durable"; section_count++; next }
in_manifest && /^  durable\.Workspace = \{$/ { section="Workspace"; life="durable"; section_count++; next }
in_manifest && /^  durable\.ExecutionTarget = \{$/ { section="ExecutionTarget"; life="durable"; section_count++; next }
in_manifest && /^  durable\.TaggedOwner = \{$/ { section="TaggedOwner"; life="durable"; section_count++; next }
in_manifest && /^  ephemeral = \{$/ { section="ephemeral"; life="ephemeral"; section_count++; next }
in_manifest && section != "" && /^  },?$/ { section=""; next }
in_manifest && section == "" && /^}$/ { in_manifest=0; ended=1; next }
in_manifest && section != "" {
  line=$0
  sub(/^[[:space:]]+/, "", line)
  sub(/[[:space:]]+$/, "", line)
  n=split(line, parts, /,[[:space:]]*/)
  for (i=1; i<=n; i++) {
    token=parts[i]
    gsub(/^[[:space:]]+|[[:space:]]+$/, "", token)
    if (token == "") continue
    name=token
    stream_owner=section
    if (section == "Installation") owner="Installation(daemon_generation)"
    else if (section == "Workspace") owner="Workspace(daemon_generation,WorkspaceId)"
    else if (section == "ExecutionTarget") owner="ExecutionTarget(daemon_generation,ExecutionTargetId,target_generation)"
    else owner=""
    if (match(token, /\(owner_key=/)) {
      name=substr(token, 1, RSTART-1)
      owner=substr(token, RSTART+11)
      sub(/\)$/, "", owner)
    }
    if (name !~ /^[A-Z][A-Za-z0-9]*$/ || owner == "") {
      printf "E_STATE_PRESENTATION_TOKEN: section=%s token=%s\n", section, token > "/dev/stderr"
      exit 2
    }
    print "vNext", name, "state_family", life, stream_owner, owner
  }
}
END {
  if (!ended || section_count != 5) {
    printf "E_STATE_PRESENTATION_SECTIONS: ended=%d sections=%d\n", ended+0, section_count+0 > "/dev/stderr"
    exit 2
  }
}
' "$protocol_path" >"$presentation_rows"

perl -0777 -ne '
  if (/The following named wire projections are explicitly annotated `request_value`.*?therefore absent from the state manifest:(.*?)\. Their logical response bodies/s) {
    $block=$1;
    while ($block =~ /`([A-Z][A-Za-z0-9]*)`/g) {
      print "vNext\t$1\trequest_value\trequest_value\trequest\tnone\n";
    }
    $found=1;
  }
  END { exit 2 unless $found }
' "$protocol_path" >>"$presentation_rows" || {
  echo "E_STATE_REQUEST_VALUES: cannot parse closed request-value presentation list" >&2
  exit 1
}

duplicate_presentation=$(cut -f2 "$presentation_rows" | sort | uniq -d | head -1)
if [[ -n "$duplicate_presentation" ]]; then
  echo "E_STATE_PRESENTATION_DUPLICATE: $duplicate_presentation" >&2
  exit 1
fi
sort -t $'\t' -k2,2 "$presentation_rows" >"$sorted_presentation"

if ! diff -u "$sorted_declarations" "$sorted_presentation" >"$tmp_dir/presentation.diff"; then
  echo "E_STATE_PRESENTATION_DRIFT: declarations and presentation manifest are not bijective" >&2
  sed -n '1,120p' "$tmp_dir/presentation.diff" >&2
  exit 1
fi

if awk -F '\t' '$2 ~ /^(Default|Misc|Other)$/ || $6 ~ /(^|[+.])(default|misc|other|wildcard)([+.()]|$)/ { bad=1 } END { exit !bad }' "$sorted_declarations"; then
  echo "E_STATE_DECLARATION_OPEN_OWNER: default/misc/wildcard owner is forbidden" >&2
  exit 1
fi

actual_row_count=$(wc -l <"$sorted_declarations" | tr -d '[:space:]')
actual_state_family_count=$(awk -F '\t' '$3 == "state_family" { count++ } END { print count+0 }' "$sorted_declarations")
actual_request_value_count=$(awk -F '\t' '$3 == "request_value" { count++ } END { print count+0 }' "$sorted_declarations")
if [[ "$actual_row_count" != "$oracle_row_count" ||
      "$actual_state_family_count" != "$oracle_state_family_count" ||
      "$actual_request_value_count" != "$oracle_request_value_count" ]]; then
  echo "E_STATE_ORACLE_COUNT: expected=$oracle_row_count/$oracle_state_family_count/$oracle_request_value_count actual=$actual_row_count/$actual_state_family_count/$actual_request_value_count" >&2
  exit 1
fi
actual_rows_sha256=$(hash_file "$sorted_declarations")
if [[ "$actual_rows_sha256" != "$oracle_rows_sha256" ]]; then
  echo "E_STATE_ORACLE_DIGEST: expected=$oracle_rows_sha256 actual=$actual_rows_sha256" >&2
  exit 1
fi

header=$'schema_version\tname\tdeclaration_class\tlifetime\tstream_owner\towner_key'
if [[ "$mode" == emit ]]; then
  output_tmp="$tmp_dir/manifest.tsv"
  {
    printf '%s\n' "$header"
    cat "$sorted_declarations"
  } >"$output_tmp"
  mv -- "$output_tmp" "$manifest_path"
  echo "STATE_MANIFEST_EMITTED: $actual_row_count declaration-derived rows -> $manifest_path"
  exit 0
fi

if [[ ! -f "$manifest_path" ]]; then
  echo "E_STATE_MANIFEST_MISSING: $manifest_path" >&2
  exit 1
fi
if [[ "$(head -n 1 "$manifest_path")" != "$header" ]]; then
  echo "E_STATE_MANIFEST_HEADER" >&2
  exit 1
fi

awk -F '\t' '
NR == 1 { next }
NF != 6 { printf "E_STATE_MANIFEST_COLUMNS: line=%d fields=%d\n", NR, NF > "/dev/stderr"; bad=1; next }
$1 != "vNext" { printf "E_STATE_MANIFEST_VERSION: line=%d value=%s\n", NR, $1 > "/dev/stderr"; bad=1 }
$2 !~ /^[A-Z][A-Za-z0-9]*$/ { printf "E_STATE_MANIFEST_NAME: line=%d value=%s\n", NR, $2 > "/dev/stderr"; bad=1 }
$3 == "state_family" && $4 !~ /^(durable|ephemeral)$/ { printf "E_STATE_MANIFEST_LIFETIME: line=%d\n", NR > "/dev/stderr"; bad=1 }
$3 == "request_value" && ($4 != "request_value" || $5 != "request" || $6 != "none") { printf "E_STATE_MANIFEST_REQUEST_VALUE: line=%d\n", NR > "/dev/stderr"; bad=1 }
$3 !~ /^(state_family|request_value)$/ { printf "E_STATE_MANIFEST_CLASS: line=%d\n", NR > "/dev/stderr"; bad=1 }
$5 == "" || $6 == "" { printf "E_STATE_MANIFEST_OWNER: line=%d\n", NR > "/dev/stderr"; bad=1 }
{ seen[$2]++; if (seen[$2] > 1) { printf "E_STATE_MANIFEST_DUPLICATE: %s\n", $2 > "/dev/stderr"; bad=1 } }
END { if (NR == 1) { print "E_STATE_MANIFEST_EMPTY" > "/dev/stderr"; bad=1 }; exit bad }
' "$manifest_path" || exit 1

tail -n +2 "$manifest_path" | sort -t $'\t' -k2,2 >"$manifest_rows"
if ! diff -u "$sorted_declarations" "$manifest_rows" >"$tmp_dir/manifest.diff"; then
  echo "E_STATE_MANIFEST_DRIFT: declarations and TSV are not bijective" >&2
  sed -n '1,120p' "$tmp_dir/manifest.diff" >&2
  exit 1
fi

echo "STATE_MANIFEST_OK: $actual_row_count declaration-derived rows"
