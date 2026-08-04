//! Projections, and the pushes built from them.
//!
//! Every product rule is applied here rather than in the client: the sidebar label,
//! the severity, whether a parent link is a guess, how many demands a session has
//! raised. A UI that computed any of these would be a second implementation of the
//! rules, written from a screenshot.

use super::command::ClientId;
use super::Core;
use turn_core::ids::{NodeId, SessionId, WorkspaceId};
use turn_core::model::SessionStatus;
use turn_proto::{
    AttentionView, ServerEvent, SessionDetails, SessionSummary, TemplateSummary, TreeNodeView,
    WorkspaceSummary,
};

impl Core {
    /// One session row, with the attention manager's numbers folded in.
    pub(crate) fn session_summary(&self, id: &SessionId, now_ms: i64) -> Option<SessionSummary> {
        let session = self.sessions.get(id)?;
        let badge = self.attention.queue().count_for_session(id, now_ms);
        let muted = self.attention.is_muted(id, now_ms);
        Some(SessionSummary::from_session(session, badge, muted, now_ms))
    }

    /// Full detail for one session.
    pub(crate) fn session_details(&self, id: &SessionId, now_ms: i64) -> Option<SessionDetails> {
        let session = self.sessions.get(id)?;
        let badge = self.attention.queue().count_for_session(id, now_ms);
        let muted = self.attention.is_muted(id, now_ms);
        Some(SessionDetails::from_session(session, badge, muted, now_ms))
    }

    /// Session rows, ordered the way the sidebar shows them: pinned first, then
    /// anything blocked on the user, then by severity, then by recency.
    pub(crate) fn session_summaries(
        &self,
        workspace: Option<&WorkspaceId>,
        include_archived: bool,
        now_ms: i64,
    ) -> Vec<SessionSummary> {
        let mut out: Vec<SessionSummary> = self
            .sessions
            .values()
            .filter(|session| workspace.is_none_or(|id| &session.workspace_id == id))
            .filter(|session| include_archived || session.status != SessionStatus::Archived)
            .filter_map(|session| self.session_summary(&session.id, now_ms))
            .collect();
        out.sort_by_key(|summary| std::cmp::Reverse(summary.sidebar_rank()));
        out
    }

    /// Workspace rows. The counts are added up from the session summaries so a
    /// workspace badge can never disagree with the badges inside it.
    pub(crate) fn workspace_summaries(
        &self,
        include_archived: bool,
        now_ms: i64,
    ) -> Vec<WorkspaceSummary> {
        let sessions = self.session_summaries(None, false, now_ms);
        let mut out: Vec<WorkspaceSummary> = self
            .workspaces
            .values()
            .filter(|workspace| include_archived || !workspace.archived)
            .map(|workspace| WorkspaceSummary::from_workspace(workspace, &sessions))
            .collect();
        out.sort_by_key(|summary| std::cmp::Reverse(summary.last_used_ms));
        out
    }

    pub(crate) fn workspace_summary(
        &self,
        id: &WorkspaceId,
        now_ms: i64,
    ) -> Option<WorkspaceSummary> {
        let workspace = self.workspaces.get(id)?;
        let sessions = self.session_summaries(None, false, now_ms);
        Some(WorkspaceSummary::from_workspace(workspace, &sessions))
    }

    pub(crate) fn template_summaries(&self) -> Vec<TemplateSummary> {
        let mut out: Vec<TemplateSummary> = self
            .templates
            .values()
            .map(TemplateSummary::from_template)
            .collect();
        // Built-ins first, then by name, so the picker is stable between runs.
        out.sort_by(|a, b| {
            b.built_in
                .cmp(&a.built_in)
                .then_with(|| a.name.cmp(&b.name))
        });
        out
    }

    /// A session's process tree in draw order.
    pub(crate) fn tree_views(&self, id: &SessionId, now_ms: i64) -> Vec<TreeNodeView> {
        match self.sessions.get(id) {
            Some(session) => TreeNodeView::for_session(session, now_ms),
            None => Vec::new(),
        }
    }

    /// One tree row, with its depth and child count as drawn.
    ///
    /// Taken out of the flattened tree rather than built from the node alone, so the
    /// depth a client renders is the depth the daemon would have sent in a
    /// `tree_changed` — a node's place in the tree is not a property of the node.
    pub(crate) fn node_view(
        &self,
        session: &SessionId,
        node: &NodeId,
        now_ms: i64,
    ) -> Option<TreeNodeView> {
        self.tree_views(session, now_ms)
            .into_iter()
            .find(|view| &view.node_id == node)
    }

    // -------------------------------------------------------------------- pushes

    /// Tells every client a session's summary changed.
    pub(crate) fn push_session_state(&mut self, id: &SessionId, now_ms: i64) {
        if let Some(session) = self.session_summary(id, now_ms) {
            self.push_all(ServerEvent::SessionStateChanged {
                session: Box::new(session),
            });
        }
    }

    /// Tells every client the process tree changed.
    pub(crate) fn push_tree(&mut self, id: &SessionId, now_ms: i64) {
        let nodes = self.tree_views(id, now_ms);
        self.push_all(ServerEvent::TreeChanged {
            session_id: id.clone(),
            nodes,
        });
    }

    /// Tells the other clients the layout changed. The client that asked already has
    /// the answer in its response.
    pub(crate) fn push_layout(&mut self, id: &SessionId, except: Option<ClientId>) {
        let Some(session) = self.sessions.get(id) else {
            return;
        };
        let event = ServerEvent::LayoutChanged {
            session_id: id.clone(),
            layout: session.layout.clone(),
        };
        match except {
            Some(client) => self.push_others(client, event),
            None => self.push_all(event),
        }
    }

    /// Tells every client a node's state changed, on both axes plus the projection.
    pub(crate) fn push_node_state(
        &mut self,
        session: &SessionId,
        node: &NodeId,
        caused_by: Option<turn_core::TurnEvent>,
        now_ms: i64,
    ) {
        let Some(view) = self.node_view(session, node, now_ms) else {
            return;
        };
        self.push_all(ServerEvent::NodeStateChanged {
            session_id: session.clone(),
            node_id: node.clone(),
            lifecycle: view.lifecycle.clone(),
            turn: view.turn.clone(),
            display_state: view.display_state,
            caused_by: caused_by.map(Box::new),
        });
    }

    /// The attention queue as the panel draws it, most urgent first.
    ///
    /// Built from the whole queue rather than through
    /// [`AttentionView::from_queue`], which projects only what is actionable right
    /// now. A snoozed demand has to appear — greyed out, which is what
    /// `AttentionView::actionable` is for — because a user who snoozes something and
    /// watches it vanish from the list has been given no reason to believe it will
    /// come back. Snoozed entries score far below everything else, so they sort to
    /// the bottom on their own.
    pub(crate) fn attention_views(&self, now_ms: i64) -> Vec<AttentionView> {
        let mut out: Vec<AttentionView> = self
            .attention
            .queue()
            .iter()
            .map(|entry| {
                let name = self
                    .sessions
                    .get(&entry.session_id)
                    .map(|session| session.name.clone())
                    .unwrap_or_else(|| entry.session_id.as_str().to_string());
                AttentionView::from_entry(entry, name, now_ms)
            })
            .collect();
        out.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                // Ties break toward the older demand, so the list drains predictably
                // instead of shuffling between pushes.
                .then_with(|| a.entry.created_ms.cmp(&b.entry.created_ms))
        });
        out
    }

    /// Tells every client the queue changed shape.
    pub(crate) fn push_attention_queue(&mut self, now_ms: i64) {
        let entries = self.attention_views(now_ms);
        self.push_all(ServerEvent::AttentionQueueChanged { entries });
    }
}
