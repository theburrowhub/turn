//! Unified navigation, checkout leases, activity previews and node views.
//!
//! These handlers deliberately keep four identities separate: selecting a tree
//! node, focusing a Pane, resolving Attention and owning a write lease are four
//! different operations. None of the handlers below performs one as a side
//! effect of another.

use std::collections::{HashMap, HashSet};

use super::workspaces::store;
use super::Answer;
use crate::core::Core;
use turn_core::ids::{CheckoutId, LeaseId, NodeId, PaneId, SessionId, WorkspaceId};
use turn_core::model::{
    HierarchyNodeKind, LeaseState, PaneNodeBinding, PreviewVisibility, ProcessNode,
    RelationshipKind, SessionMode, TreeUiState, WorkspaceWriteLease,
};
use turn_proto::{
    ErrorCode, HierarchyKey, HierarchySnapshot, NodePaneCapability, NodePaneView, PaneFocusView,
    PaneStream, ProtoError, ProtoErrorContext, Response, SessionConflictAlternative,
    SessionTreeView, TreeNodeView, TreeSurfaceState, WorkspaceTreeView, WriteLeaseOwnerView,
};

const MAX_SURFACE_ID_CHARS: usize = 128;
const LEASE_HEARTBEAT_INTERVAL_MS: i64 = 5_000;

impl Core {
    pub(super) fn require_client_surface(
        &self,
        client_id: super::ClientId,
        surface_id: &str,
    ) -> Result<(), ProtoError> {
        let expected = self
            .clients
            .get(&client_id)
            .and_then(|client| client.surface_id.as_deref());
        if expected == Some(surface_id) {
            Ok(())
        } else {
            Err(ProtoError::refused(
                "This connection does not own the requested UI surface; refresh its hierarchy first",
            ))
        }
    }

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
        let previous_surface = self
            .clients
            .get(&client_id)
            .ok_or_else(|| ProtoError::internal("this connection is not registered"))?
            .surface_id
            .clone();
        if let Some(previous) = previous_surface.as_deref() {
            if previous != surface_id {
                return Err(ProtoError::refused(
                    "A connected client cannot change its UI surface identity",
                ));
            }
        } else {
            // A first claim is a UI bootstrap, including a reconnect that overlaps
            // the final moments of the old socket. A temporary Pane cannot be
            // rehydrated from a binding alone, so retire the old surface owner and
            // its ephemeral binding before building the new snapshot.
            let displaced: Vec<_> = self
                .clients
                .iter()
                .filter_map(|(id, client)| {
                    (*id != client_id && client.surface_id.as_deref() == Some(surface_id.as_str()))
                        .then_some(*id)
                })
                .collect();
            for id in displaced {
                let watched_nodes = self
                    .clients
                    .get_mut(&id)
                    .map(|client| {
                        client.surface_id = None;
                        std::mem::take(&mut client.attachments)
                            .into_values()
                            .filter_map(|attachment| attachment.node_id)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                for node in watched_nodes {
                    self.stop_pump_if_unwatched(&node);
                }
            }
            let pruned = self
                .store
                .hierarchy()
                .clear_temporary_bindings_for_surface(&surface_id)
                .map_err(store)?;
            if pruned > 0 {
                self.bump_hierarchy();
            }
            self.clients
                .get_mut(&client_id)
                .expect("the registered client cannot disappear during dispatch")
                .surface_id = Some(surface_id.clone());
        }
        self.clients
            .get_mut(&client_id)
            .expect("the registered client cannot disappear while projecting hierarchy")
            .include_archived = include_archived;
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
        let workspace = self.workspace(workspace_id)?;
        if workspace.archived {
            return Err(ProtoError::refused(
                "Unarchive the Workspace before acquiring its write lease",
            ));
        }
        let legacy_reconciliation_required = workspace.lease_reconciliation_required;
        let session = self.session(session_id)?;
        if session.is_archived() {
            return Err(ProtoError::refused(
                "Unarchive the Session before acquiring a write lease",
            ));
        }
        if &session.workspace_id != workspace_id {
            return Err(ProtoError::invalid(
                "The Session does not belong to that Workspace",
            ));
        }
        if &session.checkout_id != checkout_id {
            return Err(ProtoError::invalid(
                "The Session is not assigned to the requested checkout",
            ));
        }
        if session.mode == SessionMode::ReadOnly
            && session.tree.iter().any(|node| node.is_running())
        {
            return Err(ProtoError::new(
                ErrorCode::Conflict,
                "End every read-only process before acquiring exclusive write access",
            ));
        }
        let current = self
            .store
            .hierarchy()
            .active_lease(workspace_id)
            .map_err(store)?;
        let mut checkout_lock = None;
        let lease = match current {
            Some(current)
                if current.state == LeaseState::RecoveryRequired
                    && &current.session_id == session_id
                    && &current.checkout_id == checkout_id =>
            {
                if legacy_reconciliation_required {
                    return Err(ProtoError::new(
                        ErrorCode::Conflict,
                        "This legacy Workspace needs checkout reconciliation before its write lease can be recovered",
                    ));
                }
                // The recovery prompt explicitly tells the user to stop an unreachable
                // process outside Turn. Reconcile the OS process table at the moment they
                // confirm so an already-dead orphan does not block this daemon generation
                // forever. Command matching also protects against PID reuse.
                self.reconcile_orphaned_recovery(session_id, now_ms)?;
                let session = self.session(session_id)?;
                if session.tree.iter().any(|node| node.is_running()) {
                    return Err(ProtoError::new(
                        ErrorCode::Conflict,
                        "A process from the previous daemon is still running; stop it outside Turn before confirming write access",
                    ));
                }
                let checkout = self
                    .store
                    .hierarchy()
                    .checkout(workspace_id, checkout_id)
                    .map_err(store)?
                    .ok_or_else(|| {
                        ProtoError::not_found("workspace checkout", checkout_id.as_str())
                    })?;
                let canonical = std::fs::canonicalize(&checkout.path)
                    .map_err(|error| {
                        ProtoError::refused(
                            "Turn cannot verify the primary checkout before recovering write access",
                        )
                        .with_detail(error.to_string())
                    })?
                    .to_string_lossy()
                    .into_owned();
                if canonical != checkout.canonical_path {
                    return Err(ProtoError::new(
                        ErrorCode::Conflict,
                        "The primary checkout now resolves to a different directory; write access was not recovered",
                    ));
                }
                let mut writer = session.clone();
                writer.mode = SessionMode::MainCheckout;
                writer.read_only_enforced = false;
                let mut claim = current.clone();
                claim.state = LeaseState::Active;
                claim.heartbeat_ms = now_ms;
                let lock = self.checkout_lock_claim(&writer, &claim)?;
                let reclaimed = self
                    .store
                    .hierarchy()
                    .reclaim_write_lease(
                        workspace_id,
                        session_id,
                        checkout_id,
                        &current.id,
                        current.generation,
                        now_ms,
                    )
                    .map_err(store)?
                    .ok_or_else(|| {
                        ProtoError::new(
                            ErrorCode::Conflict,
                            "The recovery lease changed; refresh before confirming write access",
                        )
                    })?;
                checkout_lock = Some(lock);
                reclaimed
            }
            Some(current)
                if current.state == LeaseState::Active
                    && &current.session_id == session_id
                    && &current.checkout_id == checkout_id =>
            {
                self.require_checkout_write_lock(self.session(session_id)?, &current)?;
                current
            }
            Some(held) => {
                return Err(self.local_lease_conflict(workspace_id, Some(session_id), held));
            }
            None => {
                let mut writer = self.session(session_id)?.clone();
                writer.mode = SessionMode::MainCheckout;
                writer.worktree_path = None;
                writer.read_only_enforced = false;
                let claim = WorkspaceWriteLease::active(
                    workspace_id.clone(),
                    session_id.clone(),
                    checkout_id.clone(),
                    now_ms,
                );
                let lock = self.checkout_lock_claim(&writer, &claim)?;
                let acquired = self
                    .store
                    .hierarchy()
                    .acquire_write_lease_with_id(
                        workspace_id,
                        session_id,
                        checkout_id,
                        Some(&claim.id),
                        now_ms,
                    )
                    .map_err(|error| {
                        self.map_lease_store_error(workspace_id, Some(session_id), error)
                    })?;
                checkout_lock = Some(lock);
                acquired
            }
        };
        let session = self.session_mut(session_id)?;
        session.mode = SessionMode::MainCheckout;
        session.checkout_id = checkout_id.clone();
        session.worktree_path = None;
        session.read_only_enforced = false;
        if let Some(lock) = checkout_lock {
            self.install_checkout_write_lock(session_id, &lease, lock);
        }
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
        let owner = self
            .sessions
            .get(&current.session_id)
            .cloned()
            .ok_or_else(|| {
                // Another daemon may have created the owner after this Core loaded.
                // Absence from this process is uncertainty, never proof that it is
                // safe to revoke a live/recovery claim.
                ProtoError::new(
                    ErrorCode::Conflict,
                    "The lease owner is not loaded here; refresh before releasing it",
                )
            })?;
        if owner.tree.iter().any(|node| node.is_running()) {
            return Err(ProtoError::new(
                ErrorCode::Conflict,
                "Stop the Session processes before releasing its write lease",
            ));
        }
        if current.state == LeaseState::Active {
            self.require_checkout_write_lock(&owner, &current)?;
        }
        let mut read_only_owner = owner;
        read_only_owner.mode = SessionMode::ReadOnly;
        read_only_owner.worktree_path = None;
        let read_only_enforced = match self.read_only_sandbox(&read_only_owner) {
            Ok(sandbox) => sandbox.is_some(),
            Err(error) => {
                tracing::warn!(
                    session_id = %read_only_owner.id,
                    %error,
                    "released Session will remain read-only with process launch disabled"
                );
                false
            }
        };
        let released = self
            .store
            .hierarchy()
            .release_write_lease_and_assign_read_only(
                lease_id,
                expected_generation,
                read_only_enforced,
                now_ms,
            )
            .map_err(store)?;
        if !released {
            return Err(ProtoError::new(
                ErrorCode::Conflict,
                "The write lease was no longer active",
            ));
        }
        if let Some(session) = self.sessions.get_mut(&current.session_id) {
            session.mode = SessionMode::ReadOnly;
            session.worktree_path = None;
            session.read_only_enforced = read_only_enforced;
        }
        // SQLite is demoted first, so the only release gap is conservative: the
        // kernel may still reject another writer for a moment, never admit two.
        self.drop_checkout_write_lock(lease_id);
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
        let Some(node) = session.tree.get(node_id) else {
            return Err(ProtoError::not_found("process node", node_id.as_str()));
        };
        if node.preview_visibility == turn_core::model::PreviewVisibility::Hide {
            return Ok(Response::PreviewHistory {
                session_id: session_id.clone(),
                node_id: node_id.clone(),
                entries: Vec::new(),
            });
        }
        let entries = self
            .store
            .hierarchy()
            .preview_history(node_id, usize::from(limit.unwrap_or(8)))
            .map_err(store)?;
        Ok(Response::PreviewHistory {
            session_id: session_id.clone(),
            node_id: node_id.clone(),
            entries,
        })
    }

    pub(super) fn set_preview_visibility(
        &mut self,
        session_id: &SessionId,
        node_id: &NodeId,
        visibility: PreviewVisibility,
        now_ms: i64,
    ) -> Answer {
        let session = self.session_mut(session_id)?;
        let Some(node) = session.tree.get_mut(node_id) else {
            return Err(ProtoError::not_found("process node", node_id.as_str()));
        };
        node.preview_visibility = visibility;
        self.persist_session(session_id)?;
        self.push_tree(session_id, now_ms);
        Ok(Response::Ack)
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
                attention_subject_node_id: None,
            });
        Ok(Response::PaneFocus { focus })
    }

    /// Focuses the existing Pane that can service an exact semantic Attention
    /// subject without changing who the demand belongs to.
    ///
    /// A hook-declared subagent can be a real AgentNode while sharing its parent's
    /// PTY. In that case the tree selection must stay on the child, while keyboard
    /// focus goes to the nearest trusted runtime-owning ancestor with a Pane on this
    /// surface. We never walk across provisional/merely-related edges, skip over a
    /// distinct runtime, or create a Pane as a side effect of Attention.
    pub(super) fn focus_pane_for_attention(
        &self,
        surface_id: String,
        session_id: &SessionId,
        subject_node_id: &NodeId,
    ) -> Answer {
        let surface_id = validate_surface_id(&surface_id)?;
        let session = self.session(session_id)?;
        let subject = session
            .tree
            .get(subject_node_id)
            .ok_or_else(|| ProtoError::not_found("process node", subject_node_id.as_str()))?;
        let bindings = self
            .store
            .hierarchy()
            .bindings_for_session(session_id)
            .map_err(store)?;

        let focus_binding = if node_has_distinct_runtime(self, subject) {
            // A node with its own runtime is its own input boundary. Its exact Pane
            // may be focused, but a missing Pane never licenses routing to its parent.
            visible_binding_for_node(&bindings, &surface_id, subject_node_id)
        } else {
            // A PreviewDetails binding is an honest view of a semantic Agent, not an
            // input channel. Ignore it for Attention routing and find the runtime
            // that actually owns the shared PTY.
            (|| {
                let mut cursor = subject;
                let mut visited = HashSet::from([subject_node_id.clone()]);
                loop {
                    if !relationship_routes_to_runtime_owner(cursor) {
                        return None;
                    }
                    let parent_id = cursor.parent.as_ref()?;
                    if !visited.insert(parent_id.clone()) {
                        return None;
                    }
                    let parent = session.tree.get(parent_id)?;
                    if node_has_distinct_runtime(self, parent) {
                        // This is the authentic input boundary. With no Pane on this
                        // surface there is nowhere safe to focus, and walking past it
                        // would type into a different Agent.
                        return visible_binding_for_node(&bindings, &surface_id, parent_id);
                    }
                    cursor = parent;
                }
            })()
        };

        let focus = focus_binding.map(|binding| PaneFocusView {
            surface_id,
            session_id: session_id.clone(),
            node_id: binding.node_id,
            pane_id: binding.pane_id,
            attention_subject_node_id: Some(subject_node_id.clone()),
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
                let bindings: Vec<_> = self
                    .store
                    .hierarchy()
                    .bindings_for_session(&session.id)
                    .map_err(store)?
                    .into_iter()
                    .filter(|binding| {
                        !binding.temporary || binding.surface_id.as_deref() == Some(surface_id)
                    })
                    .collect();
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
        if self.terminal_node(node_id).is_some() {
            NodePaneCapability::Terminal {
                streams: vec![PaneStream::Cells, PaneStream::Bytes],
            }
        } else {
            NodePaneCapability::PreviewDetails
        }
    }

    /// The agent this node's process is running, if it is a shell hosting one.
    pub(crate) fn hosted_agent_of(&self, node_id: &NodeId) -> Option<NodeId> {
        self.processes.get(node_id)?.hosted.clone()
    }

    /// Whether this node is a command Turn started inside a terminal it still owns.
    ///
    /// It is the answer to "can Turn end this?" for a node with no pty of its own:
    /// closing the terminal it runs in ends it, so it is not a process that has escaped
    /// Turn's reach the way a survivor of a previous daemon has.
    pub(crate) fn is_hosted(&self, node_id: &NodeId) -> bool {
        self.processes
            .values()
            .any(|process| process.hosted.as_ref() == Some(node_id))
    }

    /// The node whose terminal shows this node's work, if there is one.
    ///
    /// Itself, when Turn holds its pty. Otherwise the shell it was started in: an agent
    /// hosted in a pane's shell draws on that shell's screen, and that screen is the
    /// agent's terminal in every sense a user cares about. Resolving it here is what
    /// keeps "open this agent in a pane" working now that the pty belongs to the shell
    /// around the agent rather than to the agent itself.
    pub(crate) fn terminal_node(&self, node_id: &NodeId) -> Option<NodeId> {
        if self.processes.contains_key(node_id) {
            return Some(node_id.clone());
        }
        self.processes
            .iter()
            .find(|(_, process)| process.hosted.as_ref() == Some(node_id))
            .map(|(shell, _)| shell.clone())
    }

    pub(crate) fn heartbeat_workspace_leases(&mut self, now_ms: i64) {
        if now_ms - self.last_lease_heartbeat_ms < LEASE_HEARTBEAT_INTERVAL_MS {
            return;
        }
        self.last_lease_heartbeat_ms = now_ms;
        let workspaces: Vec<_> = self.workspaces.keys().cloned().collect();
        for workspace in workspaces {
            let Ok(Some(lease)) = self.store.hierarchy().active_lease(&workspace) else {
                continue;
            };
            let Some(session) = self.sessions.get(&lease.session_id).cloned() else {
                continue;
            };
            if lease.state != LeaseState::Active {
                continue;
            }
            if let Err(error) = self.require_checkout_write_lock(&session, &lease) {
                tracing::warn!(%error, lease = %lease.id, "refused to heartbeat a lease without host checkout authority");
                continue;
            }
            match self
                .store
                .hierarchy()
                .heartbeat_lease(&lease.id, lease.generation, now_ms)
            {
                Ok(true) => self.heartbeat_checkout_lock_owner(&lease, now_ms),
                Ok(false) => {
                    tracing::warn!(lease = %lease.id, "write lease changed before heartbeat")
                }
                Err(error) => {
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
        match &error {
            turn_store::StoreError::ArchivedWorkspace { .. } => {
                return ProtoError::refused(
                    "Unarchive the Workspace before acquiring its write lease",
                )
                .with_detail(error.to_string());
            }
            turn_store::StoreError::ArchivedSession { .. } => {
                return ProtoError::refused("Unarchive the Session before acquiring a write lease")
                    .with_detail(error.to_string());
            }
            turn_store::StoreError::LeaseReconciliationRequired { .. } => {
                return ProtoError::refused(
                    "The primary checkout requires explicit write-lease reconciliation",
                )
                .with_detail(error.to_string());
            }
            turn_store::StoreError::WorkspaceRoot { .. }
            | turn_store::StoreError::CheckoutPath { .. } => {
                return ProtoError::refused(
                    "Turn cannot prove the primary checkout filesystem identity",
                )
                .with_detail(error.to_string());
            }
            _ => {}
        }
        let lease_id = match &error {
            turn_store::StoreError::WriteLeaseHeld { lease_id, .. } => {
                LeaseId::from_stored(lease_id.clone())
            }
            _ => return store(error),
        };
        // The writer may belong to another Workspace record that aliases the same
        // canonical checkout. The conflict names the globally unique lease; filtering
        // by the requesting Workspace would lose the owner and degrade to a string.
        let Ok(Some(lease)) = self.store.hierarchy().lease(&lease_id) else {
            return store(error);
        };
        self.local_lease_conflict(workspace_id, requesting_session_id, lease)
    }

    pub(crate) fn local_lease_conflict(
        &self,
        workspace_id: &WorkspaceId,
        requesting_session_id: Option<&SessionId>,
        lease: WorkspaceWriteLease,
    ) -> ProtoError {
        // A competing daemon can create the owner after this Core's in-memory
        // snapshot. Fall back to durable state so the typed conflict never
        // degrades to a generic storage outage.
        let owner = match self.sessions.get(&lease.session_id).cloned() {
            Some(owner) => owner,
            None => match self.store.sessions().get(&lease.session_id) {
                Ok(Some(owner)) => owner,
                _ => {
                    return ProtoError::new(
                        ErrorCode::Unavailable,
                        "Turn could not load the local checkout lease owner",
                    )
                }
            },
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

fn visible_binding_for_node(
    bindings: &[PaneNodeBinding],
    surface_id: &str,
    node_id: &NodeId,
) -> Option<PaneNodeBinding> {
    bindings
        .iter()
        .find(|binding| {
            binding.node_id == *node_id
                && (!binding.temporary || binding.surface_id.as_deref() == Some(surface_id))
        })
        .cloned()
}

/// Whether this node is its own input boundary.
///
/// Owning a pty makes it one. An agent Turn started inside a pane's shell does not: it
/// has a pid of its own but reads from the shell's tty, so the shell is where typing
/// goes and routing has to continue to it. Answering "yes" for a shared runtime would
/// look for a Pane the agent does not have and give up, leaving a demand nothing can
/// take the user to.
fn node_has_distinct_runtime(core: &Core, node: &ProcessNode) -> bool {
    match core.terminal_node(&node.id) {
        Some(terminal) => terminal == node.id,
        None => node.pid.is_some(),
    }
}

fn relationship_routes_to_runtime_owner(node: &ProcessNode) -> bool {
    matches!(
        node.relationship.kind,
        RelationshipKind::SpawnedBy | RelationshipKind::OwnsProcess
    ) && node.relationship.confidence.may_steal_focus()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::testing::Harness;
    use turn_core::event::{Confidence, EventKind, EventSource, TurnEvent};
    use turn_core::model::{
        Layout, Pane, PaneKind, PreviewVisibility, ProcessNode, Session, WorkspaceCheckout,
    };
    use turn_core::state::Lifecycle;
    use turn_proto::{CloseDisposition, NewPane, Request, ServerEvent, ServerMessage};

    const NOW: i64 = 1_775_000_000_000;

    fn drain_events(
        frames: &mut tokio::sync::mpsc::Receiver<turn_proto::ServerFrame>,
    ) -> Vec<ServerEvent> {
        let mut events = Vec::new();
        while let Ok(frame) = frames.try_recv() {
            if let ServerMessage::Event { event } = frame.message {
                events.push(event);
            }
        }
        events
    }

    fn assert_surface_binding_replacement(
        events: &[ServerEvent],
        session_id: &SessionId,
        node_id: &NodeId,
        expected: &[PaneId],
        forbidden: &[PaneId],
    ) {
        let (partial_index, revision, bindings) = events
            .iter()
            .enumerate()
            .find_map(|(index, event)| match event {
                ServerEvent::PaneBindingsChanged {
                    hierarchy_revision,
                    session_id: changed_session,
                    node_id: changed_node,
                    bindings,
                } if changed_session == session_id && changed_node == node_id => {
                    Some((index, *hierarchy_revision, bindings))
                }
                _ => None,
            })
            .expect("the surface must receive a binding replacement");
        let ids: Vec<_> = bindings
            .iter()
            .map(|binding| binding.pane_id.clone())
            .collect();
        assert_eq!(ids, expected, "the partial replacement leaked a Pane");
        assert!(bindings
            .iter()
            .all(|binding| { forbidden.iter().all(|pane_id| binding.pane_id != *pane_id) }));

        let (snapshot_index, snapshot) = events
            .iter()
            .enumerate()
            .find_map(|(index, event)| match event {
                ServerEvent::HierarchyChanged { snapshot } if snapshot.revision == revision => {
                    Some((index, snapshot))
                }
                _ => None,
            })
            .expect("the same revision must also have a full surface projection");
        assert!(
            partial_index < snapshot_index,
            "this order reproduces the equal-revision client path"
        );
        let snapshot_bindings: Vec<_> = snapshot
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.sessions)
            .find(|session| session.session.id == *session_id)
            .and_then(|session| session.nodes.iter().find(|node| node.node_id == *node_id))
            .expect("the node must remain in the hierarchy")
            .pane_bindings
            .iter()
            .map(|binding| binding.pane_id.clone())
            .collect();
        assert_eq!(snapshot_bindings, expected);
    }

    #[tokio::test]
    async fn temporary_binding_pushes_are_filtered_before_each_surface_applies_them() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_surface_isolation");
        harness.add_session(session_id.clone(), PaneId::new(), NOW);
        let mut reviewer = ProcessNode::agent(
            session_id.clone(),
            "claude",
            harness._dir.path().to_string_lossy(),
            NOW,
        );
        reviewer.title = "Reviewer".into();
        reviewer.lifecycle = Lifecycle::Alive;
        let reviewer_id = reviewer.id.clone();
        harness
            .core
            .sessions
            .get_mut(&session_id)
            .unwrap()
            .tree
            .insert(reviewer);
        harness.core.persist_session(&session_id).unwrap();

        let (left_client, mut left_frames) = harness.add_client(32);
        let (right_client, mut right_frames) = harness.add_client(32);
        for (client, surface_id) in [(left_client, "left-window"), (right_client, "right-window")] {
            assert!(matches!(
                harness
                    .core
                    .dispatch(
                        client,
                        Request::GetHierarchy {
                            surface_id: surface_id.into(),
                            include_archived: false,
                        },
                        NOW + 1,
                    )
                    .unwrap(),
                Response::Hierarchy { .. }
            ));
        }
        drain_events(&mut left_frames);
        drain_events(&mut right_frames);

        let left_pane = match harness
            .core
            .dispatch(
                left_client,
                Request::OpenNodeAsTemporaryPane {
                    surface_id: "left-window".into(),
                    session_id: session_id.clone(),
                    node_id: reviewer_id.clone(),
                },
                NOW + 2,
            )
            .unwrap()
        {
            Response::NodePane { pane } => pane.binding.pane_id,
            other => panic!("unexpected {other:?}"),
        };
        assert_surface_binding_replacement(
            &drain_events(&mut left_frames),
            &session_id,
            &reviewer_id,
            std::slice::from_ref(&left_pane),
            &[],
        );
        assert_surface_binding_replacement(
            &drain_events(&mut right_frames),
            &session_id,
            &reviewer_id,
            &[],
            std::slice::from_ref(&left_pane),
        );

        let right_pane = match harness
            .core
            .dispatch(
                right_client,
                Request::OpenNodeAsTemporaryPane {
                    surface_id: "right-window".into(),
                    session_id: session_id.clone(),
                    node_id: reviewer_id.clone(),
                },
                NOW + 3,
            )
            .unwrap()
        {
            Response::NodePane { pane } => pane.binding.pane_id,
            other => panic!("unexpected {other:?}"),
        };
        assert_surface_binding_replacement(
            &drain_events(&mut left_frames),
            &session_id,
            &reviewer_id,
            std::slice::from_ref(&left_pane),
            std::slice::from_ref(&right_pane),
        );
        assert_surface_binding_replacement(
            &drain_events(&mut right_frames),
            &session_id,
            &reviewer_id,
            std::slice::from_ref(&right_pane),
            std::slice::from_ref(&left_pane),
        );
    }

    #[tokio::test]
    async fn a_core_missing_the_owner_cannot_release_another_daemons_lease() {
        let mut harness = Harness::new().await;
        let workspace_id = match harness
            .core
            .create_workspace(
                "shared".into(),
                harness._dir.path().to_string_lossy().into_owned(),
                NOW,
            )
            .unwrap()
        {
            Response::Workspace { workspace } => workspace.id,
            other => panic!("unexpected {other:?}"),
        };
        let workspace = harness.core.workspaces[&workspace_id].clone();
        let mut owner = Session::new(
            workspace_id.clone(),
            "remote owner",
            workspace.root,
            Layout::single(Pane::new(PaneKind::Agent)),
            NOW + 1,
        );
        owner.mode = SessionMode::MainCheckout;
        owner.checkout_id = CheckoutId::primary_for(&workspace_id);
        let lease = harness
            .core
            .store
            .hierarchy()
            .create_session(&owner, NOW + 1)
            .unwrap()
            .unwrap();
        assert!(!harness.core.sessions.contains_key(&owner.id));

        let error = harness
            .core
            .release_workspace_write_lease(&workspace_id, &lease.id, lease.generation, NOW + 2)
            .expect_err("a stale Core must fail closed");
        assert_eq!(error.code, ErrorCode::Conflict);
        assert_eq!(
            harness
                .core
                .store
                .hierarchy()
                .active_lease(&workspace_id)
                .unwrap()
                .unwrap()
                .id,
            lease.id
        );
    }

    #[tokio::test]
    async fn a_sqlite_lease_without_its_host_lock_is_never_heartbeated() {
        let mut harness = Harness::new().await;
        let workspace_id = match harness
            .core
            .create_workspace(
                "heartbeat authority".into(),
                harness._dir.path().to_string_lossy().into_owned(),
                NOW,
            )
            .unwrap()
        {
            Response::Workspace { workspace } => workspace.id,
            other => panic!("unexpected {other:?}"),
        };
        harness
            .core
            .create_session(
                &workspace_id,
                "writer".into(),
                None,
                Some(vec![turn_proto::NewPane::new(PaneKind::AgentTree)]),
                None,
                Vec::new(),
                NOW + 1,
            )
            .unwrap();
        let lease = harness
            .core
            .store
            .hierarchy()
            .active_lease(&workspace_id)
            .unwrap()
            .unwrap();
        harness.core.checkout_write_locks.remove(&lease.id);

        harness
            .core
            .heartbeat_workspace_leases(NOW + LEASE_HEARTBEAT_INTERVAL_MS + 2);

        let unchanged = harness
            .core
            .store
            .hierarchy()
            .active_lease(&workspace_id)
            .unwrap()
            .unwrap();
        assert_eq!(unchanged.heartbeat_ms, lease.heartbeat_ms);
    }

    #[tokio::test]
    async fn archived_sessions_and_workspaces_cannot_acquire_hidden_authority() {
        let mut harness = Harness::new().await;
        let workspace_id = match harness
            .core
            .create_workspace(
                "archived-authority".into(),
                harness._dir.path().to_string_lossy().into_owned(),
                NOW,
            )
            .unwrap()
        {
            Response::Workspace { workspace } => workspace.id,
            other => panic!("unexpected {other:?}"),
        };
        let session_id = match harness
            .core
            .create_read_only_session(
                &workspace_id,
                "reader".into(),
                None,
                Some(vec![NewPane::new(PaneKind::AgentTree)]),
                None,
                Vec::new(),
                NOW + 1,
            )
            .unwrap()
        {
            Response::Session { session } => session.id,
            other => panic!("unexpected {other:?}"),
        };
        let checkout = CheckoutId::primary_for(&workspace_id);

        harness
            .core
            .archive_session(&session_id, true, NOW + 2)
            .unwrap();
        let archived_session = harness
            .core
            .acquire_workspace_write_lease(&workspace_id, &session_id, &checkout, NOW + 3)
            .expect_err("an archived Session cannot become the hidden writer");
        assert_eq!(archived_session.code, ErrorCode::Refused);
        assert!(harness
            .core
            .store
            .hierarchy()
            .active_lease(&workspace_id)
            .unwrap()
            .is_none());

        harness
            .core
            .archive_session(&session_id, false, NOW + 4)
            .unwrap();
        harness
            .core
            .archive_workspace(&workspace_id, true, NOW + 5)
            .unwrap();
        let archived_workspace = harness
            .core
            .acquire_workspace_write_lease(&workspace_id, &session_id, &checkout, NOW + 6)
            .expect_err("an archived Workspace cannot grant write authority");
        assert_eq!(archived_workspace.code, ErrorCode::Refused);
        assert!(harness
            .core
            .store
            .hierarchy()
            .active_lease(&workspace_id)
            .unwrap()
            .is_none());
        assert_eq!(
            harness.core.sessions[&session_id].mode,
            SessionMode::ReadOnly
        );
    }

    #[tokio::test]
    async fn read_only_processes_must_end_before_explicit_write_escalation() {
        let mut harness = Harness::new().await;
        let workspace_id = match harness
            .core
            .create_workspace(
                "promotion-boundary".into(),
                harness._dir.path().to_string_lossy().into_owned(),
                NOW,
            )
            .unwrap()
        {
            Response::Workspace { workspace } => workspace.id,
            other => panic!("unexpected {other:?}"),
        };
        let session_id = match harness
            .core
            .create_read_only_session(
                &workspace_id,
                "reader".into(),
                None,
                Some(vec![NewPane::new(PaneKind::AgentTree)]),
                None,
                Vec::new(),
                NOW + 1,
            )
            .unwrap()
        {
            Response::Session { session } => session.id,
            other => panic!("unexpected {other:?}"),
        };
        let checkout = CheckoutId::primary_for(&workspace_id);
        let mut reader = ProcessNode::process(
            session_id.clone(),
            turn_core::model::NodeKind::Shell,
            "sh",
            harness._dir.path().to_string_lossy(),
            NOW + 2,
        );
        reader.lifecycle = Lifecycle::Alive;
        let reader_id = reader.id.clone();
        harness
            .core
            .sessions
            .get_mut(&session_id)
            .unwrap()
            .tree
            .insert(reader);

        let error = harness
            .core
            .acquire_workspace_write_lease(&workspace_id, &session_id, &checkout, NOW + 3)
            .expect_err("a live guarded process must not cross the authority boundary");
        assert_eq!(error.code, ErrorCode::Conflict);
        assert!(error.message.contains("End every read-only process"));
        assert!(harness
            .core
            .store
            .hierarchy()
            .active_lease(&workspace_id)
            .unwrap()
            .is_none());
        assert_eq!(
            harness.core.sessions[&session_id].mode,
            SessionMode::ReadOnly
        );

        harness
            .core
            .sessions
            .get_mut(&session_id)
            .unwrap()
            .tree
            .get_mut(&reader_id)
            .unwrap()
            .lifecycle = Lifecycle::Exited { code: 0 };
        let promoted = harness
            .core
            .acquire_workspace_write_lease(&workspace_id, &session_id, &checkout, NOW + 4)
            .expect("an ended read-only Session may be promoted explicitly");
        assert!(matches!(
            promoted,
            Response::WorkspaceWriteLease { lease: Some(_), .. }
        ));
        assert_eq!(
            harness.core.sessions[&session_id].mode,
            SessionMode::MainCheckout
        );
        assert!(!harness.core.sessions[&session_id].read_only_enforced);
    }

    #[tokio::test]
    async fn acquisition_has_no_fallible_session_persist_after_the_atomic_commit() {
        let mut harness = Harness::new().await;
        let workspace_id = match harness
            .core
            .create_workspace(
                "atomic-acquire".into(),
                harness._dir.path().to_string_lossy().into_owned(),
                NOW,
            )
            .unwrap()
        {
            Response::Workspace { workspace } => workspace.id,
            other => panic!("unexpected {other:?}"),
        };
        let session_id = match harness
            .core
            .create_read_only_session(
                &workspace_id,
                "promote me".into(),
                None,
                Some(vec![NewPane::new(PaneKind::AgentTree)]),
                None,
                Vec::new(),
                NOW + 1,
            )
            .unwrap()
        {
            Response::Session { session } => session.id,
            other => panic!("unexpected {other:?}"),
        };
        // Leave an invalid Pane-to-node reference only in memory. The lease
        // transition updates the durable Session scalar itself and should still
        // succeed. A redundant whole-Session persist after that commit would hit
        // the real foreign key, return an error after authority had changed and
        // reproduce the split-brain this test guards against.
        let pane_id = harness.core.sessions[&session_id].layout.panes()[0]
            .id
            .clone();
        harness
            .core
            .sessions
            .get_mut(&session_id)
            .unwrap()
            .layout
            .get_mut(&pane_id)
            .unwrap()
            .node_id = Some(NodeId::from_stored("proc_missing_after_lease_commit"));

        let checkout = CheckoutId::primary_for(&workspace_id);
        let response = harness
            .core
            .acquire_workspace_write_lease(&workspace_id, &session_id, &checkout, NOW + 2)
            .expect("the atomic store transition is the only durable write");
        assert!(matches!(
            response,
            Response::WorkspaceWriteLease { lease: Some(_), .. }
        ));
        assert_eq!(
            harness.core.sessions[&session_id].mode,
            SessionMode::MainCheckout
        );
        assert_eq!(
            harness
                .core
                .store
                .sessions()
                .get(&session_id)
                .unwrap()
                .unwrap()
                .mode,
            SessionMode::MainCheckout
        );
    }

    #[tokio::test]
    async fn a_worktree_session_cannot_be_promoted_with_the_primary_checkout_lease() {
        let mut harness = Harness::new().await;
        let primary_root = harness._dir.path().join("primary");
        let worktree_root = harness._dir.path().join("worktree");
        std::fs::create_dir_all(&primary_root).unwrap();
        std::fs::create_dir_all(&worktree_root).unwrap();
        let workspace_id = match harness
            .core
            .create_workspace(
                "checkout-binding".into(),
                primary_root.to_string_lossy().into_owned(),
                NOW,
            )
            .unwrap()
        {
            Response::Workspace { workspace } => workspace.id,
            other => panic!("unexpected {other:?}"),
        };
        let checkout_id = CheckoutId::new();
        let branch = "turn/isolated".to_string();
        let canonical = std::fs::canonicalize(&worktree_root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let checkout = WorkspaceCheckout {
            id: checkout_id.clone(),
            workspace_id: workspace_id.clone(),
            path: canonical.clone(),
            canonical_path: canonical.clone(),
            branch: Some(branch.clone()),
            primary: false,
            shared_resources: vec!["docker".into()],
            created_ms: NOW + 1,
        };
        let mut session = Session::new(
            workspace_id.clone(),
            "isolated",
            canonical.clone(),
            Layout::single(Pane::new(PaneKind::Agent)),
            NOW + 1,
        );
        session.mode = SessionMode::IsolatedWorktree;
        session.checkout_id = checkout_id.clone();
        session.worktree_path = Some(canonical.clone());
        session.git_branch = Some(branch);
        let mut agent =
            ProcessNode::agent(session.id.clone(), "claude", canonical.clone(), NOW + 1);
        agent.lifecycle = Lifecycle::Alive;
        let agent_id = agent.id.clone();
        session.tree.insert(agent);
        harness
            .core
            .store
            .hierarchy()
            .create_worktree_session(&session, &checkout)
            .unwrap();
        let session_id = session.id.clone();
        harness
            .core
            .sessions
            .insert(session_id.clone(), session.clone());

        let primary_checkout = CheckoutId::primary_for(&workspace_id);
        let error = harness
            .core
            .acquire_workspace_write_lease(&workspace_id, &session_id, &primary_checkout, NOW + 2)
            .expect_err("a worktree Session cannot claim a different checkout");
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert!(harness
            .core
            .store
            .hierarchy()
            .active_lease(&workspace_id)
            .unwrap()
            .is_none());

        let in_memory = &harness.core.sessions[&session_id];
        assert_eq!(in_memory.mode, SessionMode::IsolatedWorktree);
        assert_eq!(in_memory.checkout_id, checkout_id);
        assert_eq!(in_memory.cwd, canonical);
        assert_eq!(in_memory.worktree_path, session.worktree_path);
        assert!(in_memory.tree.get(&agent_id).unwrap().is_running());
        let persisted = harness
            .core
            .store
            .sessions()
            .get(&session_id)
            .unwrap()
            .unwrap();
        assert_eq!(persisted.mode, SessionMode::IsolatedWorktree);
        assert_eq!(persisted.checkout_id, in_memory.checkout_id);
        assert_eq!(persisted.cwd, in_memory.cwd);
        assert_eq!(persisted.worktree_path, in_memory.worktree_path);
        assert!(persisted.tree.get(&agent_id).unwrap().is_running());
    }

    #[tokio::test]
    async fn the_reviewer_vertical_survives_a_ui_restart_without_changing_layout() {
        let mut harness = Harness::new().await;
        let root = harness._dir.path().to_string_lossy().to_string();
        let workspace_id = match harness
            .core
            .create_workspace("space-troopers".into(), root, NOW)
            .unwrap()
        {
            Response::Workspace { workspace } => workspace.id,
            other => panic!("unexpected {other:?}"),
        };
        let session_id = match harness
            .core
            .create_session(
                &workspace_id,
                "Fix climbing bugs".into(),
                None,
                Some(vec![NewPane::new(PaneKind::Agent)]),
                None,
                Vec::new(),
                NOW + 1,
            )
            .unwrap()
        {
            Response::Session { session } => session.id,
            other => panic!("unexpected {other:?}"),
        };
        let lease = harness
            .core
            .store
            .hierarchy()
            .active_lease(&workspace_id)
            .unwrap()
            .expect("the main Session owns the checkout");
        assert_eq!(lease.session_id, session_id);

        let mut claude = ProcessNode::agent(session_id.clone(), "claude", "/repo", NOW + 2);
        claude.title = "Claude Code".into();
        claude.lifecycle = Lifecycle::Alive;
        claude.pid = Some(41_001);
        let claude_id = claude.id.clone();
        let main_pane = harness.core.sessions[&session_id].layout.panes()[0]
            .id
            .clone();
        {
            let session = harness.core.sessions.get_mut(&session_id).unwrap();
            session.tree.insert(claude);
            session.layout.get_mut(&main_pane).unwrap().node_id = Some(claude_id.clone());
        }
        harness.core.persist_session(&session_id).unwrap();
        let saved_layout = harness.core.sessions[&session_id].layout.clone();

        let event = TurnEvent::new(
            session_id.clone(),
            EventKind::AgentSpawned {
                declared_name: Some("Reviewer".into()),
                agent_type: Some("code-reviewer".into()),
                agent_id: Some("reviewer-1".into()),
                task: Some("Reviewing climb_system.gd…".into()),
            },
            EventSource::Hook {
                tool: "claude-code".into(),
                event_name: "SubagentStart".into(),
            },
            Confidence::Explicit,
            NOW + 3,
        )
        .with_node(claude_id.clone());
        harness.core.ingest(event, NOW + 3);

        let reviewer_id = harness.core.sessions[&session_id].tree.children(&claude_id)[0]
            .id
            .clone();
        let (first_client, _frames) = harness.add_client(64);
        let snapshot = match harness
            .core
            .dispatch(
                first_client,
                Request::GetHierarchy {
                    surface_id: "main-window".into(),
                    include_archived: false,
                },
                NOW + 4,
            )
            .unwrap()
        {
            Response::Hierarchy { snapshot } => *snapshot,
            other => panic!("unexpected {other:?}"),
        };
        let branch = &snapshot.workspaces[0].sessions[0];
        let reviewer = branch
            .nodes
            .iter()
            .find(|node| node.node_id == reviewer_id)
            .unwrap();
        assert_eq!(reviewer.parent.as_ref(), Some(&claude_id));
        assert_eq!(
            reviewer
                .agent
                .as_ref()
                .and_then(|agent| agent.name.declared_name.as_deref()),
            Some("Reviewer")
        );
        assert_eq!(reviewer.relationship.confidence, Confidence::Explicit);
        assert_eq!(
            reviewer
                .activity_preview
                .as_ref()
                .map(|preview| preview.normalized_text.as_str()),
            Some("Reviewing climb_system.gd…")
        );
        assert!(reviewer.pane_bindings.is_empty());
        assert_eq!(harness.core.sessions[&session_id].layout, saved_layout);

        let generic_focus = harness
            .core
            .focus_pane_for_node("main-window".into(), &session_id, &reviewer_id)
            .unwrap();
        assert!(matches!(generic_focus, Response::PaneFocus { focus: None }));
        let attention_focus = harness
            .core
            .focus_pane_for_attention("main-window".into(), &session_id, &reviewer_id)
            .unwrap();
        assert!(matches!(
            attention_focus,
            Response::PaneFocus {
                focus: Some(PaneFocusView {
                    node_id,
                    pane_id,
                    attention_subject_node_id: Some(subject),
                    ..
                })
            } if node_id == claude_id && pane_id == main_pane && subject == reviewer_id
        ));
        harness
            .core
            .sessions
            .get_mut(&session_id)
            .unwrap()
            .tree
            .get_mut(&reviewer_id)
            .unwrap()
            .relationship
            .confidence = Confidence::InferredHigh;
        assert!(matches!(
            harness
                .core
                .focus_pane_for_attention("main-window".into(), &session_id, &reviewer_id)
                .unwrap(),
            Response::PaneFocus { focus: None }
        ));
        {
            let reviewer = harness
                .core
                .sessions
                .get_mut(&session_id)
                .unwrap()
                .tree
                .get_mut(&reviewer_id)
                .unwrap();
            reviewer.relationship.confidence = Confidence::Explicit;
            reviewer.pid = Some(42_424);
        }
        assert!(matches!(
            harness
                .core
                .focus_pane_for_attention("main-window".into(), &session_id, &reviewer_id)
                .unwrap(),
            Response::PaneFocus { focus: None }
        ));
        harness
            .core
            .sessions
            .get_mut(&session_id)
            .unwrap()
            .tree
            .get_mut(&reviewer_id)
            .unwrap()
            .pid = None;

        harness
            .core
            .set_tree_expanded(
                "main-window".into(),
                HierarchyKey::workspace(workspace_id.clone()),
                true,
                NOW + 5,
            )
            .unwrap();
        harness
            .core
            .set_tree_expanded(
                "main-window".into(),
                HierarchyKey::session(session_id.clone()),
                true,
                NOW + 6,
            )
            .unwrap();
        harness
            .core
            .select_tree_node(
                "main-window".into(),
                Some(HierarchyKey::process(reviewer_id.clone())),
                NOW + 7,
            )
            .unwrap();

        // The wire contract is newest-first. Persist enough distinct facts to
        // prove both ordering and that the limit is applied to the newest end.
        for sequence in 2..=6_u64 {
            let reviewer = harness.core.sessions[&session_id]
                .tree
                .get(&reviewer_id)
                .unwrap();
            let mut preview = reviewer.activity_preview.clone().unwrap();
            preview.raw_source_sequence = Some(sequence);
            preview.normalized_text = format!("Reviewer preview {sequence}");
            preview.updated_ms = NOW + 7 + sequence as i64;
            harness
                .core
                .sessions
                .get_mut(&session_id)
                .unwrap()
                .tree
                .get_mut(&reviewer_id)
                .unwrap()
                .activity_preview = Some(preview);
            harness.core.persist_session(&session_id).unwrap();
        }

        let history = harness
            .core
            .get_preview_history(&session_id, &reviewer_id, Some(4))
            .unwrap();
        let Response::PreviewHistory { entries, .. } = history else {
            panic!("unexpected preview response");
        };
        assert_eq!(
            entries
                .iter()
                .map(|preview| preview.normalized_text.as_str())
                .collect::<Vec<_>>(),
            [
                "Reviewer preview 6",
                "Reviewer preview 5",
                "Reviewer preview 4",
                "Reviewer preview 3",
            ]
        );

        harness
            .core
            .set_preview_visibility(&session_id, &reviewer_id, PreviewVisibility::Hide, NOW + 8)
            .unwrap();
        let hidden = harness
            .core
            .hierarchy_snapshot("main-window", false, NOW + 8)
            .unwrap();
        assert!(hidden.workspaces[0].sessions[0]
            .nodes
            .iter()
            .find(|node| node.node_id == reviewer_id)
            .unwrap()
            .activity_preview
            .is_none());
        assert!(matches!(
            harness
                .core
                .get_preview_history(&session_id, &reviewer_id, Some(8))
                .unwrap(),
            Response::PreviewHistory { entries, .. } if entries.is_empty()
        ));
        harness
            .core
            .set_preview_visibility(
                &session_id,
                &reviewer_id,
                PreviewVisibility::Inherit,
                NOW + 9,
            )
            .unwrap();

        let temporary = match harness
            .core
            .open_node_as_temporary_pane("main-window".into(), &session_id, &reviewer_id, NOW + 10)
            .unwrap()
        {
            Response::NodePane { pane } => pane,
            other => panic!("unexpected {other:?}"),
        };
        assert!(temporary.binding.temporary);
        assert_eq!(temporary.capability, NodePaneCapability::PreviewDetails);
        assert_eq!(harness.core.sessions[&session_id].layout, saved_layout);
        assert!(harness.core.sessions[&session_id]
            .tree
            .get(&reviewer_id)
            .unwrap()
            .is_running());
        let preview_focus = harness
            .core
            .focus_pane_for_attention("main-window".into(), &session_id, &reviewer_id)
            .unwrap();
        assert!(matches!(
            preview_focus,
            Response::PaneFocus {
                focus: Some(PaneFocusView {
                    node_id,
                    pane_id,
                    attention_subject_node_id: Some(subject),
                    ..
                })
            } if node_id == claude_id && pane_id == main_pane && subject == reviewer_id
        ));
        let explicit_preview_focus = harness
            .core
            .focus_pane_for_node("main-window".into(), &session_id, &reviewer_id)
            .unwrap();
        assert!(matches!(
            explicit_preview_focus,
            Response::PaneFocus {
                focus: Some(PaneFocusView {
                    node_id,
                    pane_id,
                    attention_subject_node_id: None,
                    ..
                })
            } if node_id == reviewer_id && pane_id == temporary.binding.pane_id
        ));

        let (other_client, _other_frames) = harness.add_client(64);
        let other_snapshot = match harness
            .core
            .dispatch(
                other_client,
                Request::GetHierarchy {
                    surface_id: "other-window".into(),
                    include_archived: false,
                },
                NOW + 11,
            )
            .unwrap()
        {
            Response::Hierarchy { snapshot } => *snapshot,
            other => panic!("unexpected {other:?}"),
        };
        let other_reviewer = other_snapshot.workspaces[0].sessions[0]
            .nodes
            .iter()
            .find(|node| node.node_id == reviewer_id)
            .unwrap();
        assert!(
            other_reviewer.pane_bindings.is_empty(),
            "one window must not advertise another window's temporary Pane"
        );
        assert!(harness
            .core
            .close_pane(
                other_client,
                &session_id,
                &temporary.binding.pane_id,
                CloseDisposition::KeepProcesses,
                NOW + 12,
            )
            .is_err());
        assert!(harness
            .core
            .store
            .hierarchy()
            .bindings_for_session(&session_id)
            .unwrap()
            .iter()
            .any(|binding| binding.pane_id == temporary.binding.pane_id));

        harness
            .core
            .close_pane(
                first_client,
                &session_id,
                &temporary.binding.pane_id,
                CloseDisposition::KeepProcesses,
                NOW + 13,
            )
            .unwrap();
        assert!(harness.core.sessions[&session_id]
            .tree
            .get(&reviewer_id)
            .unwrap()
            .is_running());
        assert_eq!(harness.core.sessions[&session_id].layout, saved_layout);

        let abandoned = match harness
            .core
            .open_node_as_temporary_pane("main-window".into(), &session_id, &reviewer_id, NOW + 14)
            .unwrap()
        {
            Response::NodePane { pane } => pane,
            other => panic!("unexpected {other:?}"),
        };
        let (second_client, _frames) = harness.add_client(64);
        let restored = match harness
            .core
            .dispatch(
                second_client,
                Request::GetHierarchy {
                    surface_id: "main-window".into(),
                    include_archived: false,
                },
                NOW + 15,
            )
            .unwrap()
        {
            Response::Hierarchy { snapshot } => *snapshot,
            other => panic!("unexpected {other:?}"),
        };
        assert!(!harness
            .core
            .store
            .hierarchy()
            .bindings_for_session(&session_id)
            .unwrap()
            .iter()
            .any(|binding| binding.pane_id == abandoned.binding.pane_id));
        harness.core.client_closed(first_client);
        assert!(harness.core.sessions[&session_id]
            .tree
            .get(&reviewer_id)
            .unwrap()
            .is_running());
        assert_eq!(harness.core.sessions[&session_id].layout, saved_layout);
        assert_eq!(
            restored.tree_state.selected,
            Some(HierarchyKey::process(reviewer_id.clone()))
        );
        assert!(restored
            .tree_state
            .expanded
            .contains(&HierarchyKey::workspace(workspace_id)));
        let reviewer = restored.workspaces[0].sessions[0]
            .nodes
            .iter()
            .find(|node| node.node_id == reviewer_id)
            .unwrap();
        assert_eq!(reviewer.parent.as_ref(), Some(&claude_id));
        assert_eq!(
            reviewer
                .activity_preview
                .as_ref()
                .map(|preview| preview.normalized_text.as_str()),
            Some("Reviewer preview 6")
        );
        assert!(reviewer.pane_bindings.is_empty());
    }
}
