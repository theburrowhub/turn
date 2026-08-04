//! Unified navigation, checkout leases, activity previews and node views.
//!
//! These handlers deliberately keep four identities separate: selecting a tree
//! node, focusing a Pane, resolving Attention and owning a write lease are four
//! different operations. None of the handlers below performs one as a side
//! effect of another.

use std::collections::HashMap;

use super::workspaces::store;
use super::Answer;
use crate::core::Core;
use turn_core::ids::{CheckoutId, LeaseId, NodeId, PaneId, SessionId, WorkspaceId};
use turn_core::model::{HierarchyNodeKind, PaneNodeBinding, SessionMode, TreeUiState};
use turn_proto::{
    ErrorCode, HierarchyKey, HierarchySnapshot, NodePaneCapability, NodePaneView, PaneFocusView,
    PaneStream, ProtoError, ProtoErrorContext, Response, SessionConflictAlternative,
    SessionTreeView, TreeNodeView, TreeSurfaceState, WorkspaceTreeView, WriteLeaseOwnerView,
};

const MAX_SURFACE_ID_CHARS: usize = 128;
const LEASE_HEARTBEAT_INTERVAL_MS: i64 = 5_000;

impl Core {
    /// Associates a protocol client with its window id before producing the
    /// snapshot. Kept separate from `get_hierarchy` because client identity is a
    /// transport concern, while snapshot construction is pure projection.
    pub(super) fn get_hierarchy_for_client(
        &mut self,
        client_id: super::ClientId,
        surface_id: String,
        include_archived: bool,
        now_ms: i64,
    ) -> Answer {
        let surface_id = validate_surface_id(&surface_id)?;
        let client = self
            .clients
            .get_mut(&client_id)
            .ok_or_else(|| ProtoError::internal("this connection is not registered"))?;
        client.surface_id = Some(surface_id.clone());
        Ok(Response::Hierarchy {
            snapshot: Box::new(self.hierarchy_snapshot(&surface_id, include_archived, now_ms)?),
        })
    }

    pub(super) fn set_tree_expanded(
        &self,
        surface_id: String,
        key: HierarchyKey,
        expanded: bool,
        now_ms: i64,
    ) -> Answer {
        let surface_id = validate_surface_id(&surface_id)?;
        self.require_hierarchy_key(&key)?;
        let previous = self
            .store
            .hierarchy()
            .tree_state(&surface_id)
            .map_err(store)?;
        let (kind, node_id) = key_parts(&key);
        let old = previous
            .iter()
            .find(|state| state.node_kind == kind && state.node_id == node_id);
        self.store
            .hierarchy()
            .save_tree_state(&TreeUiState {
                surface_id: surface_id.clone(),
                node_kind: kind,
                node_id,
                expanded,
                selected: old.is_some_and(|state| state.selected),
                manual_order: old.and_then(|state| state.manual_order),
                visibility_mode: old.and_then(|state| state.visibility_mode.clone()),
                updated_ms: now_ms,
            })
            .map_err(store)?;
        Ok(Response::TreeState {
            state: self.tree_surface_state(&surface_id)?,
        })
    }

    pub(super) fn select_tree_node(
        &self,
        surface_id: String,
        selected: Option<HierarchyKey>,
        now_ms: i64,
    ) -> Answer {
        let surface_id = validate_surface_id(&surface_id)?;
        if let Some(key) = &selected {
            self.require_hierarchy_key(key)?;
        }
        let previous = self
            .store
            .hierarchy()
            .tree_state(&surface_id)
            .map_err(store)?;
        // Clearing selection is represented by clearing the selected bit on the
        // existing row. It never focuses a Pane or acknowledges Attention.
        for state in previous.iter().filter(|state| state.selected) {
            let mut cleared = state.clone();
            cleared.selected = false;
            cleared.updated_ms = now_ms;
            self.store
                .hierarchy()
                .save_tree_state(&cleared)
                .map_err(store)?;
        }
        if let Some(key) = selected {
            let (kind, node_id) = key_parts(&key);
            let old = previous
                .iter()
                .find(|state| state.node_kind == kind && state.node_id == node_id);
            self.store
                .hierarchy()
                .save_tree_state(&TreeUiState {
                    surface_id: surface_id.clone(),
                    node_kind: kind,
                    node_id,
                    expanded: old.is_some_and(|state| state.expanded),
                    selected: true,
                    manual_order: old.and_then(|state| state.manual_order),
                    visibility_mode: old.and_then(|state| state.visibility_mode.clone()),
                    updated_ms: now_ms,
                })
                .map_err(store)?;
        }
        Ok(Response::TreeState {
            state: self.tree_surface_state(&surface_id)?,
        })
    }

    pub(super) fn workspace_write_lease(&self, workspace_id: &WorkspaceId) -> Answer {
        self.workspace(workspace_id)?;
        let lease = self
            .store
            .hierarchy()
            .active_lease(workspace_id)
            .map_err(store)?;
        Ok(Response::WorkspaceWriteLease {
            workspace_id: workspace_id.clone(),
            lease,
        })
    }

    pub(super) fn acquire_workspace_write_lease(
        &mut self,
        workspace_id: &WorkspaceId,
        session_id: &SessionId,
        checkout_id: &CheckoutId,
        now_ms: i64,
    ) -> Answer {
        self.workspace(workspace_id)?;
        let session = self.session(session_id)?;
        if &session.workspace_id != workspace_id {
            return Err(ProtoError::invalid(
                "The Session does not belong to that Workspace",
            ));
        }
        let lease = self
            .store
            .hierarchy()
            .acquire_write_lease(workspace_id, session_id, checkout_id, now_ms)
            .map_err(|error| self.map_lease_store_error(workspace_id, Some(session_id), error))?;
        let session = self.session_mut(session_id)?;
        session.mode = SessionMode::MainCheckout;
        session.checkout_id = checkout_id.clone();
        session.worktree_path = None;
        session.read_only_enforced = false;
        self.persist_session(session_id)?;
        self.bump_hierarchy();
        self.push_workspace_lease(workspace_id, Some(lease.clone()), now_ms);
        Ok(Response::WorkspaceWriteLease {
            workspace_id: workspace_id.clone(),
            lease: Some(lease),
        })
    }

    pub(super) fn release_workspace_write_lease(
        &mut self,
        workspace_id: &WorkspaceId,
        lease_id: &LeaseId,
        expected_generation: u64,
        now_ms: i64,
    ) -> Answer {
        self.workspace(workspace_id)?;
        let current = self
            .store
            .hierarchy()
            .active_lease(workspace_id)
            .map_err(store)?
            .ok_or_else(|| {
                ProtoError::not_found("active workspace write lease", lease_id.as_str())
            })?;
        if current.id != *lease_id || current.generation != expected_generation {
            return Err(ProtoError::new(
                ErrorCode::Conflict,
                "The write lease changed; refresh it before releasing",
            )
            .with_context(ProtoErrorContext::StaleLeaseGeneration {
                workspace_id: workspace_id.clone(),
                lease_id: lease_id.clone(),
                expected_generation,
                actual_generation: current.generation,
            }));
        }
        if self
            .sessions
            .get(&current.session_id)
            .is_some_and(|session| session.tree.iter().any(|node| node.is_running()))
        {
            return Err(ProtoError::new(
                ErrorCode::Conflict,
                "Stop the Session processes before releasing its write lease",
            ));
        }
        let released = self
            .store
            .hierarchy()
            .release_write_lease(lease_id, expected_generation, now_ms)
            .map_err(store)?;
        if !released {
            return Err(ProtoError::new(
                ErrorCode::Conflict,
                "The write lease was no longer active",
            ));
        }
        if let Some(session) = self.sessions.get_mut(&current.session_id) {
            session.mode = SessionMode::ReadOnly;
            session.read_only_enforced = false;
        }
        self.persist_session(&current.session_id)?;
        self.bump_hierarchy();
        self.push_workspace_lease(workspace_id, None, now_ms);
        Ok(Response::WorkspaceWriteLease {
            workspace_id: workspace_id.clone(),
            lease: None,
        })
    }

    pub(super) fn get_preview_history(
        &self,
        session_id: &SessionId,
        node_id: &NodeId,
        limit: Option<u16>,
    ) -> Answer {
        let session = self.session(session_id)?;
        if session.tree.get(node_id).is_none() {
            return Err(ProtoError::not_found("process node", node_id.as_str()));
        }
        let mut entries = self
            .store
            .hierarchy()
            .preview_history(node_id, usize::from(limit.unwrap_or(8)))
            .map_err(store)?;
        entries.reverse();
        Ok(Response::PreviewHistory {
            session_id: session_id.clone(),
            node_id: node_id.clone(),
            entries,
        })
    }

    pub(super) fn open_node_as_temporary_pane(
        &mut self,
        surface_id: String,
        session_id: &SessionId,
        node_id: &NodeId,
        now_ms: i64,
    ) -> Answer {
        let surface_id = validate_surface_id(&surface_id)?;
        let session = self.session(session_id)?;
        if session.tree.get(node_id).is_none() {
            return Err(ProtoError::not_found("process node", node_id.as_str()));
        }

        // A surface owns at most one temporary Pane. Replacing it changes only
        // surface-scoped bindings; saved Layout and Process lifetime are untouched.
        let session_ids: Vec<SessionId> = self.sessions.keys().cloned().collect();
        for owner in session_ids {
            let bindings = self
                .store
                .hierarchy()
                .bindings_for_session(&owner)
                .map_err(store)?;
            for binding in bindings.into_iter().filter(|binding| {
                binding.temporary && binding.surface_id.as_deref() == Some(surface_id.as_str())
            }) {
                self.detach_everyone(&binding.session_id, &binding.pane_id);
                self.store
                    .hierarchy()
                    .unbind_pane(&binding.session_id, &binding.pane_id)
                    .map_err(store)?;
            }
        }

        let binding = PaneNodeBinding {
            pane_id: PaneId::new(),
            session_id: session_id.clone(),
            node_id: node_id.clone(),
            temporary: true,
            surface_id: Some(surface_id),
            opened_ms: now_ms,
        };
        self.store.hierarchy().bind_pane(&binding).map_err(store)?;
        let capability = self.node_pane_capability(node_id);
        self.bump_hierarchy();
        self.push_pane_bindings(session_id, node_id, now_ms);
        Ok(Response::NodePane {
            pane: NodePaneView {
                binding,
                capability,
            },
        })
    }

    pub(super) fn focus_pane_for_node(
        &self,
        surface_id: String,
        session_id: &SessionId,
        node_id: &NodeId,
    ) -> Answer {
        let surface_id = validate_surface_id(&surface_id)?;
        let session = self.session(session_id)?;
        if session.tree.get(node_id).is_none() {
            return Err(ProtoError::not_found("process node", node_id.as_str()));
        }
        let bindings = self
            .store
            .hierarchy()
            .bindings_for_session(session_id)
            .map_err(store)?;
        let focus = bindings
            .into_iter()
            .filter(|binding| binding.node_id == *node_id)
            .find(|binding| {
                !binding.temporary || binding.surface_id.as_deref() == Some(surface_id.as_str())
            })
            .map(|binding| PaneFocusView {
                surface_id,
                session_id: session_id.clone(),
                node_id: node_id.clone(),
                pane_id: binding.pane_id,
            });
        Ok(Response::PaneFocus { focus })
    }

    pub(crate) fn hierarchy_snapshot(
        &self,
        surface_id: &str,
        include_archived: bool,
        now_ms: i64,
    ) -> Result<HierarchySnapshot, ProtoError> {
        let summaries = self.session_summaries(None, include_archived, now_ms);
        let mut branches = Vec::new();
        let mut workspaces: Vec<_> = self
            .workspaces
            .values()
            .filter(|workspace| include_archived || !workspace.archived)
            .collect();
        workspaces.sort_by(|a, b| {
            b.last_used_ms
                .cmp(&a.last_used_ms)
                .then_with(|| a.name.cmp(&b.name))
        });
        for workspace in workspaces {
            let workspace_summary =
                turn_proto::WorkspaceSummary::from_workspace(workspace, &summaries);
            let mut sessions: Vec<_> = self
                .sessions
                .values()
                .filter(|session| session.workspace_id == workspace.id)
                .filter(|session| include_archived || !session.is_archived())
                .collect();
            // Stable while activity changes: navigation does not jump under the
            // pointer merely because a Process emitted Attention.
            sessions.sort_by(|a, b| {
                b.pinned
                    .cmp(&a.pinned)
                    .then_with(|| a.sort_key.cmp(&b.sort_key))
                    .then_with(|| a.created_ms.cmp(&b.created_ms))
                    .then_with(|| a.name.cmp(&b.name))
            });
            let mut session_views = Vec::with_capacity(sessions.len());
            for session in sessions {
                let bindings = self
                    .store
                    .hierarchy()
                    .bindings_for_session(&session.id)
                    .map_err(store)?;
                let capabilities: HashMap<NodeId, NodePaneCapability> = session
                    .tree
                    .iter()
                    .map(|node| (node.id.clone(), self.node_pane_capability(&node.id)))
                    .collect();
                let summary = summaries
                    .iter()
                    .find(|summary| summary.id == session.id)
                    .cloned()
                    .unwrap_or_else(|| {
                        turn_proto::SessionSummary::from_session(session, 0, false, now_ms)
                    });
                session_views.push(SessionTreeView {
                    session: summary,
                    nodes: TreeNodeView::for_session_with_panes(
                        session,
                        &bindings,
                        &capabilities,
                        now_ms,
                    ),
                });
            }
            let checkouts = self
                .store
                .hierarchy()
                .checkouts_for_workspace(&workspace.id)
                .map_err(store)?;
            let write_lease = self
                .store
                .hierarchy()
                .active_lease(&workspace.id)
                .map_err(store)?;
            branches.push(WorkspaceTreeView {
                workspace: workspace_summary,
                checkouts,
                write_lease,
                sessions: session_views,
            });
        }
        Ok(HierarchySnapshot {
            revision: self.hierarchy_revision,
            tree_state: self.tree_surface_state(surface_id)?,
            workspaces: branches,
        })
    }

    pub(crate) fn bump_hierarchy(&mut self) {
        self.hierarchy_revision = self.hierarchy_revision.saturating_add(1);
    }

    pub(crate) fn node_pane_capability(&self, node_id: &NodeId) -> NodePaneCapability {
        if self.processes.contains_key(node_id) {
            NodePaneCapability::Terminal {
                streams: vec![PaneStream::Cells, PaneStream::Bytes],
            }
        } else {
            NodePaneCapability::PreviewDetails
        }
    }

    pub(crate) fn heartbeat_workspace_leases(&mut self, now_ms: i64) {
        if now_ms - self.last_lease_heartbeat_ms < LEASE_HEARTBEAT_INTERVAL_MS {
            return;
        }
        self.last_lease_heartbeat_ms = now_ms;
        for workspace in self.workspaces.keys() {
            let Ok(Some(lease)) = self.store.hierarchy().active_lease(workspace) else {
                continue;
            };
            if self.sessions.contains_key(&lease.session_id) {
                if let Err(error) =
                    self.store
                        .hierarchy()
                        .heartbeat_lease(&lease.id, lease.generation, now_ms)
                {
                    tracing::warn!(%error, lease = %lease.id, "could not heartbeat write lease");
                }
            }
        }
    }

    fn tree_surface_state(&self, surface_id: &str) -> Result<TreeSurfaceState, ProtoError> {
        let rows = self
            .store
            .hierarchy()
            .tree_state(surface_id)
            .map_err(store)?;
        let mut expanded = Vec::new();
        let mut selected = None;
        for row in rows {
            let key = state_key(row.node_kind, &row.node_id);
            if !self.hierarchy_key_exists(&key) {
                continue;
            }
            if row.expanded {
                expanded.push(key.clone());
            }
            if row.selected {
                selected = Some(key);
            }
        }
        Ok(TreeSurfaceState {
            surface_id: surface_id.to_string(),
            selected,
            expanded,
        })
    }

    fn require_hierarchy_key(&self, key: &HierarchyKey) -> Result<(), ProtoError> {
        if self.hierarchy_key_exists(key) {
            Ok(())
        } else {
            Err(ProtoError::not_found("hierarchy node", &key_id(key)))
        }
    }

    fn hierarchy_key_exists(&self, key: &HierarchyKey) -> bool {
        match key {
            HierarchyKey::Workspace { workspace_id } => self.workspaces.contains_key(workspace_id),
            HierarchyKey::Session { session_id } => self.sessions.contains_key(session_id),
            HierarchyKey::Process { node_id } => self
                .sessions
                .values()
                .any(|session| session.tree.get(node_id).is_some()),
        }
    }

    pub(crate) fn map_lease_store_error(
        &self,
        workspace_id: &WorkspaceId,
        requesting_session_id: Option<&SessionId>,
        error: turn_store::StoreError,
    ) -> ProtoError {
        if !matches!(error, turn_store::StoreError::WriteLeaseHeld { .. }) {
            return store(error);
        }
        let Ok(Some(lease)) = self.store.hierarchy().active_lease(workspace_id) else {
            return store(error);
        };
        let Some(owner) = self.sessions.get(&lease.session_id) else {
            return store(error);
        };
        ProtoError::workspace_write_lease_conflict(ProtoErrorContext::WorkspaceWriteLeaseConflict {
            workspace_id: workspace_id.clone(),
            checkout_id: lease.checkout_id.clone(),
            requesting_session_id: requesting_session_id.cloned(),
            lease: Box::new(lease),
            owner: Box::new(WriteLeaseOwnerView {
                session_id: owner.id.clone(),
                session_name: owner.name.clone(),
                mode: owner.mode,
                cwd: owner.cwd.clone(),
                branch: owner.git_branch.clone(),
                last_activity_ms: owner.last_activity_ms,
            }),
            alternatives: vec![
                SessionConflictAlternative::FocusOwner,
                SessionConflictAlternative::CreateReadOnly,
                SessionConflictAlternative::CreateIsolatedWorktree,
                SessionConflictAlternative::Cancel,
            ],
        })
    }
}

fn validate_surface_id(raw: &str) -> Result<String, ProtoError> {
    let id = raw.trim();
    if id.is_empty() || id.chars().count() > MAX_SURFACE_ID_CHARS {
        return Err(ProtoError::invalid(
            "A surface id must contain between 1 and 128 characters",
        ));
    }
    Ok(id.to_string())
}

fn key_parts(key: &HierarchyKey) -> (HierarchyNodeKind, String) {
    match key {
        HierarchyKey::Workspace { workspace_id } => {
            (HierarchyNodeKind::Workspace, workspace_id.to_string())
        }
        HierarchyKey::Session { session_id } => {
            (HierarchyNodeKind::Session, session_id.to_string())
        }
        HierarchyKey::Process { node_id } => (HierarchyNodeKind::Process, node_id.to_string()),
    }
}

fn key_id(key: &HierarchyKey) -> String {
    key_parts(key).1
}

fn state_key(kind: HierarchyNodeKind, id: &str) -> HierarchyKey {
    match kind {
        HierarchyNodeKind::Workspace => {
            HierarchyKey::workspace(WorkspaceId::from_stored(id.to_string()))
        }
        HierarchyNodeKind::Session => HierarchyKey::session(SessionId::from_stored(id.to_string())),
        HierarchyNodeKind::Process => HierarchyKey::process(NodeId::from_stored(id.to_string())),
    }
}
