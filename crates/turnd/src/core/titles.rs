//! Process-supplied terminal titles and their deliberately low naming priority.
//!
//! The terminal parser already owns OSC parsing and sanitisation. This module is
//! only the projection boundary: it associates that safe title with the exact
//! PTY-backed node, updates the tree, and leaves layout, focus and Attention alone.

use super::Core;
use turn_core::event::Confidence;
use turn_core::ids::NodeId;
use turn_core::model::NameSource;

impl Core {
    /// Observes all PTYs, including panes no UI currently watches.
    ///
    /// This is why closing and reopening Turn's window does not lose a title: the
    /// daemon remains the state owner even when no output pump has a subscriber.
    pub(crate) fn observe_process_titles(&mut self, now_ms: i64) {
        let nodes: Vec<NodeId> = self.processes.keys().cloned().collect();
        for node in nodes {
            self.observe_process_title(&node, now_ms);
        }
    }

    /// Projects the current OSC title of one PTY into its own node.
    pub(crate) fn observe_process_title(&mut self, node_id: &NodeId, now_ms: i64) {
        let Some(process) = self.processes.get(node_id) else {
            return;
        };
        let Ok(observed) = process.pty.title() else {
            return;
        };

        // Keep the PTY read and registry mutation as two lookups. The first lookup
        // immutably borrows `self.processes` while the buffer mutex is held; ending
        // that borrow before the mutable lookup below is what keeps the lock out of
        // the state projection and satisfies the borrow checker.
        self.apply_observed_process_title(node_id, observed, now_ms);
    }

    /// Projects a title already read while another terminal operation held the lock.
    pub(crate) fn apply_observed_process_title(
        &mut self,
        node_id: &NodeId,
        observed: Option<String>,
        now_ms: i64,
    ) {
        let Some(process) = self.processes.get_mut(node_id) else {
            return;
        };
        if process.process_title == observed {
            return;
        }
        process.process_title = observed.clone();
        let session_id = process.session_id.clone();
        let fallback_title = process.fallback_title.clone();
        let fallback_agent_name = process.fallback_agent_name.clone();

        let Some(session) = self.sessions.get_mut(&session_id) else {
            return;
        };
        let Some(node) = session.tree.get_mut(node_id) else {
            return;
        };

        let next_title = observed.as_deref().unwrap_or(&fallback_title);
        let mut changed = false;
        if node.title != next_title {
            node.title = next_title.to_string();
            changed = true;
        }

        if let Some(agent) = node.agent.as_mut() {
            match observed {
                Some(title)
                    if !agent.name.user_renamed
                        && agent.name.declared_name.is_none()
                        && matches!(
                            agent.name.source,
                            NameSource::ProcessTitle | NameSource::Inferred | NameSource::Fallback
                        ) =>
                {
                    if agent.name.display_name != title
                        || agent.name.source != NameSource::ProcessTitle
                        || agent.name.confidence != Confidence::InferredHigh
                    {
                        agent.name.display_name = title;
                        agent.name.source = NameSource::ProcessTitle;
                        agent.name.confidence = Confidence::InferredHigh;
                        changed = true;
                    }
                }
                None if !agent.name.user_renamed
                    && agent.name.declared_name.is_none()
                    && agent.name.source == NameSource::ProcessTitle =>
                {
                    if let Some(fallback) = fallback_agent_name {
                        if agent.name != fallback {
                            agent.name = fallback;
                            changed = true;
                        }
                    }
                }
                _ => {}
            }
        }

        if changed {
            self.persist_session_quietly(&session_id);
            self.push_tree(&session_id, now_ms);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::core::testing::Harness;
    use turn_core::ids::{PaneId, SessionId};
    use turn_core::model::{AgentInfo, AgentName, Direction, Pane, PaneKind};

    const NOW: i64 = 1_775_000_000_000;

    /// The acceptance path uses two actual PTYs. Each `cat` writes the OSC sequence
    /// into its own terminal buffer; no parser fixture or direct callback is used.
    #[tokio::test]
    async fn real_ptys_keep_dynamic_titles_isolated_and_preserve_stronger_names() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_dynamic_titles");
        let first_pane = PaneId::from_stored("pane_dynamic_alpha");
        let second_pane = PaneId::from_stored("pane_dynamic_beta");
        harness.add_session(session_id.clone(), first_pane.clone(), NOW);

        let mut second = Pane::new(PaneKind::Agent).with_title("claude");
        second.id = second_pane.clone();
        assert!(harness
            .core
            .sessions
            .get_mut(&session_id)
            .unwrap()
            .layout
            .split(&first_pane, Direction::Horizontal, second));

        let first_node = harness.spawn_process(&session_id, &first_pane, NOW).await;
        let second_node = harness.spawn_process(&session_id, &second_pane, NOW).await;

        // A declared/integration name is stronger than an OSC process title. The
        // node title still records the process title for pane chrome and inspectors.
        let first = harness
            .core
            .sessions
            .get_mut(&session_id)
            .unwrap()
            .tree
            .get_mut(&first_node)
            .unwrap();
        first.agent = Some(AgentInfo {
            name: AgentName::declared("Reviewer"),
            ..AgentInfo::default()
        });

        let layout_before = harness.core.sessions[&session_id].layout.clone();
        harness
            .feed(&first_node, b"\x1b]0;Claude Alpha\x07\n")
            .await;
        harness
            .feed(&second_node, b"\x1b]2;Claude Beta\x1b\\\n")
            .await;

        let session = &harness.core.sessions[&session_id];
        let first = session.tree.get(&first_node).unwrap();
        let second = session.tree.get(&second_node).unwrap();
        assert_eq!(first.title, "Claude Alpha");
        assert_eq!(second.title, "Claude Beta");
        assert_eq!(
            first.agent.as_ref().unwrap().name.display_name,
            "Reviewer",
            "a process title must not replace a declared agent name"
        );
        assert_eq!(
            session.layout, layout_before,
            "title changes must not move focus or mutate layout"
        );

        // No client is attached and no pump is running here. The reader still owns
        // the authoritative buffer, and the daemon's periodic observer must learn
        // the new title so a later UI attach sees it immediately.
        harness.core.processes[&second_node]
            .pty
            .write(b"\x1b]2;Claude Beta updated\x07UPDATED\n")
            .unwrap();
        harness.wait_for_output(&second_node, "UPDATED").await;
        harness.core.observe_process_titles(NOW + 1);
        let session = &harness.core.sessions[&session_id];
        assert_eq!(session.tree.get(&first_node).unwrap().title, "Claude Alpha");
        assert_eq!(
            session.tree.get(&second_node).unwrap().title,
            "Claude Beta updated"
        );

        // The projection is durable daemon state, not a window-local cache.
        let persisted = harness
            .core
            .store
            .sessions()
            .get(&session_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            persisted.tree.get(&second_node).unwrap().title,
            "Claude Beta updated"
        );

        harness.core.processes[&second_node]
            .pty
            .write(b"\x1b]2;safe\x1b[2Jtitle\x07HOSTILE\n")
            .unwrap();
        harness.wait_for_output(&second_node, "HOSTILE").await;
        harness.core.observe_process_titles(NOW + 2);
        assert_eq!(
            harness.core.sessions[&session_id]
                .tree
                .get(&second_node)
                .unwrap()
                .title,
            "safe",
            "escape syntax inside a process title must not reach UI state"
        );

        harness.core.processes[&second_node]
            .pty
            .write(b"\x1b]2;\x07CLEARED\n")
            .unwrap();
        harness.wait_for_output(&second_node, "CLEARED").await;
        harness.core.observe_process_titles(NOW + 3);
        assert_eq!(
            harness.core.sessions[&session_id]
                .tree
                .get(&second_node)
                .unwrap()
                .title,
            "cat",
            "clearing an OSC title restores the process's configured fallback"
        );
    }
}
