//! Safe, rate-limited activity previews for the unified hierarchy.
//!
//! A preview is deliberately not a tail of PTY bytes. It is a small semantic
//! snapshot built from an adapter event where possible, or from a stable row of
//! the already-parsed terminal screen as a fallback. This keeps ANSI, carriage
//! return progress, alternate-screen noise and secrets out of navigation.

use super::Core;
use turn_core::event::{Confidence, EventKind, TurnEvent};
use turn_core::ids::{NodeId, SessionId};
use turn_core::model::{ActivityPreview, PreviewSource, PreviewVisibility};
use turn_pty::ScreenSnapshot;

pub(crate) const ACTIVE_PREVIEW_INTERVAL_MS: i64 = 500;
pub(crate) const BACKGROUND_PREVIEW_INTERVAL_MS: i64 = 1_500;
pub(crate) const MAX_PREVIEW_CHARS: usize = 96;

#[derive(Debug, Default)]
pub(crate) struct PreviewProbe {
    candidate: Option<String>,
    repeated: u8,
    published: Option<String>,
    next_due_ms: i64,
}

impl PreviewProbe {
    fn due(&self, now_ms: i64) -> bool {
        now_ms >= self.next_due_ms
    }

    fn defer(&mut self, watched: bool, now_ms: i64) {
        self.next_due_ms = now_ms
            + if watched {
                ACTIVE_PREVIEW_INTERVAL_MS
            } else {
                BACKGROUND_PREVIEW_INTERVAL_MS
            };
    }

    /// Publishes a row immediately when it is already complete, or after seeing
    /// the same cursor row twice. That second path prevents a command being typed
    /// character-by-character from making the sidebar flicker.
    fn observe(&mut self, snapshot: &ScreenSnapshot) -> Option<(String, u64)> {
        let (candidate, under_cursor) = snapshot_candidate(snapshot)?;
        if self.candidate.as_deref() == Some(&candidate) {
            self.repeated = self.repeated.saturating_add(1);
        } else {
            self.candidate = Some(candidate.clone());
            self.repeated = 1;
        }
        if under_cursor && self.repeated < 2 {
            return None;
        }
        if self.published.as_deref() == Some(&candidate) {
            return None;
        }
        self.published = Some(candidate.clone());
        Some((candidate, snapshot.bytes_seen))
    }
}

impl Core {
    /// Applies the highest-quality compact message carried by a structured event.
    /// Returns whether the hierarchy projection changed.
    pub(crate) fn update_preview_from_event(
        &mut self,
        event: &TurnEvent,
        target: Option<&NodeId>,
        now_ms: i64,
    ) -> bool {
        let Some(text) = semantic_text(&event.kind) else {
            return false;
        };
        let Some(node_id) = target.or(event.node_id.as_ref()) else {
            return false;
        };
        self.set_activity_preview(
            &event.session_id,
            node_id,
            &text,
            PreviewSource::SemanticEvent,
            event.confidence,
            None,
            true,
            now_ms,
        )
    }

    /// Samples every live PTY at a bounded rate. The PTY parser is authoritative;
    /// raw output never enters this path.
    pub(crate) fn observe_activity_previews(&mut self, now_ms: i64) {
        let due: Vec<(NodeId, SessionId, bool, ScreenSnapshot)> = self
            .processes
            .iter()
            .filter_map(|(node_id, process)| {
                if self
                    .sessions
                    .get(&process.session_id)
                    .is_none_or(|session| session.is_archived())
                {
                    return None;
                }
                let watched = self.is_watched(node_id);
                let probe = self.preview_probes.get(node_id);
                if probe.is_some_and(|probe| !probe.due(now_ms)) || !process.pty.is_running() {
                    return None;
                }
                process.pty.snapshot().map(|snapshot| {
                    (
                        node_id.clone(),
                        process.session_id.clone(),
                        watched,
                        snapshot,
                    )
                })
            })
            .collect();

        let mut changed = Vec::new();
        for (node_id, session_id, watched, snapshot) in due {
            let probe = self.preview_probes.entry(node_id.clone()).or_default();
            probe.defer(watched, now_ms);
            if let Some((candidate, sequence)) = probe.observe(&snapshot) {
                changed.push((session_id, node_id, candidate, sequence));
            }
        }

        for (session_id, node_id, candidate, sequence) in changed {
            if self.set_activity_preview(
                &session_id,
                &node_id,
                &candidate,
                PreviewSource::StableScreenLine,
                Confidence::InferredHigh,
                Some(sequence),
                true,
                now_ms,
            ) {
                self.persist_session_quietly(&session_id);
                self.push_activity_preview(&session_id, &node_id, now_ms);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn set_activity_preview(
        &mut self,
        session_id: &SessionId,
        node_id: &NodeId,
        raw: &str,
        source: PreviewSource,
        confidence: Confidence,
        sequence: Option<u64>,
        stable: bool,
        now_ms: i64,
    ) -> bool {
        let Some((text, redacted)) = normalise_preview(raw) else {
            return false;
        };
        let Some(session) = self.sessions.get_mut(session_id) else {
            return false;
        };
        let Some(node) = session.tree.get_mut(node_id) else {
            return false;
        };
        if node.preview_visibility == PreviewVisibility::Hide {
            return false;
        }
        let next = ActivityPreview {
            node_id: node_id.clone(),
            raw_source_sequence: sequence,
            normalized_text: text,
            source,
            confidence,
            stable,
            contains_sensitive_data: redacted,
            redacted,
            updated_ms: now_ms,
        };
        if node.activity_preview.as_ref().is_some_and(|current| {
            current.normalized_text == next.normalized_text
                && current.source == next.source
                && current.redacted == next.redacted
        }) {
            return false;
        }
        node.activity_preview = Some(next);
        true
    }
}

fn semantic_text(kind: &EventKind) -> Option<String> {
    match kind {
        EventKind::AgentSpawned {
            task,
            declared_name,
            ..
        } => task
            .clone()
            .or_else(|| declared_name.as_ref().map(|name| format!("{name} started"))),
        EventKind::AgentTurnStarted { prompt_excerpt } => prompt_excerpt.clone(),
        EventKind::AgentWaitingForUser { summary, .. } => summary.clone(),
        EventKind::AgentQuestionAsked { question } => Some(question.clone()),
        EventKind::AgentPermissionRequired { summary, .. } => Some(summary.clone()),
        EventKind::AgentTurnCompleted { last_message, .. } => last_message.clone(),
        EventKind::AgentTaskCompleted { summary } => summary.clone(),
        EventKind::AgentFailed { reason } => Some(format!("Failed: {reason}")),
        EventKind::ProcessStarted { command, .. } => Some(format!("Running {command}")),
        _ => None,
    }
}

fn snapshot_candidate(snapshot: &ScreenSnapshot) -> Option<(String, bool)> {
    for (row, raw) in snapshot.lines.iter().enumerate().rev() {
        let Some((candidate, _)) = normalise_preview(raw) else {
            continue;
        };
        let under_cursor = row == usize::from(snapshot.cursor.0);
        return Some((candidate, under_cursor));
    }
    None
}

/// Resolves rewritten lines, removes terminal controls and spinner noise, redacts
/// credentials and truncates on a Unicode scalar boundary.
pub(crate) fn normalise_preview(raw: &str) -> Option<(String, bool)> {
    let rewritten = raw
        .split(['\n', '\r'])
        .rev()
        .find(|part| !part.trim().is_empty())?;
    let safe = turn_pty::sanitise_label(rewritten, usize::MAX)?;
    let compact = safe.split_whitespace().collect::<Vec<_>>().join(" ");
    let compact = trim_spinner(&compact).trim();
    if compact.is_empty() || is_noise(compact) {
        return None;
    }
    let redacted_text = turn_store::redact::redact_secrets(compact);
    let redacted = redacted_text != compact;
    let text: String = redacted_text.chars().take(MAX_PREVIEW_CHARS).collect();
    if text.is_empty() {
        None
    } else if redacted_text.chars().count() > MAX_PREVIEW_CHARS {
        let mut truncated = text;
        if truncated.chars().count() >= 1 {
            truncated.pop();
        }
        truncated.push('…');
        Some((truncated, redacted))
    } else {
        Some((text, redacted))
    }
}

fn trim_spinner(text: &str) -> &str {
    const SPINNERS: &[char] = &[
        '⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏', '◐', '◓', '◑', '◒',
    ];
    match text.chars().next() {
        Some(first) if SPINNERS.contains(&first) => &text[first.len_utf8()..],
        _ => text,
    }
}

fn is_noise(text: &str) -> bool {
    matches!(text, ">" | "❯" | "$" | "#" | "…" | "..." | "-")
        || text
            .chars()
            .all(|ch| ch.is_whitespace() || "|/-\\⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏".contains(ch))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_carriage_returns_and_removes_ansi_and_spinner_prefixes() {
        let (preview, redacted) = normalise_preview(
            "\x1b[33m⠋ Running tests 11/18\x1b[0m\r\x1b[32m⠙ Running tests 12/18\x1b[0m",
        )
        .expect("a useful preview");
        assert_eq!(preview, "Running tests 12/18");
        assert!(!redacted);
    }

    #[test]
    fn ignores_prompts_and_bare_spinners() {
        assert!(normalise_preview("❯").is_none());
        assert!(normalise_preview("⠋").is_none());
    }

    #[test]
    fn redacts_credentials_before_the_preview_can_reach_disk_or_ui() {
        let (preview, redacted) =
            normalise_preview("Authorization: Bearer sk-proj-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
                .expect("the line remains representable");
        assert!(redacted);
        assert!(!preview.contains("AAAAAAAA"));
        assert!(preview.contains("[redacted]"));
    }

    #[test]
    fn truncates_unicode_without_splitting_a_scalar() {
        let long = "🧪".repeat(MAX_PREVIEW_CHARS + 20);
        let (preview, _) = normalise_preview(&long).unwrap();
        assert_eq!(preview.chars().count(), MAX_PREVIEW_CHARS);
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn thirty_sessions_keep_active_and_background_preview_cadence_bounded() {
        const START: i64 = 1_700_000_000_000;
        let mut probes: Vec<PreviewProbe> = (0..30).map(|_| PreviewProbe::default()).collect();
        for (index, probe) in probes.iter_mut().enumerate() {
            probe.defer(index < 3, START);
        }

        assert_eq!(
            probes
                .iter()
                .filter(|probe| probe.due(START + ACTIVE_PREVIEW_INTERVAL_MS - 1))
                .count(),
            0
        );
        assert_eq!(
            probes
                .iter()
                .filter(|probe| probe.due(START + ACTIVE_PREVIEW_INTERVAL_MS))
                .count(),
            3,
            "only the three watched panes sample at the active cadence"
        );
        assert_eq!(
            probes
                .iter()
                .filter(|probe| probe.due(START + BACKGROUND_PREVIEW_INTERVAL_MS))
                .count(),
            30,
            "background panes are delayed, not starved"
        );
    }
}
