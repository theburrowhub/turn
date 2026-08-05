//! Secret hygiene: what must never reach the database file.
//!
//! Turn launches processes with the user's environment, which on a developer
//! machine reliably contains `GITHUB_TOKEN`, `ANTHROPIC_API_KEY`, session
//! cookies and cloud credentials. A store that survives restarts also survives
//! being copied into a bug report, synced to a backup, or read by anything else
//! running as the user — so none of that may be written down.
//!
//! The first rule is narrow and mechanical: **the value of any key that looks
//! like a credential is replaced before the row is built.** The key itself is
//! kept, because "GITHUB_TOKEN was set" is exactly what Turn needs in order to
//! explain why an agent could not authenticate after a restore, while its value is
//! only a liability.
//!
//! Matching on keys is deliberately greedy — substring, case-insensitive.
//! Redacting a variable called `MONKEY_MODE` because it contains `KEY` costs the
//! user nothing; missing one called `deploy_key` costs them a repository.
//!
//! ## Why keys are not enough
//!
//! A key-name rule only sees credentials that arrive *labelled*. The most common
//! way one reaches Turn is not labelled at all: it is inside a value under an
//! innocuous name. An agent asks permission to run
//! `curl -H "Authorization: Bearer sk-ant-…"`, and the payload key is `command`.
//! A user pastes a token into a prompt, and the key is `prompt`. Both of those end
//! up in the event log.
//!
//! So there is a second rule: **anything shaped like a known credential is
//! replaced wherever it appears**, by [`redact_secrets`]. It matches on the
//! issuer-assigned prefixes that make a token recognisable — `ghp_`, `sk-ant-`,
//! `AKIA`, a PEM private-key block — rather than on entropy, because a
//! high-entropy-looking-string rule would eat commit hashes, paths and UUIDs and
//! make the log useless. It will therefore miss a credential with no distinctive
//! shape; it is a net under the key rule, not a replacement for it.

use std::collections::HashMap;
use turn_core::attention::AttentionPolicy;
use turn_core::event::AgentRef;
use turn_core::model::layout::{Layout, LayoutNode};
use turn_core::model::node::{AgentInfo, PendingPermission, ProcessNode};
use turn_core::model::{
    ActivityPreview, AgentName, Session, SessionTree, Template, Workspace, WorkspaceCheckout,
};
use turn_core::state::{Lifecycle, Turn};

/// Written in place of a secret value.
pub const REDACTED: &str = "[redacted]";

/// Fragments that mark a key as holding a credential.
const SENSITIVE_FRAGMENTS: [&str; 7] = [
    "TOKEN",
    "KEY",
    "SECRET",
    "PASSWORD",
    "CREDENTIAL",
    "COOKIE",
    "AUTH",
];

/// Whether a variable name looks like it holds a credential.
pub fn is_sensitive_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    SENSITIVE_FRAGMENTS
        .iter()
        .any(|fragment| upper.contains(fragment))
}

/// Credential shapes, as `(prefix, smallest length that is plausibly real)`.
///
/// Every entry is a prefix an issuer actually assigns, which is what makes this
/// safe to run over free text: `ghp_` followed by twenty more characters is a
/// GitHub token and nothing else, whereas "looks random" would match a commit
/// hash. The minimum lengths keep prose like "sk-" out of it.
const SECRET_SHAPES: &[(&str, usize)] = &[
    // Anthropic, OpenAI and compatible.
    ("sk-ant-", 30),
    ("sk-proj-", 30),
    ("sk-", 24),
    // GitHub: personal, OAuth, user-to-server, server-to-server, refresh, fine-grained.
    ("ghp_", 36),
    ("gho_", 36),
    ("ghu_", 36),
    ("ghs_", 36),
    ("ghr_", 36),
    ("github_pat_", 40),
    // GitLab.
    ("glpat-", 26),
    // Slack.
    ("xoxb-", 24),
    ("xoxp-", 24),
    ("xoxa-", 24),
    ("xoxr-", 24),
    ("xapp-", 24),
    // AWS access key ids, which are always twenty characters.
    ("AKIA", 20),
    ("ASIA", 20),
    // Google.
    ("AIza", 35),
    // Others common on a developer machine.
    ("npm_", 30),
    ("dop_v1_", 40),
    ("hf_", 30),
    ("shpat_", 30),
    ("figd_", 30),
    ("sbp_", 30),
];

/// Characters that can be part of a credential. Everything else is a boundary.
fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+' | '/' | '=' | '~')
}

/// How far a credential starting at `text[0]` extends, or `None` if there is none.
///
/// The match has to be anchored on the issuer's prefix rather than on a whole
/// word, because a credential is usually welded to something else by the time it
/// reaches Turn: `SLACK=xoxb-…`, `https://npm_…@registry`, `Bearer sk-ant-…`. It
/// then runs to the end of the token characters, so a base64 body with `/`, `+`
/// and `=` in it is taken whole.
fn secret_length_at(text: &str) -> Option<usize> {
    let run_end = text.find(|c: char| !is_token_char(c)).unwrap_or(text.len());
    let run = &text[..run_end];

    for (prefix, minimum) in SECRET_SHAPES {
        if run.starts_with(prefix) && run.len() >= *minimum {
            return Some(run.len());
        }
    }
    is_jwt(run).then_some(run.len())
}

/// A JSON Web Token: three base64url segments, the first of which is a JSON
/// object, so it always begins `eyJ`.
///
/// Recognised on shape rather than by prefix rule because a JWT is a bearer
/// credential wherever it turns up, and it turns up in `Authorization` headers
/// echoed into payloads.
fn is_jwt(candidate: &str) -> bool {
    if !candidate.starts_with("eyJ") || candidate.len() < 40 {
        return false;
    }
    let segments: Vec<&str> = candidate.split('.').collect();
    segments.len() == 3
        && segments.iter().all(|segment| segment.len() >= 8)
        && segments
            .iter()
            .all(|segment| segment.chars().all(|c| is_token_char(c) && c != '.'))
}

/// Replaces anything shaped like a credential, wherever it sits in a string.
///
/// Applied to free text and to every JSON string value, because the key beside a
/// secret is very often innocent: a token pasted into a prompt, or passed with
/// `-H "Authorization: Bearer …"` inside a command an agent asks to run.
///
/// PEM blocks are handled as blocks rather than as tokens: a private key is many
/// lines of base64 that individually look like nothing, and half a key in a log is
/// still a leak.
pub fn redact_secrets(text: &str) -> String {
    let without_keys = redact_pem_blocks(text);
    let source = without_keys.as_str();

    let mut out = String::with_capacity(source.len());
    let mut previous: Option<char> = None;
    let mut index = 0usize;
    while index < source.len() {
        let rest = &source[index..];
        let current = rest.chars().next().expect("index is a char boundary");

        // A prefix only counts at the start of a word. Otherwise `ask-a-question`
        // contains `sk-`, and the scanner would start eating prose.
        let at_word_start = previous.is_none_or(|c| !c.is_ascii_alphanumeric());
        if at_word_start {
            if let Some(length) = secret_length_at(rest) {
                out.push_str(REDACTED);
                index += length;
                previous = Some(' ');
                continue;
            }
        }

        out.push(current);
        index += current.len_utf8();
        previous = Some(current);
    }
    out
}

const PEM_BEGIN: &str = "-----BEGIN";
const PEM_END: &str = "-----END";
const PEM_DASHES: &str = "-----";

/// Shortest line an *unterminated* block takes as key material on shape alone.
///
/// PEM wraps a body at 64 characters, so every line of a real key but its last
/// clears this comfortably, while the single words prose is made of ("header",
/// "example", "placeholder") do not. A block that closes properly needs no such
/// guess and does not apply this.
const MIN_BODY_LINE: usize = 16;

/// Replaces `-----BEGIN … PRIVATE KEY-----` … `-----END … -----` with a marker.
fn redact_pem_blocks(text: &str) -> String {
    if !text.contains(PEM_BEGIN) {
        return text.to_string();
    }

    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(PEM_BEGIN) {
        out.push_str(&rest[..start]);
        let block = &rest[start..];
        // Only private material is worth hiding; a certificate is public by
        // definition and useful when debugging a TLS complaint.
        if !names_private_material(block) {
            out.push_str(PEM_BEGIN);
            rest = &block[PEM_BEGIN.len()..];
            continue;
        }
        out.push_str(REDACTED);
        rest = after_private_block(block);
    }
    out.push_str(rest);
    out
}

/// The label of a `-----BEGIN` header, and the offset in `block` where whatever
/// the header introduces begins.
///
/// The label search starts *after* the opening marker because that marker is
/// itself five dashes: looking for `-----` from the start of the block matches at
/// offset zero, yields an empty label, and so reads every key in the world as
/// public. Only the exact `-----\n` spelling escaped that, which left a key with
/// CRLF endings, no trailing newline, or one welded into a single line of JSON
/// going to disk verbatim.
///
/// The label ends at whichever comes *first*, the closing dashes or a line break;
/// taking the dashes in preference is what let a header that never closes borrow
/// the words "private key" from a `-----` further down the payload.
fn pem_header(block: &str) -> (&str, usize) {
    let after_marker = &block[PEM_BEGIN.len()..];
    let dashes = after_marker.find(PEM_DASHES);
    let line_break = next_line_break(after_marker).map(|(at, _)| at);
    // The well-formed case: `-----BEGIN RSA PRIVATE KEY-----`, closing on its own
    // line, with the body on the far side of the closing dashes.
    let closing_dashes = dashes.filter(|offset| line_break.is_none_or(|at| *offset < at));
    if let Some(offset) = closing_dashes {
        return (
            &after_marker[..offset],
            PEM_BEGIN.len() + offset + PEM_DASHES.len(),
        );
    }
    // A header cut short by a line break is malformed, but a truncated payload
    // can still deliver one with a body underneath it.
    match line_break {
        Some(at) => (&after_marker[..at], PEM_BEGIN.len() + at),
        None => (after_marker, block.len()),
    }
}

/// Whether the label of a `-----BEGIN` header says the block is a private key.
///
/// Only private material is worth hiding, and the label is the only thing that
/// says so. [`pem_header`] bounds it at the first line break, so a header that
/// never closes cannot drag the words "private key" in from unrelated text
/// further down the payload.
fn names_private_material(block: &str) -> bool {
    pem_header(block)
        .0
        .to_ascii_uppercase()
        .contains("PRIVATE KEY")
}

/// Whatever follows a private block: its `-----END …-----` when it has one, and
/// otherwise the end of the key body rather than the end of the payload.
///
/// A block that never closes used to take *everything* after it, on the grounds
/// that half a key is still a leak. That is true of a truncated key and false of
/// the far commoner input: text that merely mentions a header — a prompt about
/// key handling, a diff of documentation, an error message quoting a malformed
/// key. Those arrive in `TurnEvent::raw` and in permission summaries, and a
/// mention silently swallowing the rest of the payload loses the evidence the
/// event log exists to keep, in exchange for redacting a key that was not there.
///
/// So an unterminated block consumes only what can plausibly be key material: a
/// PEM body is base64 wrapped into lines, so the run of base64 lines under the
/// header is taken and the first line that reads as prose ends the block. A block
/// that does close is unaffected — [`closed_block_length`] still takes it whole.
///
/// Where it is unsure, it errs toward redacting a line and toward keeping the
/// payload: a long base64-looking line right under the header goes even when it
/// is really a hash out of a diff, while a truncated body whose only remaining
/// line is too short to tell from a single word of prose leaves that line behind.
/// Losing the tail of a key that had no `-----END` to protect it is the cheaper
/// mistake — the header is replaced either way, and what stays is a fragment far
/// too short to authenticate with, whereas the alternative deletes evidence the
/// event log was written to keep.
fn after_private_block(block: &str) -> &str {
    let (_, body_start) = pem_header(block);
    let body = &block[body_start..];
    &body[private_body_length(body)..]
}

/// How much of the text under a private header belongs to the key.
fn private_body_length(body: &str) -> usize {
    closed_block_length(body).unwrap_or_else(|| unterminated_body_length(body))
}

/// Where the block's `-----END …-----` leaves off, or `None` when no closing
/// marker is reachable from the header across key-shaped lines alone.
///
/// Reachability is what keeps a passing mention from claiming the `-----END` of an
/// unrelated block further down and taking everything in between: prose between
/// the two disqualifies it. Once the marker *is* reachable this is the whole
/// block, so no minimum line length applies — a short final line of base64
/// belongs to the key just as much as a full one.
fn closed_block_length(body: &str) -> Option<usize> {
    for (line, line_end) in pem_lines(body) {
        // The closing marker can share its line with the body, because a payload
        // squashed onto one line has no line breaks left to put it on its own.
        if let Some(offset) = line.find(PEM_END) {
            let line_start = line_end - line.len();
            let after_marker = &line[offset + PEM_END.len()..];
            return Some(match after_marker.find(PEM_DASHES) {
                Some(tail) => line_start + offset + PEM_END.len() + tail + PEM_DASHES.len(),
                None => line_end,
            });
        }
        let trimmed = line.trim();
        if !(trimmed.is_empty() || is_armour_header(trimmed) || is_base64_line(trimmed)) {
            return None;
        }
    }
    None
}

/// How far the key body under an unclosed header runs.
///
/// Only lines that look like key material count, and the first line that reads as
/// prose ends the block, so a payload that merely mentions a header keeps
/// everything it goes on to say.
fn unterminated_body_length(body: &str) -> usize {
    // The end of the last line that is key material. Blank and armour lines are
    // only swallowed when a body line follows them.
    let mut committed = 0usize;
    let mut taken_a_body_line = false;

    for (line, line_end) in pem_lines(body) {
        let trimmed = line.trim();
        if trimmed.is_empty() || is_armour_header(trimmed) {
            continue;
        }
        // Past the first body line the block is plainly a key, so a short line is
        // taken too: that is how a body ends.
        if is_base64_line(trimmed) && (taken_a_body_line || trimmed.len() >= MIN_BODY_LINE) {
            taken_a_body_line = true;
            committed = line_end;
            continue;
        }
        break;
    }
    committed
}

/// The lines of `text`, each with its end offset in `text`.
fn pem_lines(text: &str) -> impl Iterator<Item = (&str, usize)> {
    let mut cursor = Some(0usize);
    std::iter::from_fn(move || {
        let start = cursor?;
        match next_line_break(&text[start..]) {
            Some((at, width)) => {
                let end = start + at;
                cursor = Some(end + width);
                Some((&text[start..end], end))
            }
            None => {
                cursor = None;
                Some((&text[start..], text.len()))
            }
        }
    })
}

/// The next line break and its width in bytes, counting the JSON spelling.
///
/// A payload welded into one line of JSON carries its newlines as the two
/// characters `\` and `n`, so a scanner that only knows `'\n'` reads a key body
/// and the prose underneath it as a single line — and then classifies neither.
fn next_line_break(text: &str) -> Option<(usize, usize)> {
    let mut index = 0usize;
    while index < text.len() {
        let rest = &text[index..];
        let current = rest.chars().next()?;
        match current {
            '\n' | '\r' => return Some((index, current.len_utf8())),
            '\\' if matches!(rest[1..].chars().next(), Some('n') | Some('r')) => {
                return Some((index, 2));
            }
            _ => index += current.len_utf8(),
        }
    }
    None
}

/// Whether a line is only base64, and so could be part of a key body.
fn is_base64_line(line: &str) -> bool {
    !line.is_empty()
        && line
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '='))
}

/// Whether a line is a PEM armour header, as an encrypted key carries above its
/// body: `Proc-Type: 4,ENCRYPTED`, `DEK-Info: AES-128-CBC,…`.
fn is_armour_header(line: &str) -> bool {
    let Some((name, _)) = line.split_once(':') else {
        return false;
    };
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// The value to store for one variable.
///
/// A sensitive name loses its value outright. An innocent name keeps its value,
/// scanned — `NODE_OPTIONS`, `npm_config_registry` and a dozen other ordinary
/// variables routinely have a token embedded in them.
fn redact_value(key: &str, value: &str) -> String {
    if is_sensitive_key(key) {
        REDACTED.to_string()
    } else {
        redact_secrets(value)
    }
}

/// Redacts an ordered environment list, preserving order and keys.
pub fn redact_pairs(env: &[(String, String)]) -> Vec<(String, String)> {
    env.iter()
        .map(|(key, value)| (key.clone(), redact_value(key, value)))
        .collect()
}

/// Redacts a keyed environment map.
pub fn redact_map(env: &HashMap<String, String>) -> HashMap<String, String> {
    env.iter()
        .map(|(key, value)| (key.clone(), redact_value(key, value)))
        .collect()
}

fn redact_optional(text: &Option<String>) -> Option<String> {
    text.as_deref().map(redact_secrets)
}

fn redact_strings(values: &[String]) -> Vec<String> {
    values.iter().map(|value| redact_secrets(value)).collect()
}

/// Returns the Workspace document that is allowed to cross the durable boundary.
///
/// Filesystem identity is validated and written separately by `WorkspaceRepo`; every
/// other user- or tool-supplied string is scanned here. Identifiers remain byte-for-byte
/// stable so redaction can never break foreign keys.
pub(crate) fn workspace_for_persistence(workspace: &Workspace) -> Workspace {
    let mut safe = workspace.clone();
    safe.name = redact_secrets(&safe.name);
    safe.git_remote = redact_optional(&safe.git_remote);
    safe.env = redact_pairs(&safe.env);
    safe.default_shell = redact_optional(&safe.default_shell);
    safe.default_agent = redact_optional(&safe.default_agent);
    safe.init_commands = redact_strings(&safe.init_commands);
    safe.attention = attention_policy_for_persistence(&safe.attention);
    safe.colour = redact_optional(&safe.colour);
    safe.icon = redact_optional(&safe.icon);
    safe
}

/// Returns the Session document that is allowed to cross the durable boundary.
pub(crate) fn session_for_persistence(session: &Session) -> Session {
    let mut safe = session.clone();
    safe.name = redact_secrets(&safe.name);
    safe.note = redact_optional(&safe.note);
    safe.cwd = redact_secrets(&safe.cwd);
    safe.worktree_path = redact_optional(&safe.worktree_path);
    safe.env = redact_pairs(&safe.env);
    safe.layout = redact_layout(&safe.layout);
    safe.attention = attention_policy_for_persistence(&safe.attention);
    safe.tags = redact_strings(&safe.tags);
    safe.git_branch = redact_optional(&safe.git_branch);
    safe.linked_ref = redact_optional(&safe.linked_ref);
    let mut tree = SessionTree::new();
    for node in safe.tree.iter() {
        tree.insert(node_for_persistence(node));
    }
    safe.tree = tree;
    safe
}

/// Returns the Template document that is allowed to cross the durable boundary.
pub(crate) fn template_for_persistence(template: &Template) -> Template {
    let mut safe = template.clone();
    safe.name = redact_secrets(&safe.name);
    safe.description = redact_optional(&safe.description);
    safe.icon = redact_optional(&safe.icon);
    safe.layout = redact_layout(&safe.layout);
    safe.attention = safe
        .attention
        .as_ref()
        .map(attention_policy_for_persistence);
    safe.init_commands = redact_strings(&safe.init_commands);
    safe.name_pattern = redact_optional(&safe.name_pattern);
    safe.hotkey = redact_optional(&safe.hotkey);
    safe.env = redact_pairs(&safe.env);
    safe
}

/// Returns checkout metadata with labels redacted while preserving filesystem
/// identity. Checkout paths are structural fencing keys and are rejected by the
/// repository if they contain credential-shaped material rather than rewritten.
pub(crate) fn checkout_for_persistence(checkout: &WorkspaceCheckout) -> WorkspaceCheckout {
    let mut safe = checkout.clone();
    safe.branch = redact_optional(&safe.branch);
    safe.shared_resources = redact_strings(&safe.shared_resources);
    safe
}

/// Returns a ProcessNode whose structural identity is intact and whose durable free
/// text has been scanned for credentials.
pub(crate) fn node_for_persistence(node: &ProcessNode) -> ProcessNode {
    let mut safe = node.clone();
    safe.title = redact_secrets(&safe.title);
    safe.command = redact_secrets(&safe.command);
    safe.args = redact_strings(&safe.args);
    safe.cwd = redact_secrets(&safe.cwd);
    safe.lifecycle = lifecycle_for_persistence(&safe.lifecycle);
    safe.turn = safe.turn.as_ref().map(turn_for_persistence);
    safe.agent = safe.agent.as_ref().map(agent_info_for_persistence);
    safe.activity_preview = safe
        .activity_preview
        .as_ref()
        .map(activity_preview_for_persistence);
    safe.env_highlights = redact_map(&safe.env_highlights);
    safe
}

fn lifecycle_for_persistence(lifecycle: &Lifecycle) -> Lifecycle {
    match lifecycle {
        Lifecycle::Signaled { signal } => Lifecycle::Signaled {
            signal: redact_secrets(signal),
        },
        Lifecycle::Stopped { signal } => Lifecycle::Stopped {
            signal: redact_secrets(signal),
        },
        other => other.clone(),
    }
}

fn turn_for_persistence(turn: &Turn) -> Turn {
    match turn {
        Turn::Failed { reason } => Turn::Failed {
            reason: redact_secrets(reason),
        },
        other => other.clone(),
    }
}

fn attention_policy_for_persistence(policy: &AttentionPolicy) -> AttentionPolicy {
    let mut safe = policy.clone();
    safe.custom_command = redact_optional(&safe.custom_command);
    safe
}

pub(crate) fn activity_preview_for_persistence(preview: &ActivityPreview) -> ActivityPreview {
    let mut safe = preview.clone();
    let text = redact_secrets(&safe.normalized_text);
    if text != safe.normalized_text {
        safe.normalized_text = text;
        safe.contains_sensitive_data = true;
        safe.redacted = true;
    }
    safe
}

fn agent_ref_for_persistence(agent: &AgentRef) -> AgentRef {
    AgentRef {
        provider: redact_optional(&agent.provider),
        tool: redact_optional(&agent.tool),
        model: redact_optional(&agent.model),
        external_id: redact_optional(&agent.external_id),
    }
}

fn agent_name_for_persistence(name: &AgentName) -> AgentName {
    AgentName {
        declared_name: redact_optional(&name.declared_name),
        display_name: redact_secrets(&name.display_name),
        source: name.source,
        confidence: name.confidence,
        user_renamed: name.user_renamed,
    }
}

fn pending_permission_for_persistence(permission: &PendingPermission) -> PendingPermission {
    PendingPermission {
        summary: redact_secrets(&permission.summary),
        command: redact_optional(&permission.command),
        tool_name: redact_optional(&permission.tool_name),
        risk: permission.risk,
        requested_ms: permission.requested_ms,
        cwd: redact_optional(&permission.cwd),
    }
}

pub(crate) fn agent_info_for_persistence(agent: &AgentInfo) -> AgentInfo {
    AgentInfo {
        agent: agent_ref_for_persistence(&agent.agent),
        name: agent_name_for_persistence(&agent.name),
        external_id: redact_optional(&agent.external_id),
        agent_type: redact_optional(&agent.agent_type),
        current_task: redact_optional(&agent.current_task),
        last_message: redact_optional(&agent.last_message),
        pending_permission: agent
            .pending_permission
            .as_ref()
            .map(pending_permission_for_persistence),
        pending_question: redact_optional(&agent.pending_question),
        tokens_used: agent.tokens_used,
        cost_usd: agent.cost_usd,
        permission_mode: redact_optional(&agent.permission_mode),
        git_branch: redact_optional(&agent.git_branch),
        resumable: agent.resumable,
    }
}

/// Redacts every free-text field in every pane of a layout.
///
/// Layouts are stored as one JSON blob, so a token pasted into a pane's env
/// would otherwise ride into the file inside the tree — the one place it is easy
/// to forget to look.
pub fn redact_layout(layout: &Layout) -> Layout {
    let mut copy = layout.clone();
    scrub(&mut copy.root);
    copy
}

fn scrub(node: &mut LayoutNode) {
    match node {
        LayoutNode::Leaf(pane) => {
            pane.title = redact_optional(&pane.title);
            pane.command = redact_optional(&pane.command);
            pane.args = redact_strings(&pane.args);
            pane.cwd = redact_optional(&pane.cwd);
            pane.env = redact_pairs(&pane.env);
        }
        LayoutNode::Split(split) => {
            for child in split.children.iter_mut() {
                scrub(&mut child.node);
            }
        }
    }
}

/// Redacts sensitive members of a JSON document, at any depth.
///
/// Used on both halves of a stored event: the raw adapter payload, and the
/// serialised event kind — which is where a permission request keeps the command
/// it is asking about, and a turn start keeps an excerpt of what the user typed.
/// Both are places a credential arrives under an innocent key.
///
/// A document that is not JSON is scanned as text rather than passed through: the
/// key rule has nothing to work with there, but a token shaped like a token is
/// still a token.
pub fn redact_json(raw: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return redact_secrets(raw);
    };
    scrub_json(&mut value);
    serde_json::to_string(&value).unwrap_or_else(|_| redact_secrets(raw))
}

/// What to store in place of a scalar under a sensitive key, keeping its type.
///
/// The scrubbed document is read back into [`turn_core::event::EventKind`], so a
/// marker *string* written over a numeric or boolean field would make the row
/// permanently undecodable — and the key rule is a greedy substring match, so it
/// only takes an event field called `tokens_used` or `auth_required` for the whole
/// event to become unreadable. A zero and a `false` say just as little about the
/// value that was there.
fn redacted_scalar(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Number(_) => serde_json::Value::from(0),
        serde_json::Value::Bool(_) => serde_json::Value::Bool(false),
        // An absent value has nothing to hide, and must stay absent: `null` is how
        // an optional field says it was not set.
        serde_json::Value::Null => serde_json::Value::Null,
        _ => serde_json::Value::String(REDACTED.to_string()),
    }
}

fn scrub_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if is_sensitive_key(key) && !child.is_object() && !child.is_array() {
                    *child = redacted_scalar(child);
                } else {
                    scrub_json(child);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items.iter_mut() {
                scrub_json(item);
            }
        }
        // Every string value, whatever it is called. This is the arm that catches
        // a bearer token inside a `command`.
        serde_json::Value::String(text) => {
            let scanned = redact_secrets(text);
            if scanned != *text {
                *text = scanned;
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turn_core::model::layout::{Pane, PaneKind};

    #[test]
    fn every_documented_sensitive_fragment_is_caught_in_any_casing() {
        for key in [
            "GITHUB_TOKEN",
            "ANTHROPIC_API_KEY",
            "aws_secret_access_key",
            "DB_PASSWORD",
            "GOOGLE_APPLICATION_CREDENTIALS",
            "session_cookie",
            "AUTHORIZATION",
        ] {
            assert!(is_sensitive_key(key), "{key} must be treated as a secret");
        }
    }

    #[test]
    fn ordinary_variables_are_left_alone() {
        for key in ["PATH", "HOME", "LANG", "TERM", "CARGO_TARGET_DIR", "EDITOR"] {
            assert!(!is_sensitive_key(key), "{key} was needlessly redacted");
        }
    }

    /// The matcher is a substring match, which over-redacts. That is the trade
    /// this module chooses on purpose, and it is documented here so nobody
    /// "fixes" it into a word-boundary match that lets `deploy_key` through.
    #[test]
    fn the_matcher_prefers_over_redacting_to_leaking() {
        assert!(
            is_sensitive_key("MONKEY_MODE"),
            "a false positive is acceptable"
        );
        assert!(is_sensitive_key("deploy_key"));
        assert!(is_sensitive_key("KEYBOARD_LAYOUT"));
    }

    #[test]
    fn redacting_keeps_the_key_and_the_order_but_drops_the_value() {
        let env = vec![
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("GITHUB_TOKEN".to_string(), "ghp_reallysecret".to_string()),
            ("TERM".to_string(), "xterm".to_string()),
        ];
        let safe = redact_pairs(&env);

        assert_eq!(
            safe.len(),
            3,
            "the variable is still known to have been set"
        );
        assert_eq!(safe[0], ("PATH".to_string(), "/usr/bin".to_string()));
        assert_eq!(safe[1].0, "GITHUB_TOKEN");
        assert_eq!(safe[1].1, REDACTED);
        assert_eq!(safe[2].1, "xterm");
    }

    #[test]
    fn a_token_hidden_in_a_pane_environment_is_redacted_with_the_layout() {
        let mut pane = Pane::new(PaneKind::Agent).with_command("claude");
        pane.env = vec![("ANTHROPIC_API_KEY".to_string(), "sk-ant-secret".to_string())];
        let layout = Layout::single(pane);

        let safe = redact_layout(&layout);
        assert_eq!(safe.panes()[0].env[0].1, REDACTED);
        assert_eq!(
            layout.panes()[0].env[0].1,
            "sk-ant-secret",
            "redaction must not mutate the caller's layout"
        );
    }

    #[test]
    fn nested_json_payloads_are_scrubbed_at_every_depth() {
        let raw = r#"{"cwd":"/repo","headers":{"authorization":"Bearer abc123"},
                      "items":[{"api_key":"sk-1"}]}"#;
        let safe = redact_json(raw);
        assert!(!safe.contains("abc123"), "got {safe}");
        assert!(!safe.contains("sk-1"), "got {safe}");
        assert!(
            safe.contains("/repo"),
            "the useful payload survives: {safe}"
        );
    }

    #[test]
    fn a_payload_that_is_not_json_keeps_everything_that_is_not_a_credential() {
        let raw = "Stop hook fired for session 42";
        assert_eq!(redact_json(raw), raw);
    }

    /// The leak the key rule cannot see: a real credential under an innocent key.
    /// Every one of these is a shape that reaches Turn through a permission
    /// request or a prompt excerpt.
    #[test]
    fn a_credential_under_an_innocent_key_is_still_redacted() {
        let cases = [
            (
                r#"{"command":"curl -H 'Authorization: Bearer sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA' https://api.anthropic.com"}"#,
                "sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            ),
            (
                r#"{"prompt_excerpt":"here is my token ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 please fix the CI"}"#,
                "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789",
            ),
            (
                r#"{"summary":"Run `aws configure set aws_access_key_id AKIAIOSFODNN7EXAMPLE`"}"#,
                "AKIAIOSFODNN7EXAMPLE",
            ),
            (
                r#"{"command":"export SLACK=xoxb-1234567890-1234567890123-abcdefghijklmnop"}"#,
                "xoxb-1234567890-1234567890123-abcdefghijklmnop",
            ),
            (
                r#"{"note":"gitlab glpat-ABCDEFGHIJKLMNOPQRST is in CI"}"#,
                "glpat-ABCDEFGHIJKLMNOPQRST",
            ),
            (
                r#"{"header":"eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r_wW1gFWFOEjXk"}"#,
                "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9",
            ),
        ];

        for (raw, secret) in cases {
            let safe = redact_json(raw);
            assert!(!safe.contains(secret), "{secret} survived in {safe}");
            assert!(safe.contains(REDACTED), "nothing was marked in {safe}");
        }
    }

    /// A private key is many lines that individually look like nothing, so the
    /// block is recognised as a block.
    #[test]
    fn a_private_key_block_is_removed_whole() {
        let raw = "deploy step\n-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\nAAAABG5vbmUAAAAEbm9uZQ==\n-----END OPENSSH PRIVATE KEY-----\nfinished";
        let safe = redact_secrets(raw);

        assert!(!safe.contains("b3BlbnNzaC1rZXktdjEAAAAA"), "got {safe}");
        assert!(!safe.contains("BEGIN OPENSSH"), "got {safe}");
        assert!(safe.starts_with("deploy step"), "got {safe}");
        assert!(safe.trim_end().ends_with("finished"), "got {safe}");
    }

    /// The shapes a real key arrives in. Only one of them ends its header with
    /// exactly `-----\n`, and for a while that was the only one recognised: a key
    /// written by a Windows tool, pasted without its trailing newline, or echoed
    /// back inside a single line of JSON went to disk in full.
    #[test]
    fn a_private_key_is_recognised_however_its_header_line_ends() {
        let body = "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQ";
        let cases = [
            (
                "CRLF endings",
                format!("-----BEGIN RSA PRIVATE KEY-----\r\n{body}\r\n-----END RSA PRIVATE KEY-----\r\n"),
            ),
            (
                "no trailing newline",
                format!("-----BEGIN PRIVATE KEY-----\n{body}\n-----END PRIVATE KEY-----"),
            ),
            (
                "an encrypted key",
                format!("-----BEGIN ENCRYPTED PRIVATE KEY-----\n{body}\n-----END ENCRYPTED PRIVATE KEY-----\n"),
            ),
            (
                "indented by whatever quoted it",
                format!("    -----BEGIN OPENSSH PRIVATE KEY-----\n    {body}\n    -----END OPENSSH PRIVATE KEY-----\n"),
            ),
            (
                "welded into one line of JSON",
                format!(r#"{{"payload":"-----BEGIN PRIVATE KEY-----\n{body}\n-----END PRIVATE KEY-----\n"}}"#),
            ),
            (
                "no line breaks at all",
                format!("-----BEGIN PRIVATE KEY----- {body} -----END PRIVATE KEY-----"),
            ),
            (
                "truncated before it closes",
                format!("-----BEGIN PRIVATE KEY-----\n{body}"),
            ),
        ];

        for (shape, raw) in cases {
            let safe = redact_secrets(&raw);
            assert!(
                !safe.contains(body),
                "a key with {shape} reached the output: {safe}"
            );
            assert!(safe.contains(REDACTED), "nothing was marked for {shape}");
        }
    }

    /// The same key, arriving the way a hook payload actually delivers one.
    #[test]
    fn a_private_key_inside_a_stored_payload_is_removed_whether_or_not_it_parses() {
        let body = "b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAAB";
        let json = format!(
            r#"{{"tool":"Write","content":"-----BEGIN OPENSSH PRIVATE KEY-----\n{body}\n-----END OPENSSH PRIVATE KEY-----"}}"#
        );
        let safe = redact_json(&json);
        assert!(!safe.contains(body), "got {safe}");
        assert!(safe.contains("Write"), "the rest of the payload survives");

        // And when the payload is not valid JSON, so the text scanner is all
        // there is.
        let truncated = format!(r#"{{"content":"-----BEGIN PRIVATE KEY-----\n{body}"#);
        assert!(!redact_json(&truncated).contains(body));
    }

    /// The other half of the truncated-key rule. A payload that only *talks*
    /// about a header — a prompt about key handling, a quoted error, a diff of
    /// documentation — used to lose everything after the header, because the
    /// unterminated-block branch consumed the remainder of the text. Silent data
    /// loss in an audit trail is a worse trade than redacting a key that was
    /// never there.
    #[test]
    fn a_payload_that_only_mentions_a_private_key_header_keeps_the_text_after_it() {
        let raw = "the file must start with -----BEGIN RSA PRIVATE KEY----- and be chmod 600\n\
                   the agent asked to read ~/.ssh/id_rsa\n\
                   permission was denied";
        let safe = redact_secrets(raw);

        assert!(
            !safe.contains("BEGIN RSA"),
            "the header itself is still replaced: {safe}"
        );
        assert!(safe.contains(REDACTED), "got {safe}");
        assert!(
            safe.contains("and be chmod 600"),
            "the rest of the line survives: {safe}"
        );
        assert!(
            safe.contains("~/.ssh/id_rsa") && safe.contains("permission was denied"),
            "the rest of the payload survives: {safe}"
        );
    }

    /// The same mention arriving the way the store actually receives one: inside
    /// a hook payload, under an innocent key.
    #[test]
    fn a_mention_inside_a_stored_payload_costs_only_the_header() {
        let raw = r#"{"tool":"Read","prompt":"explain why -----BEGIN OPENSSH PRIVATE KEY----- must never be committed","cwd":"/repo"}"#;
        let safe = redact_json(raw);

        assert!(!safe.contains("BEGIN OPENSSH"), "got {safe}");
        assert!(
            safe.contains("must never be committed"),
            "the prompt survives past the mention: {safe}"
        );
        assert!(safe.contains("/repo"), "got {safe}");
    }

    /// Bounding the unterminated block must not reopen the leak it was written
    /// for: a key whose `-----END` never arrives still does not reach the file,
    /// and now only its own body goes with it.
    #[test]
    fn an_unterminated_private_key_is_still_removed_and_takes_only_its_own_body() {
        let first = "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQCw6D9kZQIBADAN";
        let second = "BgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQCw6D9kZQIBADANBgkqhkiG9w0B";
        let raw =
            format!("-----BEGIN RSA PRIVATE KEY-----\n{first}\n{second}\nthe log continues here");
        let safe = redact_secrets(&raw);

        assert!(!safe.contains(first), "the body reached the output: {safe}");
        assert!(
            !safe.contains(second),
            "the body reached the output: {safe}"
        );
        assert!(safe.contains(REDACTED), "got {safe}");
        assert!(
            safe.contains("the log continues here"),
            "only the key is taken: {safe}"
        );
    }

    /// An encrypted key carries `Proc-Type`/`DEK-Info` above its body. Those
    /// lines are not base64, so the body scan has to walk through them or the
    /// base64 underneath would survive a truncated key.
    #[test]
    fn an_unterminated_encrypted_key_loses_its_body_from_under_its_armour_headers() {
        let body = "9w0BAQEFAASCBKcwggSjAgEAAoIBAQCw6D9kZQIBADANBgkqhkiG9w0BAQEFAASC";
        let raw = format!(
            "-----BEGIN RSA PRIVATE KEY-----\nProc-Type: 4,ENCRYPTED\nDEK-Info: AES-128-CBC,0123456789ABCDEF\n\n{body}\ntail of the payload"
        );
        let safe = redact_secrets(&raw);

        assert!(!safe.contains(body), "got {safe}");
        assert!(safe.contains("tail of the payload"), "got {safe}");
    }

    /// The closing marker settles the question, so nothing inside a block that
    /// closes has to look long enough to be key material on its own: a body's
    /// final line is short by nature. Only an unterminated block guesses.
    #[test]
    fn a_closed_block_is_removed_whole_however_short_its_lines_are() {
        let raw = "before\n-----BEGIN PRIVATE KEY-----\nAAA=\n-----END PRIVATE KEY-----\nafter";
        let safe = redact_secrets(raw);

        assert_eq!(safe, "before\n[redacted]\nafter", "got {safe}");
    }

    /// The guarantee written on the header parser: the label stops at the first
    /// line break, so a stray `-----BEGIN` cannot reach a `-----` further down
    /// the payload, read the words in between as its label, and redact from the
    /// mention to the end of the text.
    #[test]
    fn a_header_that_never_closes_cannot_borrow_the_words_private_key_from_later_text() {
        let raw = "-----BEGIN\nthe words private key appear in -----this heading\nkeep this line";
        assert_eq!(
            redact_secrets(raw),
            raw,
            "nothing here is a private key header"
        );
    }

    /// A certificate is public. Hiding it would only make a TLS complaint harder
    /// to read.
    #[test]
    fn a_public_certificate_is_left_alone() {
        let raw = "-----BEGIN CERTIFICATE-----\nMIIBkTCB+w==\n-----END CERTIFICATE-----";
        assert_eq!(redact_secrets(raw), raw);
    }

    /// The scanner runs over every stored string, so a false positive would eat
    /// the log. These are the shapes that must survive.
    #[test]
    fn ordinary_developer_text_is_not_mistaken_for_a_credential() {
        for innocent in [
            "cargo test -p turn-store",
            "/Users/jamuriano/personal-workspace/turn/crates/turn-store/src/redact.rs",
            "fix(store): redact secrets in 4f3a9c1e8b2d5a7f0c3e6b9d2a5f8c1e4b7a0d3f",
            "84cde77e-f54f-41e7-bb05-2716cb61b6bf",
            "git commit --amend --no-edit",
            "sk-",
            "ask-a-question",
            "npm_config_registry",
            "https://github.com/jamuriano/turn/pull/42",
            "AKIA",
            "eyJ",
        ] {
            assert_eq!(
                redact_secrets(innocent),
                innocent,
                "{innocent:?} was needlessly redacted"
            );
        }
    }

    #[test]
    fn a_token_inside_an_otherwise_innocent_environment_value_is_caught() {
        let env = vec![(
            "NPM_CONFIG_REGISTRY".to_string(),
            "https://npm_ABCDEFGHIJKLMNOPQRSTUVWXYZ012345@registry.example".to_string(),
        )];
        let safe = redact_pairs(&env);
        assert!(!safe[0].1.contains("npm_ABCDEFGHIJKLMNOPQRSTUVWXYZ012345"));
        assert!(
            safe[0].1.contains("registry.example"),
            "the useful part survives: {}",
            safe[0].1
        );
    }

    /// The key rule is a greedy substring match, so it lands on fields that were
    /// never credentials — a count, a flag. Overwriting those with the marker
    /// *string* would leave the row undecodable for good, which loses the event
    /// entirely rather than one field of it.
    #[test]
    fn redacting_a_scalar_keeps_its_json_type_so_the_row_stays_readable() {
        let raw = r#"{"tokens_used":4096,"auth_required":true,"api_key":null,
                      "secret_note":"ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"}"#;
        let safe = redact_json(raw);
        let value: serde_json::Value = serde_json::from_str(&safe).expect("still JSON");

        assert_eq!(
            value["tokens_used"],
            serde_json::json!(0),
            "a number must stay a number: {safe}"
        );
        assert_eq!(
            value["auth_required"],
            serde_json::json!(false),
            "a boolean must stay a boolean: {safe}"
        );
        assert!(
            value["api_key"].is_null(),
            "an unset optional stays unset: {safe}"
        );
        assert_eq!(value["secret_note"], serde_json::json!(REDACTED));
    }

    #[test]
    fn a_sensitive_key_holding_an_object_is_walked_rather_than_flattened() {
        // "auth" is sensitive, but blanking the whole object would throw away
        // the structure a bad adapter has to be debugged from.
        let raw = r#"{"auth":{"scheme":"bearer","token":"t0ps3cret"}}"#;
        let safe = redact_json(raw);
        assert!(!safe.contains("t0ps3cret"), "got {safe}");
        assert!(safe.contains("bearer"), "got {safe}");
    }
}
