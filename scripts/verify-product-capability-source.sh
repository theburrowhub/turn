#!/usr/bin/env bash
set -euo pipefail

export LC_ALL=C

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
coverage="$repo_root/docs/PRODUCT_CAPABILITY_COVERAGE_V1.tsv"
census="$repo_root/docs/PRODUCT_CAPABILITY_SOURCE_CENSUS_V1.tsv"
mapping="$repo_root/docs/PRODUCT_CAPABILITY_SOURCE_MAPPING_V1.tsv"
source_repository=${1:-}

die() {
  code=$1
  shift
  echo "product-capability-source-acceptance: $code: $*" >&2
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

[[ "$#" == 1 ]] ||
  die E_USAGE "usage: verify-product-capability-source.sh /path/to/audited-source-repository"
[[ -z "${GIT_DIR+x}" && -z "${GIT_WORK_TREE+x}" && -z "${GIT_COMMON_DIR+x}" &&
   -z "${GIT_OBJECT_DIRECTORY+x}" && -z "${GIT_ALTERNATE_OBJECT_DIRECTORIES+x}" &&
   -z "${GIT_INDEX_FILE+x}" && -z "${GIT_NAMESPACE+x}" &&
   -z "${GIT_REPLACE_REF_BASE+x}" && -z "${GIT_CONFIG_COUNT+x}" &&
   -z "${GIT_CONFIG_PARAMETERS+x}" ]] ||
  die E_GIT_ENV "ambient Git repository, object-store or config overrides are forbidden"
export GIT_NO_REPLACE_OBJECTS=1
export GIT_NO_LAZY_FETCH=1
export GIT_TERMINAL_PROMPT=0
export GIT_CONFIG_NOSYSTEM=1
export GIT_CONFIG_SYSTEM=/dev/null
export GIT_CONFIG_GLOBAL=/dev/null

[[ -d "$source_repository" ]] || die E_SOURCE_REPOSITORY "argument is not a readable Git repository"
source_repository=$(cd "$source_repository" 2>/dev/null && pwd -P) ||
  die E_SOURCE_REPOSITORY "argument is not a readable Git repository"
[[ -f "$coverage" && ! -L "$coverage" ]] || die E_LEDGER "capability ledger is unavailable"
[[ -f "$census" && ! -L "$census" ]] || die E_CENSUS "capability source census is unavailable"
[[ -f "$mapping" && ! -L "$mapping" ]] || die E_MAPPING "capability source mapping is unavailable"
inside_work_tree=$(git -C "$source_repository" rev-parse --is-inside-work-tree 2>/dev/null || true)
bare_repository=$(git -C "$source_repository" rev-parse --is-bare-repository 2>/dev/null || true)
[[ "$inside_work_tree" == true || "$bare_repository" == true ]] ||
  die E_SOURCE_REPOSITORY "argument is not a readable Git repository"

snapshot=$(sed -n '2s/^# source-snapshot: //p' "$coverage")
expected_tree=$(sed -n '3s/^# source-tree-sha256: //p' "$coverage")
expected_count=$(sed -n '7s/^# expected-feature-count: //p' "$coverage")
census_snapshot=$(sed -n '2s/^# source-snapshot: //p' "$census")
selector_version=$(sed -n '3s/^# candidate-selector-version: //p' "$census")
expected_module_count=$(sed -n '4s/^# expected-module-count: //p' "$census")
expected_registry_count=$(sed -n '5s/^# expected-registry-count: //p' "$census")
expected_candidate_count=$(sed -n '6s/^# expected-candidate-count: //p' "$census")
mapping_snapshot=$(sed -n '2s/^# source-snapshot: //p' "$mapping")
mapping_candidate_count=$(sed -n '3s/^# expected-candidate-count: //p' "$mapping")
expected_mapping_count=$(sed -n '4s/^# expected-mapping-count: //p' "$mapping")
[[ "$snapshot" =~ ^[0-9a-f]{40}$ ]] || die E_SNAPSHOT "ledger snapshot is malformed"
[[ "$expected_tree" =~ ^[0-9a-f]{64}$ ]] || die E_TREE "ledger tree digest is malformed"
[[ "$expected_count" =~ ^[1-9][0-9]*$ ]] || die E_COUNT "ledger feature count is malformed"
[[ "$census_snapshot" == "$snapshot" ]] || die E_CENSUS_SNAPSHOT "census and ledger snapshots differ"
[[ "$mapping_snapshot" == "$snapshot" ]] || die E_MAPPING_SNAPSHOT "mapping and ledger snapshots differ"
[[ "$selector_version" == 1 ]] || die E_CENSUS_SELECTOR "unsupported candidate selector version"
[[ "$expected_module_count" =~ ^[1-9][0-9]*$ ]] || die E_CENSUS_COUNT "module count is malformed"
[[ "$expected_registry_count" =~ ^[1-9][0-9]*$ ]] || die E_CENSUS_COUNT "registry count is malformed"
[[ "$expected_candidate_count" =~ ^[1-9][0-9]*$ ]] || die E_CENSUS_COUNT "candidate count is malformed"
[[ "$mapping_candidate_count" == "$expected_candidate_count" ]] ||
  die E_MAPPING_COUNT "mapping and census candidate counts differ"
[[ "$expected_mapping_count" =~ ^[1-9][0-9]*$ ]] || die E_MAPPING_COUNT "mapping count is malformed"
[[ $((expected_module_count + expected_registry_count)) == "$expected_candidate_count" ]] ||
  die E_CENSUS_COUNT "module and registry counts do not sum to the candidate count"
git -C "$source_repository" cat-file -e "$snapshot^{commit}" 2>/dev/null ||
  die E_SNAPSHOT "frozen source snapshot is absent"

actual_tree=$(git -C "$source_repository" ls-tree -r --full-tree "$snapshot" | hash_stream)
[[ "$actual_tree" == "$expected_tree" ]] || die E_TREE "frozen source tree digest differs"

checked=0
while IFS=$'\t' read -r feature_id capability_key description disposition requirement acceptance decision locator digest rationale; do
  [[ "$feature_id" =~ ^CAP-[0-9]{3}$ ]] || continue
  [[ -n "$locator" && "$locator" != /* && "$locator" != *..* ]] ||
    die E_LOCATOR "$feature_id has an unsafe evidence locator"
  [[ "$(git -C "$source_repository" cat-file -t "$snapshot:$locator" 2>/dev/null || true)" == blob ]] ||
    die E_LOCATOR "$feature_id evidence is absent or is not a blob"
  actual_digest=$(git -C "$source_repository" show "$snapshot:$locator" | hash_stream)
  [[ "$actual_digest" == "$digest" ]] || die E_DIGEST "$feature_id evidence digest differs"
  checked=$((checked + 1))
done <"$coverage"

[[ "$checked" == "$expected_count" ]] ||
  die E_COUNT "expected $expected_count evidence rows, checked $checked"

scratch=$(mktemp -d "${TMPDIR:-/tmp}/turn-capability-census.XXXXXX")
trap 'rm -rf "$scratch"' EXIT

# Selector v1 deliberately has no catch-all. It inventories first-party runtime modules, top-level
# product documents, renderer-owned runtime assets/styles and installer entry points, plus the closed
# registries that define externally reachable operations, node surfaces, built-in agent identities,
# settings sections, commands, shortcuts and audited internal verb/route/kind/capability vocabularies.
# Tests, fixtures, design-process archives, packaging/build infrastructure outside the executable
# installers, vendored/generated data and non-renderer binary assets are not product-capability candidates.
git -C "$source_repository" ls-tree -r --name-only "$snapshot" |
  awk '
    ($0 == "README.md" ||
     $0 ~ /^docs\/[^\/]+\.md$/ ||
     $0 ~ /^src\/.*\.(ts|tsx)$/ ||
     $0 ~ /^src\/renderer\/.*\.(css|html|svg|webp|png)$/ ||
     $0 ~ /^scripts\/install[^\/]*\.sh$/) &&
    $0 !~ /(^|\/)(__fixtures__|__snapshots__)(\/|$)/ &&
    $0 !~ /\.d\.ts$/ &&
    $0 !~ /\.(test|spec)\.(ts|tsx)$/ {
      print "module\t" $0 "\t-"
    }
  ' >"$scratch/modules"

git -C "$source_repository" show "$snapshot:src/shared/ipc.ts" 2>/dev/null |
  sed -n '/^export const IPC = {/,/^}/p' |
  sed -nE 's/^  ([A-Za-z][A-Za-z0-9]*):.*/registry_ipc\tsrc\/shared\/ipc.ts\t\1/p' \
  >"$scratch/registry-ipc" || die E_CENSUS_SELECTOR "IPC registry cannot be enumerated"

git -C "$source_repository" show "$snapshot:src/shared/types.ts" 2>/dev/null |
  sed -n 's/^export type NodeKind = //p' |
  tr '|' '\n' |
  sed -nE "s/^[[:space:]]*'([^']+)'[[:space:]]*$/registry_node_kind\tsrc\/shared\/types.ts\t\1/p" \
  >"$scratch/registry-node-kind" || die E_CENSUS_SELECTOR "node-kind registry cannot be enumerated"

git -C "$source_repository" show "$snapshot:src/shared/agents/config.ts" 2>/dev/null |
  sed -n 's/^export type BuiltinAgentId = //p' |
  tr '|' '\n' |
  sed -nE "s/^[[:space:]]*'([^']+)'[[:space:]]*$/registry_agent_adapter\tsrc\/shared\/agents\/config.ts\t\1/p" \
  >"$scratch/registry-agent-adapter" || die E_CENSUS_SELECTOR "agent registry cannot be enumerated"

if [[ "$(git -C "$source_repository" cat-file -t "$snapshot:src/renderer/components/settings/nav.ts" 2>/dev/null || true)" == blob ]]; then
  git -C "$source_repository" show "$snapshot:src/renderer/components/settings/nav.ts" |
    sed -n '/^export type SettingsSectionId =/,/^$/p' |
    sed -nE "s/^[[:space:]]*\\| '([^']+)'[[:space:]]*$/registry_settings_section\tsrc\/renderer\/components\/settings\/nav.ts\t\1/p" \
    >"$scratch/registry-settings-section"
else
  : >"$scratch/registry-settings-section"
fi

if [[ "$(git -C "$source_repository" cat-file -t "$snapshot:src/renderer/canvas/Canvas.tsx" 2>/dev/null || true)" == blob ]]; then
  git -C "$source_repository" show "$snapshot:src/renderer/canvas/Canvas.tsx" |
    sed -n '/const buildCommands = useCallback/,/^  \/\/ Build the palette/p' |
    sed -nE "s/.*id: '([^']+)'.*/registry_command\tsrc\/renderer\/canvas\/Canvas.tsx\t\1/p" |
    sort -u >"$scratch/registry-command"
  git -C "$source_repository" show "$snapshot:src/renderer/canvas/Canvas.tsx" |
    sed -n '/const buildCommands = useCallback/,/^  \/\/ Build the palette/p' |
    sed -nE 's/.*id: `([^`]+)`.*/registry_command_family\tsrc\/renderer\/canvas\/Canvas.tsx\t\1/p' |
    sort -u >"$scratch/registry-command-family"
else
  : >"$scratch/registry-command"
  : >"$scratch/registry-command-family"
fi

if [[ "$(git -C "$source_repository" cat-file -t "$snapshot:src/renderer/components/ShortcutsPanel.tsx" 2>/dev/null || true)" == blob ]]; then
  git -C "$source_repository" show "$snapshot:src/renderer/components/ShortcutsPanel.tsx" |
    sed -n '/function buildSections/,/^}/p' |
    sed -nE "s/.*label: '([^']+)'.*/registry_shortcut\tsrc\/renderer\/components\/ShortcutsPanel.tsx\t\1/p" |
    sort -u >"$scratch/registry-shortcut"
  if git -C "$source_repository" show "$snapshot:src/renderer/components/ShortcutsPanel.tsx" |
    grep -F '{ keys: dictationKeys, label: dictationLabel }' >/dev/null; then
    printf '%s\n' $'registry_shortcut\tsrc/renderer/components/ShortcutsPanel.tsx\tdictate' \
      >>"$scratch/registry-shortcut"
    sort -u "$scratch/registry-shortcut" -o "$scratch/registry-shortcut"
  fi
else
  : >"$scratch/registry-shortcut"
fi

git -C "$source_repository" show "$snapshot:src/shared/types.ts" 2>/dev/null |
  sed -n '/^export interface Settings {/,/^}/p' |
  sed -nE 's/^  ([A-Za-z][A-Za-z0-9]*)(\?)?:.*/registry_setting_key\tsrc\/shared\/types.ts\t\1/p' >"$scratch/registry-setting-key" || die E_CENSUS_SELECTOR "Settings keys cannot be enumerated"

emit_closed_union() {
  locator=$1
  union_name=$2
  member_file="$scratch/closed-union-members"
  git -C "$source_repository" show "$snapshot:$locator" 2>/dev/null |
    awk -v declaration="export type $union_name =" '
      index($0, declaration) == 1 { active=1; print; next }
      active && $0 ~ /^[[:space:]]*\|/ { print; next }
      active { exit }
    ' |
    grep -oE "'[^']+'" |
    tr -d "'" >"$member_file" || true
  # The selector is reusable by its mutation fixture: an audited snapshot may omit one of the
  # known registries entirely.  In the frozen production snapshot, removing that registry (or a
  # member) still changes the enumerated set and therefore fails E_CENSUS_SOURCE_SET below.
  [[ -s "$member_file" ]] || return 0
  while IFS= read -r member; do
    printf 'registry_closed_union\t%s\t%s=%s\n' "$locator" "$union_name" "$member"
  done <"$member_file"
}

: >"$scratch/registry-closed-union"
while IFS=$'\t' read -r locator union_name; do
  emit_closed_union "$locator" "$union_name" >>"$scratch/registry-closed-union"
done <<'EOF'
src/core/agents/agent-message-decide.ts	TraceKind
src/core/board-log-handlers.ts	BoardLogRoute
src/core/context-link-render.ts	ContextLinkVerb
src/main/browser-guest-registry.ts	BrowserSurfaceKind
src/main/canvas-control-core.ts	ControlVerb
src/renderer/lib/controlRouting.ts	ControlRoute
src/renderer/lib/download.ts	DownloadRoute
src/renderer/lib/githubIssueMove.ts	GitHubMoveIntentKind
src/renderer/lib/noteLink.ts	LinkKind
src/renderer/lib/sessionList.ts	SidebarGrouping
src/renderer/lib/sessionList.ts	StatusKind
src/renderer/lib/sessionList.ts	StatusGroup
src/renderer/lib/sfx.ts	SfxKind
src/shared/agents/agent-messaging.ts	AgentMessageVerb
src/shared/agents/model-gateway.ts	ModelGatewayCredentialKind
src/shared/project-capabilities.ts	ProjectCapability
src/shared/project-capability-consent.ts	CapabilityAnswer
src/shared/types.ts	WorkspaceMigrationKind
EOF

emit_closed_set() {
  locator=$1
  set_name=$2
  member_file="$scratch/closed-set-members"
  git -C "$source_repository" show "$snapshot:$locator" 2>/dev/null |
    awk -v needle="const $set_name" '
      index($0, needle) {
        active=1; print
        rest=$0; sub(/^[^=]*=/, "", rest)
        if (rest ~ /\[[^]]*\]/) exit
        next
      }
      active { print; if ($0 ~ /^[[:space:]]*\]/) exit }
    ' |
    grep -oE "'[^']+'" |
    tr -d "'" >"$member_file" || true
  [[ -s "$member_file" ]] || return 0
  while IFS= read -r member; do
    printf 'registry_closed_set\t%s\t%s=%s\n' "$locator" "$set_name" "$member"
  done <"$member_file"
}

: >"$scratch/registry-closed-set"
while IFS=$'\t' read -r locator set_name; do
  emit_closed_set "$locator" "$set_name" >>"$scratch/registry-closed-set"
done <<'EOF'
src/core/agents/node-identity-policy.ts	TOLERANT_CONTROL_VERBS
src/core/agents/node-identity-policy.ts	STRICT_CONTROL_VERBS
src/core/context-link-render.ts	CONTEXT_LINK_VERBS
src/main/canvas-control-core.ts	VERBS
src/renderer/lib/controlRouting.ts	STORE_ANSWERED_VERBS
src/shared/agents/agent-messaging.ts	AGENT_MESSAGE_VERBS
src/shared/agents/config.ts	AGENT_HOOK_TARGETS
src/shared/agents/config.ts	RESUMABLE_AGENTS
src/shared/agents/config.ts	SESSION_ID_CAPABLE
src/shared/agents/config.ts	UNCONDITIONAL_SESSION_ID_CAPABLE
src/shared/agents/config.ts	SUBAGENT_CAPABLE
src/shared/agents/config.ts	RECURRING_CAPABLE
src/shared/agents/config.ts	BRANCH_CAPABLE
src/shared/agents/config.ts	CONTEXT_LINK_CAPABLE
src/shared/agents/config.ts	USAGE_CAPABLE
src/shared/agents/config.ts	CHAT_CAPABLE
src/shared/agents/config.ts	TRANSFER_SOURCE_CAPABLE
src/shared/agents/config.ts	RENAME_CAPABLE
src/shared/agents/config.ts	TITLE_READ_CAPABLE
src/shared/agents/config.ts	SHARED_IDENTITY_CAPABLE
src/shared/agents/config.ts	CANVAS_CONTROL_CAPABLE
src/shared/agents/config.ts	PERMISSION_MODE_CAPABLE
src/shared/agents/config.ts	MODEL_SWITCH_CAPABLE
src/shared/agents/config.ts	SELF_REPORTS_COPY
src/shared/agents/pane-owner-predicate.ts	RUNNER_VERBS
src/shared/control-verbs.ts	DESTRUCTIVE_VERBS
src/shared/project-capabilities.ts	PROJECT_CAPABILITIES
src/renderer/lib/sessionList.ts	STATUS_GROUP_ORDER
src/renderer/lib/verifyPanel.ts	DEFAULT_LENSES
src/shared/usage-limits.ts	USAGE_PROVIDER_IDS
src/core/agents/hooks/index.ts	MANAGED_HOOK_INSTALLERS
src/core/agents/hooks/index.ts	MANAGED_HOOK_REMOVERS
EOF

# Closed object registries need field-aware extraction: taking every quoted token would silently
# promote labels, filenames and comments to capabilities. The emitted surface keeps the registry,
# slot and source-to-action relation visible so moving an id between menu/header (or changing the
# action it hides) changes the frozen denominator.
emit_object_field_set() {
  locator=$1
  set_name=$2
  field_name=$3
  member_file="$scratch/object-field-members"
  git -C "$source_repository" show "$snapshot:$locator" 2>/dev/null |
    awk -v needle="const $set_name" '
      index($0, needle) { active=1 }
      active { print; if ($0 ~ /^[[:space:]]*\]/) exit }
    ' |
    sed -nE "s/.*${field_name}:[[:space:]]*'([^']+)'.*/\1/p" >"$member_file" || true
  [[ -s "$member_file" ]] || return 0
  while IFS= read -r member; do
    printf 'registry_closed_set\t%s\t%s=%s\n' "$locator" "$set_name" "$member"
  done <"$member_file"
}

emit_visibility_relations() {
  locator=src/renderer/lib/ui-visibility.ts
  source_file="$scratch/visibility-source"
  git -C "$source_repository" show "$snapshot:$locator" 2>/dev/null >"$source_file" || return 0
  for set_spec in 'HIDEABLE_MENU_ITEMS 7' 'HIDEABLE_HEADER_BUTTONS 4'; do
    set -- $set_spec
    set_name=$1
    expected_members=$2
    actual_members=$(awk -v needle="const $set_name" 'index($0, needle) { active=1 } active { print; if ($0 ~ /^[[:space:]]*\]/) exit }' "$source_file" | sed -nE "s/.*id:[[:space:]]*'([^']+)'.*/\1/p" | wc -l | tr -d '[:space:]')
    [[ "$actual_members" == "$expected_members" ]] || die E_CENSUS_SELECTOR "$set_name member count changed"
  done
  while IFS=$'\t' read -r set_name slot member action; do
    section_file="$scratch/visibility-section"
    awk -v needle="const $set_name" 'index($0, needle) { active=1 } active { print; if ($0 ~ /^[[:space:]]*\]/) exit }' "$source_file" >"$section_file"
    count=$(sed -nE "s/.*id:[[:space:]]*'([^']+)'.*/\1/p" "$section_file" | awk -v wanted="$member" '$0 == wanted { n++ } END { print n+0 }')
    [[ "$count" == 1 ]] || die E_CENSUS_SELECTOR "$set_name must contain exactly one $member member"
    printf 'registry_closed_set\t%s\t%s=%s:%s->%s\n' "$locator" "$set_name" "$slot" "$member" "$action"
  done <<'EOF'
HIDEABLE_MENU_ITEMS	menu	group	group_selection
HIDEABLE_MENU_ITEMS	menu	remove-from-group	remove_from_group
HIDEABLE_MENU_ITEMS	menu	colors	colour
HIDEABLE_MENU_ITEMS	menu	duplicate	duplicate
HIDEABLE_MENU_ITEMS	menu	collapse	collapse_expand
HIDEABLE_MENU_ITEMS	menu	markdown-view	markdown_projection
HIDEABLE_MENU_ITEMS	menu	refresh-terminal	refresh_terminal
HIDEABLE_HEADER_BUTTONS	header	refresh	header_refresh
HIDEABLE_HEADER_BUTTONS	header	mic	voice_dictation
HIDEABLE_HEADER_BUTTONS	header	ai-name	generate_name
HIDEABLE_HEADER_BUTTONS	header	comments	comments
EOF
}

# Provider hook lists include comments and Grok object rows; enumerate only array string members
# and explicit event fields. A quoted matcher/comment is deliberately not an event candidate.
emit_hook_event_set() {
  locator=$1
  set_name=$2
  member_file="$scratch/hook-event-members"
  git -C "$source_repository" show "$snapshot:$locator" 2>/dev/null |
    awk -v needle="const $set_name" 'index($0, needle) { active=1 } active { print; if ($0 ~ /^[[:space:]]*\]/) exit }' |
    sed -nE -e "s/^[[:space:]]*'([^']+)'[,]?[[:space:]]*$/\1/p" -e "s/.*event:[[:space:]]*'([^']+)'.*/\1/p" |
    sort -u >"$member_file" || true
  [[ -s "$member_file" ]] || return 0
  while IFS= read -r member; do
    printf 'registry_closed_set\t%s\t%s=%s\n' "$locator" "$set_name" "$member"
  done <"$member_file"
}

emit_numeric_constant() {
  locator=$1
  constant_name=$2
  value=$(git -C "$source_repository" show "$snapshot:$locator" 2>/dev/null | sed -nE "s/^(export )?const ${constant_name}([[:space:]]*:[^=]+)?[[:space:]]*=[[:space:]]*(.*)$/\3/p" | sed -E 's/[[:space:]]*\/\/.*$//; s/[[:space:]]*$//' | head -n 1 || true)
  [[ -n "$value" ]] || return 0
  printf 'registry_closed_set\t%s\t%s=%s\n' "$locator" "$constant_name" "$value"
}

emit_interface_fields() {
  locator=$1
  interface_name=$2
  member_file="$scratch/interface-field-members"
  git -C "$source_repository" show "$snapshot:$locator" 2>/dev/null |
    sed -n "/^export interface ${interface_name} {/,/^}/p" |
    sed -nE 's/^  ([A-Za-z][A-Za-z0-9]*)(\?)?:.*/\1/p' >"$member_file" || true
  [[ -s "$member_file" ]] || return 0
  while IFS= read -r member; do
    printf 'registry_closed_set\t%s\t%s.%s\n' "$locator" "$interface_name" "$member"
  done <"$member_file"
}

: >"$scratch/registry-object-field"
emit_visibility_relations >>"$scratch/registry-object-field"
emit_object_field_set src/shared/speech.ts WHISPER_MODELS id >>"$scratch/registry-object-field"
emit_object_field_set src/renderer/lib/addMenuSpec.tsx CONTENT_ADD_ITEMS kind >>"$scratch/registry-object-field"
while IFS=$'\t' read -r locator interface_name; do
  emit_interface_fields "$locator" "$interface_name" >>"$scratch/registry-object-field"
done <<'EOF'
src/shared/types.ts	PendingLaunch
src/renderer/lib/pendingLaunch.ts	ArmedNode
src/renderer/lib/pendingLaunch.ts	LaunchToFire
src/renderer/lib/pendingLaunch.ts	DependencyEdge
src/core/transcript-index-core.ts	TranscriptIndexEntry
src/core/transcript-index-core.ts	ScanFile
src/core/transcript-index-core.ts	RefreshPlan
src/shared/types.ts	TranscriptHit
src/core/pty-reap.ts	ReapCandidate
src/renderer/terminal/offscreen-policy.ts	OffscreenPlan
src/renderer/terminal/park-budget.ts	ParkedEntryState
EOF
if git -C "$source_repository" show "$snapshot:src/main/canvas-control-core.ts" 2>/dev/null | grep -F -- '--after <id,id>' >/dev/null; then
  printf 'registry_closed_set\tsrc/main/canvas-control-core.ts\tCANVAS_DEPENDENCY_OPTION=--after\n' >>"$scratch/registry-object-field"
fi

: >"$scratch/registry-hook-event"
while IFS=$'\t' read -r locator set_name; do
  emit_hook_event_set "$locator" "$set_name" >>"$scratch/registry-hook-event"
done <<'EOF'
src/shared/agents/hook-events.ts	CLAUDE_HOOK_EVENTS
src/shared/agents/hook-events.ts	GEMINI_HOOK_EVENTS
src/shared/agents/hook-events.ts	COPILOT_HOOK_EVENTS
src/shared/agents/hook-events.ts	GROK_HOOK_EVENTS
src/core/agents/hooks/codex.ts	CODEX_EVENTS
EOF

: >"$scratch/registry-numeric-constant"
while IFS=$'\t' read -r locator constant_name; do
  emit_numeric_constant "$locator" "$constant_name" >>"$scratch/registry-numeric-constant"
done <<'EOF'
src/core/pty-devices.ts	PTY_DEVICE_HEADROOM
src/core/pty-pressure.ts	PTY_PRESSURE_ELEVATED_RATIO
src/core/pty-pressure.ts	PTY_PRESSURE_INTERVAL_MS
src/core/pty-pressure.ts	PTY_PRESSURE_RE_ANNOUNCE_MS
src/main/ptmx-limit.ts	PTMX_MIN_TARGET
src/renderer/terminal/offscreen-policy.ts	OFFSCREEN_DISPOSE_MS_DEFAULT
src/renderer/terminal/offscreen-policy.ts	OFFSCREEN_DEFER_RETRY_MS
src/renderer/terminal/park-budget.ts	PARK_MAX
src/renderer/terminal/park-budget.ts	PARK_RECHECK_MS
src/core/pty-reap.ts	REAP_IDLE_MS
src/core/pty-reap.ts	REAP_SWEEP_MS
src/core/session-budget.ts	NOMINAL_SESSION_MB
src/core/session-budget.ts	HOST_SHARE
src/core/session-budget.ts	MAX_DETACHED_FLOOR
src/core/session-budget.ts	MAX_DETACHED_CEILING
src/core/session-budget.ts	SESSION_SWEEP_INTERVAL_MS
src/core/transcript-index-core.ts	INDEX_TEXT_CAP_BYTES
src/core/transcript-index.ts	TRANSCRIPT_INDEX_REFRESH_MS
src/core/transcript-index.ts	READ_CAP_BYTES
EOF

cat "$scratch/modules" "$scratch/registry-ipc" "$scratch/registry-node-kind" \
  "$scratch/registry-agent-adapter" "$scratch/registry-settings-section" \
  "$scratch/registry-command" "$scratch/registry-command-family" "$scratch/registry-shortcut" \
  "$scratch/registry-setting-key" \
  "$scratch/registry-closed-union" "$scratch/registry-closed-set" \
  "$scratch/registry-object-field" "$scratch/registry-hook-event" \
  "$scratch/registry-numeric-constant" |
  sort >"$scratch/expected-candidates"

actual_module_count=$(wc -l <"$scratch/modules" | tr -d '[:space:]')
actual_registry_count=$(cat "$scratch/registry-ipc" "$scratch/registry-node-kind" \
  "$scratch/registry-agent-adapter" "$scratch/registry-settings-section" \
  "$scratch/registry-command" "$scratch/registry-command-family" "$scratch/registry-shortcut" \
  "$scratch/registry-setting-key" \
  "$scratch/registry-closed-union" "$scratch/registry-closed-set" \
  "$scratch/registry-object-field" "$scratch/registry-hook-event" \
  "$scratch/registry-numeric-constant" |
  wc -l | tr -d '[:space:]')
actual_candidate_count=$(wc -l <"$scratch/expected-candidates" | tr -d '[:space:]')
[[ "$actual_module_count" == "$expected_module_count" ]] ||
  die E_CENSUS_SOURCE_SET "expected $expected_module_count modules, enumerated $actual_module_count"
[[ "$actual_registry_count" == "$expected_registry_count" ]] ||
  die E_CENSUS_SOURCE_SET "expected $expected_registry_count registry surfaces, enumerated $actual_registry_count"
[[ "$actual_candidate_count" == "$expected_candidate_count" ]] ||
  die E_CENSUS_SOURCE_SET "expected $expected_candidate_count candidates, enumerated $actual_candidate_count"

awk -F '\t' '$1 ~ /^CAP-[0-9][0-9][0-9]$/ { print $1 "\t" $4 "\t" $8 "\t" $2 }' "$coverage" |
  sort >"$scratch/ledger-features"

awk -F '\t' -v expected="$expected_candidate_count" -v errors="$scratch/census-errors" '
  function fail(code, detail) { print code "\t" detail > errors; failed=1; exit 1 }
  BEGIN { OFS="\t"; sequence=0 }
  /^#/ { next }
  $1 == "candidate_id" {
    if ($0 != "candidate_id\tcandidate_kind\tsource_locator\tsurface_key\tsource_blob_sha256") {
      fail("E_CENSUS_SCHEMA", "unexpected census header")
    }
    header++
    next
  }
  {
    if (NF != 5) fail("E_CENSUS_SCHEMA", "row has " NF " fields")
    sequence++
    wanted=sprintf("CEN-%04d", sequence)
    if ($1 != wanted) fail("E_CENSUS_SEQUENCE", "expected " wanted ", found " $1)
    if ($2 !~ /^(module|registry_ipc|registry_node_kind|registry_agent_adapter|registry_settings_section|registry_setting_key|registry_command|registry_command_family|registry_shortcut|registry_closed_union|registry_closed_set)$/) {
      fail("E_CENSUS_KIND", $1 " has unknown kind " $2)
    }
    if ($3 == "" || $3 ~ /^\// || $3 ~ /(^|\/)\.\.($|\/)/) {
      fail("E_CENSUS_LOCATOR", $1 " has unsafe source locator")
    }
    if (($2 == "module" && $4 != "-") || ($2 != "module" && ($4 == "" || length($4) > 160))) {
      fail("E_CENSUS_SURFACE", $1 " has invalid surface key")
    }
    if ($5 !~ /^[0-9a-f]+$/ || length($5) != 64) {
      fail("E_CENSUS_DIGEST", $1 " has malformed blob digest")
    }
    key=$2 "\t" $3 "\t" $4
    if (seen[key]++) fail("E_CENSUS_DUPLICATE", "duplicate candidate " key)
    print key
    print $1 "\t" $2 "\t" $3 "\t" $4 > records
    print $3 "\t" $5 > digests
  }
  END {
    if (failed) exit 1
    if (!header || header != 1) fail("E_CENSUS_SCHEMA", "missing or repeated header")
    if (sequence != expected) fail("E_CENSUS_COUNT", "expected " expected ", found " sequence)
  }
' records="$scratch/census-records" digests="$scratch/census-digests" "$census" \
  >"$scratch/census-candidates" || {
    if [[ -s "$scratch/census-errors" ]]; then
      IFS=$'\t' read -r code detail <"$scratch/census-errors"
      die "$code" "$detail"
    fi
    die E_CENSUS_PARSE "census schema or row is invalid"
  }

sort "$scratch/census-candidates" -o "$scratch/census-candidates"
diff -u "$scratch/expected-candidates" "$scratch/census-candidates" >/dev/null ||
  die E_CENSUS_SOURCE_SET "enumerated source candidates differ from the frozen census"

awk -F '\t' -v expected="$expected_mapping_count" -v errors="$scratch/mapping-errors" '
  function fail(code, detail) { print code "\t" detail > errors; failed=1; exit 1 }
  BEGIN { OFS="\t"; sequence=0 }
  /^#/ { next }
  $1 == "mapping_id" {
    if ($0 != "mapping_id\tcandidate_id\tresolution\tfeature_id\tfeature_disposition\tmapping_basis\tcandidate_rationale") {
      fail("E_MAPPING_SCHEMA", "unexpected mapping header")
    }
    header++
    next
  }
  {
    if (NF != 7) fail("E_MAPPING_SCHEMA", "row has " NF " fields")
    sequence++
    wanted=sprintf("MAP-%04d", sequence)
    if ($1 != wanted) fail("E_MAPPING_SEQUENCE", "expected " wanted ", found " $1)
    if ($2 !~ /^CEN-[0-9][0-9][0-9][0-9]$/) fail("E_MAPPING_CANDIDATE", $1 " has malformed candidate id")
    if ($3 !~ /^(mapped|supporting)$/) fail("E_MAPPING_RESOLUTION", $1 " has unknown resolution")
    if ($4 !~ /^CAP-[0-9][0-9][0-9]$/ || $5 !~ /^(adopted|adapted|rejected|irrelevant)$/) {
      fail("E_MAPPING_FEATURE", $1 " has malformed feature mapping")
    }
    if ($6 !~ /^(ledger_evidence|closed_registry|manual_source_audit|supporting_module_audit)$/) {
      fail("E_MAPPING_BASIS", $1 " has unknown mapping basis")
    }
    if ($7 == "" || length($7) > 512 || index($7, $4) == 0) {
      fail("E_MAPPING_RATIONALE", $1 " has no bounded feature-specific rationale")
    }
    if ($3 == "supporting" && $7 !~ /no independent capability/) {
      fail("E_MAPPING_RATIONALE", $1 " does not explain why it adds no independent capability")
    }
    key=$2 "\t" $4
    if (seen[key]++) fail("E_MAPPING_DUPLICATE", "duplicate candidate-feature mapping " key)
    print $2 "\t" $3 "\t" $4 "\t" $5 "\t" $6 "\t" $7
  }
  END {
    if (failed) exit 1
    if (!header || header != 1) fail("E_MAPPING_SCHEMA", "missing or repeated header")
    if (sequence != expected) fail("E_MAPPING_COUNT", "expected " expected ", found " sequence)
  }
' "$mapping" >"$scratch/candidate-mappings" || {
  if [[ -s "$scratch/mapping-errors" ]]; then
    IFS=$'\t' read -r code detail <"$scratch/mapping-errors"
    die "$code" "$detail"
  fi
  die E_MAPPING_PARSE "mapping schema or row is invalid"
}

awk -F '\t' '
  NR == FNR { kind[$1]=$2; locator[$1]=$3; surface[$1]=$4; next }
  {
    if (!($1 in kind)) { print "E_MAPPING_CANDIDATE\tunknown candidate " $1; exit 1 }
    covered[$1]++
    if ($2 == "supporting" && kind[$1] != "module") {
      print "E_MAPPING_RESOLUTION\t" $1 " uses supporting outside a module candidate"; exit 1
    }
    if (kind[$1] != "module" && $2 != "mapped") {
      print "E_MAPPING_RESOLUTION\t" $1 " leaves a closed registry surface unmapped"; exit 1
    }
    if (($5 == "ledger_evidence" || $5 == "manual_source_audit") &&
        !(kind[$1] == "module" && $2 == "mapped")) {
      print "E_MAPPING_BASIS\t" $1 " has an invalid module-primary basis"; exit 1
    }
    if ($5 == "supporting_module_audit" && !(kind[$1] == "module" && $2 == "supporting")) {
      print "E_MAPPING_BASIS\t" $1 " has an invalid supporting basis"; exit 1
    }
    if ($5 == "closed_registry" && !(kind[$1] != "module" && $2 == "mapped")) {
      print "E_MAPPING_BASIS\t" $1 " has an invalid registry basis"; exit 1
    }
    anchor=(kind[$1] == "module" ? locator[$1] : surface[$1])
    if (index($6, anchor) == 0) {
      print "E_MAPPING_RATIONALE\t" $1 " rationale omits its exact source anchor"; exit 1
    }
  }
  END {
    if (NR == FNR) exit
    for (id in kind) if (!covered[id]) { print "E_MAPPING_COVERAGE\t" id " has no mapping"; exit 1 }
  }
' "$scratch/census-records" "$scratch/candidate-mappings" >"$scratch/candidate-mapping-result" || true
if [[ -s "$scratch/candidate-mapping-result" ]]; then
  IFS=$'\t' read -r code detail <"$scratch/candidate-mapping-result"
  die "$code" "$detail"
fi

awk -F '\t' '
  NR == FNR { disposition[$1]=$2; feature_key[$1]=$4; next }
  {
    if (!($3 in disposition)) { print "E_MAPPING_FEATURE\tunknown feature " $3; exit 1 }
    if ($4 != disposition[$3]) { print "E_MAPPING_DISPOSITION\t" $3 " disposition differs"; exit 1 }
    if (index($6, feature_key[$3]) == 0) {
      print "E_MAPPING_RATIONALE\t" $1 " rationale omits capability key " feature_key[$3]; exit 1
    }
    if ($2 == "mapped") mapped[$3]++
  }
  END {
    if (NR == FNR) exit
    for (id in disposition) if (!mapped[id]) { print "E_MAPPING_COVERAGE\t" id " has no primary candidate"; exit 1 }
  }
' "$scratch/ledger-features" "$scratch/candidate-mappings" >"$scratch/mapping-feature-result" || true
if [[ -s "$scratch/mapping-feature-result" ]]; then
  IFS=$'\t' read -r code detail <"$scratch/mapping-feature-result"
  die "$code" "$detail"
fi

awk -F '\t' '$2 == "module" { print $3 "\t" $1 }' "$scratch/census-records" >"$scratch/module-locators"
awk -F '\t' '$2 == "mapped" && $5 == "ledger_evidence" { print $1 "\t" $3 }' "$scratch/candidate-mappings" >"$scratch/primary-mappings"
awk -F '\t' '
  FILENAME == ARGV[1] { candidate[$1]=$2; next }
  FILENAME == ARGV[2] { primary[$1 SUBSEP $2]++; next }
  {
    candidate_id=candidate[$3]
    if (candidate_id == "") { print "E_MAPPING_EVIDENCE\t" $1 " evidence locator is not a selected module"; exit 1 }
    if (!primary[candidate_id SUBSEP $1]) {
      print "E_MAPPING_EVIDENCE\t" candidate_id " lacks primary " $1 " mapping for its ledger evidence"; exit 1
    }
  }
' "$scratch/module-locators" "$scratch/primary-mappings" "$scratch/ledger-features" >"$scratch/evidence-mapping-result" || true
if [[ -s "$scratch/evidence-mapping-result" ]]; then
  IFS=$'\t' read -r code detail <"$scratch/evidence-mapping-result"
  die "$code" "$detail"
fi

sort -u "$scratch/census-digests" >"$scratch/census-unique-digests"
while IFS=$'\t' read -r locator expected_digest; do
  [[ "$(git -C "$source_repository" cat-file -t "$snapshot:$locator" 2>/dev/null || true)" == blob ]] ||
    die E_CENSUS_LOCATOR "$locator is absent or is not a blob"
  actual_digest=$(git -C "$source_repository" show "$snapshot:$locator" | hash_stream)
  [[ "$actual_digest" == "$expected_digest" ]] ||
    die E_CENSUS_DIGEST "$locator digest differs"
done <"$scratch/census-unique-digests"

echo "product-capability-source-acceptance: snapshot $snapshot, tree $actual_tree, $checked evidence references, $actual_candidate_count candidates and $expected_mapping_count audited mappings verified"
