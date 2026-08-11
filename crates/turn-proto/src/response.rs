//! Responses: one typed result per request, plus the single error shape.
//!
//! Every successful response is a variant of [`Response`], tagged with `result`.
//! [`Request::expected_result`](crate::Request::expected_result) names which one to
//! expect, and a test in this crate checks that every name it produces exists
//! here — so a client can treat the pairing as load-bearing rather than as
//! documentation that might be stale.
//!
//! Failures never arrive as a `Response`. They arrive as
//! [`ServerMessage::Error`](crate::ServerMessage::Error) carrying a
//! [`ProtoError`](crate::ProtoError), which keeps the success path free of
//! per-request error variants and gives a client one place to handle them.

use serde::{Deserialize, Serialize};
use turn_core::attention::Effect;
use turn_core::ids::{NodeId, PaneId, SessionId, WorkspaceId};
use turn_core::model::{ActivityPreview, Layout, Template, WorkspaceWriteLease};

use crate::bytes::TerminalBytes;
use crate::cells::{Grid, Scrollback};
use crate::geometry::PtySize;
use crate::screen::PaneStream;
use crate::search::SearchOutcome;
use crate::view::{
    AttentionView, ContextHandoffView, HierarchySnapshot, NodePaneView, PaneFocusView,
    SessionDetails, SessionSummary, SettingsView, TemplateSummary, TreeNodeView, TreeSurfaceState,
    WorkspaceSummary,
};

/// What a client needs to rebuild a terminal on attach.
///
/// This structure is what makes "processes survive UI restarts" a visible feature
/// rather than a claim. The daemon has held the pty the whole time; attaching hands
/// over the screen it has been keeping and the pane looks exactly as the user left
/// it.
///
/// Which of the two payloads is filled in depends on `stream`, and exactly one of
/// them ever is: a cells attachment gets `screen` and no bytes, a byte attachment
/// gets `replay` and no grid. Sending both would double the cost of every attach to
/// serve a client that asked for one of them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PaneAttachment {
    pub session_id: SessionId,
    pub pane_id: PaneId,
    /// The process behind the pane. `None` for a pane that has no process yet —
    /// an empty slot after a partial restore, or one of Turn's own views.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<NodeId>,
    /// Which representation this attachment will receive from now on.
    #[serde(default)]
    pub stream: PaneStream,
    /// The screen as cells, for a [`PaneStream::Cells`] attachment.
    ///
    /// Boxed because a grid is the largest thing in this catalogue by a wide margin
    /// and `Response` is moved through the daemon's channels for every keystroke.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen: Option<Box<Grid>>,
    /// Durable rows above the cell screen, oldest first. Empty for byte attachments.
    #[serde(default, skip_serializing_if = "Scrollback::is_empty")]
    pub scrollback: Scrollback,
    /// Bytes to feed a terminal emulator, for a [`PaneStream::Bytes`] attachment.
    /// Empty otherwise.
    ///
    /// This is the parsed screen re-emitted, not the raw scrollback: the raw ring can
    /// begin mid-escape-sequence after being truncated, which would corrupt the
    /// receiving terminal.
    #[serde(default, skip_serializing_if = "TerminalBytes::is_empty")]
    pub replay: TerminalBytes,
    /// The size the screen was taken at, which is the size the client asked for.
    pub size: PtySize,
    /// Whether output was dropped from the daemon's ring before this replay. The
    /// screen is still correct; the scrollback above it is incomplete, and the UI
    /// should say so rather than let the user scroll up into a lie.
    pub scrollback_truncated: bool,
    /// Total bytes this pane has ever produced, for staleness checks.
    pub bytes_seen: u64,
    /// Sequence number the next update for this attachment will carry — a
    /// [`ServerEvent::PaneScreen`](crate::ServerEvent::PaneScreen) for a cells
    /// attachment, a [`ServerEvent::PaneOutput`](crate::ServerEvent::PaneOutput) for
    /// a byte one. Lets a client detect a gap between what it was handed here and
    /// the live stream.
    pub next_seq: u64,
}

/// A process Turn stopped short of and that may still be alive.
///
/// Ending a Session is authoritative: it does not fail because one of its processes
/// escaped the daemon that started it. But it must not pretend either. A survivor of a
/// previous daemon has a PID-shaped observation and no owned handle, so Turn can neither
/// signal it safely — PID reuse makes that a coin flip on somebody else's process — nor
/// claim it exited. What it can do is name it, and let the user go and look.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct EscapedProcess {
    pub node_id: NodeId,
    pub session_id: SessionId,
    /// What the row was called in the tree, so the sentence the user reads names the
    /// same thing they were looking at.
    pub title: String,
    /// The last PID observed for it, when one was. `None` for a node whose runtime was
    /// only ever known through a pty the previous daemon owned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

/// A successful result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    /// The request succeeded and has nothing to report.
    Ack,

    /// Something was closed, ended or deleted, and it happened.
    ///
    /// Separate from [`Response::Ack`] because ending is destructive and authoritative,
    /// and the honest report of it has two halves: it is done, *and* these processes may
    /// still be running. Turn used to refuse the whole act when it could not guarantee
    /// the second half was empty, which left the user holding a Session they had already
    /// finished with and no way to be rid of it. `escaped` is empty in the ordinary case.
    Closed {
        escaped: Vec<EscapedProcess>,
    },

    Workspaces {
        workspaces: Vec<WorkspaceSummary>,
    },
    Workspace {
        workspace: WorkspaceSummary,
    },

    /// Full replacement for the unified navigation projection.
    Hierarchy {
        snapshot: Box<HierarchySnapshot>,
    },
    /// The daemon-authoritative state for one surface after expand/select.
    TreeState {
        state: TreeSurfaceState,
    },
    WorkspaceWriteLease {
        workspace_id: WorkspaceId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lease: Option<WorkspaceWriteLease>,
    },

    Sessions {
        sessions: Vec<SessionSummary>,
    },
    /// Boxed, like every other large payload here. The hot response by a wide
    /// margin is [`Response::Ack`] — one per keystroke written to a pty — and an
    /// enum sized for its biggest variant would make every one of those a
    /// kilobyte-wide move through the daemon's channels.
    Session {
        session: Box<SessionSummary>,
    },
    SessionDetails {
        details: Box<SessionDetails>,
    },

    /// Every preference in force, with where each value came from.
    ///
    /// Sent whole after a write as well as on a read, for the same reason a pane operation
    /// answers with the resulting layout: one change can move what is in force for more than
    /// the key that was written — removing a Session override reveals the Workspace's value —
    /// and a client that patched its own copy would be a second resolver, able to disagree
    /// with the daemon's. `scopes` says which levels this answer was assembled from, so a
    /// surface can offer "set at the Workspace level" only when there is a Workspace to set
    /// it on.
    Settings {
        settings: Box<SettingsView>,
    },

    Templates {
        templates: Vec<TemplateSummary>,
    },
    Template {
        template: TemplateSummary,
    },
    /// The complete Template definition, fetched only for the explicit editor.
    TemplateDetails {
        template: Box<Template>,
    },

    /// The layout after a pane operation. Returned rather than acked so the UI
    /// renders the daemon's arrangement instead of its own guess at what a split,
    /// a close or a clamped resize did.
    Layout {
        session_id: SessionId,
        layout: Layout,
    },
    Attached {
        attachment: Box<PaneAttachment>,
    },
    /// A pane's whole screen, after a client asked for it again.
    ///
    /// The same grid an attach would return, so a client's recovery path and its
    /// first-render path are one piece of code rather than two.
    Screen {
        session_id: SessionId,
        pane_id: PaneId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        /// The sequence number the next update for this attachment will carry. The
        /// grid is the state as of just before it, so a client can apply what arrives
        /// next without wondering whether it has already been applied.
        next_seq: u64,
        grid: Box<Grid>,
    },

    /// A screen-shaped window of a pane's history.
    ///
    /// The same [`Grid`] shape a live screen arrives in, so a client paints history with
    /// the code it already has rather than with a second renderer. `scrollback_offset` is
    /// the offset actually served — clamped to what the daemon still holds — and
    /// `scrollback_len` is how deep the record goes, so a client never has to guess
    /// whether it has reached the beginning.
    PaneHistory {
        session_id: SessionId,
        pane_id: PaneId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        grid: Box<Grid>,
    },
    /// What a search found in a pane's retained output.
    ///
    /// Boxed for the same reason as the other large payloads: the hot response is an ack
    /// per keystroke, and an enum sized for a thousand matches would widen every one of
    /// them.
    PaneMatches {
        session_id: SessionId,
        pane_id: PaneId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        outcome: Box<SearchOutcome>,
    },

    /// The pixels of one inline image, RGBA, for a client to upload as a texture.
    ///
    /// Boxed for the same reason as every other large payload here, and for a stronger
    /// one: this is by a wide margin the biggest thing the protocol carries, and an enum
    /// sized for it would widen the ack sent for every keystroke.
    PaneImage {
        session_id: SessionId,
        pane_id: PaneId,
        image: Box<crate::images::ImagePayload>,
    },

    Tree {
        session_id: SessionId,
        nodes: Vec<TreeNodeView>,
    },
    /// A single node after it changed — a relaunch, or a user correction.
    Node {
        node: Box<TreeNodeView>,
    },
    PreviewHistory {
        session_id: SessionId,
        node_id: NodeId,
        /// Newest first, bounded and already stable/redacted. Entry zero is the
        /// current item highlighted by Quick Preview.
        entries: Vec<ActivityPreview>,
    },
    /// A safe draft the client must show before requesting delivery.
    ContextHandoff {
        handoff: Box<ContextHandoffView>,
    },
    /// One explicit temporary Pane binding. It never mutates saved Layout.
    NodePane {
        pane: NodePaneView,
    },
    /// `None` is the normal "this node has no existing Pane" outcome. The
    /// request never opens one implicitly.
    PaneFocus {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        focus: Option<PaneFocusView>,
    },

    /// The next demand, or `None` when the queue is empty.
    Attention {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        entry: Option<AttentionView>,
    },
    AttentionList {
        entries: Vec<AttentionView>,
    },
    /// Effects for the UI to perform, straight from the attention manager.
    ///
    /// The domain type is sent as-is: the manager already decided what may happen,
    /// including whether a focus change was granted, deferred or refused, and
    /// re-describing that decision here would be a second place for it to be got
    /// wrong.
    Effects {
        effects: Vec<Effect>,
    },
}

impl Response {
    /// The stable `result` tag.
    pub fn result_name(&self) -> &'static str {
        match self {
            Response::Ack => "ack",
            Response::Closed { .. } => "closed",
            Response::Workspaces { .. } => "workspaces",
            Response::Workspace { .. } => "workspace",
            Response::Hierarchy { .. } => "hierarchy",
            Response::TreeState { .. } => "tree_state",
            Response::WorkspaceWriteLease { .. } => "workspace_write_lease",
            Response::Sessions { .. } => "sessions",
            Response::Session { .. } => "session",
            Response::SessionDetails { .. } => "session_details",
            Response::Settings { .. } => "settings",
            Response::Templates { .. } => "templates",
            Response::Template { .. } => "template",
            Response::TemplateDetails { .. } => "template_details",
            Response::Layout { .. } => "layout",
            Response::Attached { .. } => "attached",
            Response::Screen { .. } => "screen",
            Response::PaneHistory { .. } => "pane_history",
            Response::PaneMatches { .. } => "pane_matches",
            Response::PaneImage { .. } => "pane_image",
            Response::Tree { .. } => "tree",
            Response::Node { .. } => "node",
            Response::PreviewHistory { .. } => "preview_history",
            Response::ContextHandoff { .. } => "context_handoff",
            Response::NodePane { .. } => "node_pane",
            Response::PaneFocus { .. } => "pane_focus",
            Response::Attention { .. } => "attention",
            Response::AttentionList { .. } => "attention_list",
            Response::Effects { .. } => "effects",
        }
    }

    /// Every tag this catalogue defines. Used to check the request-to-response
    /// mapping is complete.
    pub const RESULT_NAMES: &'static [&'static str] = &[
        "ack",
        "closed",
        "workspaces",
        "workspace",
        "hierarchy",
        "tree_state",
        "workspace_write_lease",
        "sessions",
        "session",
        "session_details",
        "settings",
        "templates",
        "template",
        "template_details",
        "layout",
        "attached",
        "screen",
        "pane_history",
        "pane_matches",
        "pane_image",
        "tree",
        "node",
        "preview_history",
        "context_handoff",
        "node_pane",
        "pane_focus",
        "attention",
        "attention_list",
        "effects",
    ];
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::cells::Grid;
    use turn_core::attention::{DeferReason, Sound};
    use turn_core::event::Confidence;
    use turn_core::ids::{AttentionId, CheckoutId, WorkspaceId};
    use turn_core::model::{Pane, PaneKind, PaneNodeBinding, PreviewSource, Session, Template};

    const T0: i64 = 1_700_000_000_000;

    fn session() -> Session {
        Session::new(
            WorkspaceId::from_stored("ws_resp00001"),
            "Fix the flaky test",
            "/repo",
            Layout::single(Pane::new(PaneKind::Agent).with_command("claude")),
            T0,
        )
    }

    #[test]
    fn an_ack_is_a_tag_and_nothing_else() {
        let json = serde_json::to_string(&Response::Ack).unwrap();
        assert_eq!(json, "{\"result\":\"ack\"}");
        assert_eq!(
            serde_json::from_str::<Response>(&json).unwrap(),
            Response::Ack
        );
    }

    /// The default attach: the screen the daemon has been keeping, as cells, with no
    /// escape stream for the client to parse.
    #[test]
    fn an_attachment_carries_the_screen_the_daemon_already_parsed() {
        let mut screen = Grid::from_lines(&["ready"], 80);
        if let Some(cell) = screen.cell_mut(0, 0) {
            cell.fg = Some(crate::cells::Rgb::new(0, 205, 0));
        }
        let response = Response::Attached {
            attachment: Box::new(PaneAttachment {
                session_id: session().id,
                pane_id: PaneId::from_stored("pane_a"),
                node_id: Some(NodeId::from_stored("proc_a")),
                stream: PaneStream::Cells,
                screen: Some(Box::new(screen.clone())),
                scrollback: Scrollback::default(),
                replay: TerminalBytes::default(),
                size: PtySize::new(24, 80),
                scrollback_truncated: true,
                bytes_seen: 1_048_576,
                next_seq: 9_001,
            }),
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"result\":\"attached\""), "got {json}");
        assert!(
            !json.contains("\"replay\""),
            "a cells attachment must not also pay for the byte replay: {json}"
        );

        match serde_json::from_str::<Response>(&json).unwrap() {
            Response::Attached { attachment } => {
                assert_eq!(attachment.screen.as_deref(), Some(&screen));
                assert!(
                    attachment.scrollback_truncated,
                    "the UI must be able to admit the scrollback is partial"
                );
                assert_eq!(attachment.next_seq, 9_001);
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    /// The byte path is still here for whatever genuinely needs the stream itself,
    /// and it carries bytes rather than a grid.
    #[test]
    fn a_byte_attachment_carries_the_replay_bytes_intact() {
        // A realistic replay: colour, cursor placement, and the alternate screen a
        // TUI leaves the terminal in.
        let replay = b"\x1b[?1049h\x1b[H\x1b[2J\x1b[32mready\x1b[0m".to_vec();
        let attachment = PaneAttachment {
            session_id: session().id,
            pane_id: PaneId::from_stored("pane_a"),
            node_id: Some(NodeId::from_stored("proc_a")),
            stream: PaneStream::Bytes,
            screen: None,
            scrollback: Scrollback::default(),
            replay: TerminalBytes::new(replay.clone()),
            size: PtySize::new(24, 80),
            scrollback_truncated: false,
            bytes_seen: 12,
            next_seq: 0,
        };
        let json = serde_json::to_string(&attachment).unwrap();
        assert!(json.contains("\"stream\":\"bytes\""), "got {json}");
        assert!(!json.contains("\"screen\""), "got {json}");
        let back: PaneAttachment = serde_json::from_str(&json).unwrap();
        assert_eq!(back.replay.as_slice(), replay.as_slice());
        assert_eq!(back, attachment);
    }

    /// The resync answer and the attach answer carry the same grid, so a client has
    /// one piece of code for "here is the screen".
    #[test]
    fn a_resync_answers_with_the_whole_screen_and_the_sequence_it_starts_from() {
        let response = Response::Screen {
            session_id: session().id,
            pane_id: PaneId::from_stored("pane_a"),
            node_id: Some(NodeId::from_stored("proc_a")),
            next_seq: 77,
            grid: Box::new(Grid::from_lines(&["recovered"], 20)),
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"result\":\"screen\""), "got {json}");
        assert!(json.contains("\"next_seq\":77"));
        assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), response);
    }

    #[test]
    fn a_pane_with_no_process_attaches_without_inventing_a_node() {
        let attachment = PaneAttachment {
            session_id: session().id,
            pane_id: PaneId::from_stored("pane_empty"),
            node_id: None,
            stream: PaneStream::Cells,
            // An empty pane still has a screen: a blank one at the client's size,
            // which is better than a renderer with nothing to draw.
            screen: Some(Box::new(Grid::blank(24, 80))),
            scrollback: Scrollback::default(),
            replay: TerminalBytes::default(),
            size: PtySize::default(),
            scrollback_truncated: false,
            bytes_seen: 0,
            next_seq: 0,
        };
        let json = serde_json::to_string(&attachment).unwrap();
        assert!(
            !json.contains("node_id"),
            "an empty pane has no node: {json}"
        );
        assert_eq!(
            serde_json::from_str::<PaneAttachment>(&json).unwrap(),
            attachment
        );
    }

    #[test]
    fn an_empty_attention_queue_answers_with_no_entry_rather_than_an_error() {
        let json = serde_json::to_string(&Response::Attention { entry: None }).unwrap();
        assert_eq!(json, "{\"result\":\"attention\"}");
        assert_eq!(
            serde_json::from_str::<Response>(&json).unwrap(),
            Response::Attention { entry: None }
        );
    }

    /// The governor's verdicts travel as themselves. A deferral must not reach the
    /// UI looking like a granted focus change.
    #[test]
    fn focus_effects_keep_the_governors_verdict_distinguishable() {
        let session_id = session().id;
        let response = Response::Effects {
            effects: vec![
                Effect::Enqueued {
                    attention_id: AttentionId::from_stored("attn_a"),
                    session_id: session_id.clone(),
                },
                Effect::FocusDeferred {
                    session_id: session_id.clone(),
                    until_ms: T0 + 1_500,
                    reason: DeferReason::UserTyping,
                },
                Effect::PlaySound {
                    session_id,
                    sound: Sound::Subtle,
                },
            ],
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"effect\":\"focus_deferred\""), "got {json}");
        assert!(json.contains("\"reason\":\"user_typing\""));
        assert!(
            !json.contains("\"effect\":\"focus\""),
            "a deferral must not be mistakable for a jump: {json}"
        );
        assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), response);
    }

    #[test]
    fn every_response_round_trips_and_its_tag_matches_result_name() {
        for response in all_responses() {
            let value = serde_json::to_value(&response).unwrap();
            let tag = value.get("result").and_then(|v| v.as_str()).unwrap();
            assert_eq!(tag, response.result_name(), "tag mismatch for {response:?}");

            let json = serde_json::to_string(&response).unwrap();
            assert_eq!(
                serde_json::from_str::<Response>(&json).unwrap(),
                response,
                "round trip lost information: {json}"
            );
        }
    }

    #[test]
    fn the_declared_tag_list_matches_the_variants_that_exist() {
        let actual: std::collections::HashSet<&'static str> =
            all_responses().iter().map(|r| r.result_name()).collect();
        let declared: std::collections::HashSet<&'static str> =
            Response::RESULT_NAMES.iter().copied().collect();
        assert_eq!(actual, declared, "RESULT_NAMES has drifted from the enum");
        assert_eq!(
            declared.len(),
            Response::RESULT_NAMES.len(),
            "duplicate tag"
        );
        // 29 result shapes. Asserted so adding one without documenting it in
        // docs/PROTOCOL.md becomes a deliberate act.
        assert_eq!(declared.len(), 29, "the response catalogue changed size");
    }

    /// One of each variant, shared with the crate-wide contract tests.
    pub(crate) fn all_responses() -> Vec<Response> {
        let s = session();
        let summary = SessionSummary::from_session(&s, 0, false, T0);
        let workspace = WorkspaceSummary::from_workspace(
            &turn_core::model::Workspace::new("turn", "/Users/x/turn", T0),
            std::slice::from_ref(&summary),
        );
        let template = TemplateSummary::from_template(&Template::coding(T0));
        let node_id = NodeId::from_stored("proc_a");
        let pane_id = PaneId::from_stored("pane_node_a");
        let lease = WorkspaceWriteLease::active(
            s.workspace_id.clone(),
            s.id.clone(),
            CheckoutId::primary_for(&s.workspace_id),
            T0,
        );
        let preview = ActivityPreview {
            node_id: node_id.clone(),
            raw_source_sequence: Some(11),
            normalized_text: "Reviewing auth.rs".into(),
            source: PreviewSource::SemanticEvent,
            confidence: Confidence::Explicit,
            stable: true,
            contains_sensitive_data: false,
            redacted: false,
            updated_ms: T0 + 1,
        };
        let binding = PaneNodeBinding {
            pane_id: pane_id.clone(),
            session_id: s.id.clone(),
            node_id: node_id.clone(),
            temporary: true,
            surface_id: Some("window-a".into()),
            opened_ms: T0 + 2,
        };

        vec![
            Response::Ack,
            Response::Closed {
                escaped: vec![EscapedProcess {
                    node_id: node_id.clone(),
                    session_id: s.id.clone(),
                    title: "npm run dev".into(),
                    pid: Some(4821),
                }],
            },
            Response::Workspaces {
                workspaces: vec![workspace.clone()],
            },
            Response::Workspace { workspace },
            Response::Hierarchy {
                snapshot: Box::new(HierarchySnapshot::empty("window-a", 7)),
            },
            Response::TreeState {
                state: TreeSurfaceState {
                    surface_id: "window-a".into(),
                    selected: Some(crate::HierarchyKey::session(s.id.clone())),
                    expanded: vec![crate::HierarchyKey::workspace(s.workspace_id.clone())],
                    ..TreeSurfaceState::empty("window-a")
                },
            },
            Response::WorkspaceWriteLease {
                workspace_id: s.workspace_id.clone(),
                lease: Some(lease),
            },
            Response::Sessions {
                sessions: vec![summary.clone()],
            },
            Response::Session {
                session: Box::new(summary),
            },
            Response::SessionDetails {
                details: Box::new(SessionDetails::from_session(&s, 1, false, T0)),
            },
            Response::Templates {
                templates: vec![template.clone()],
            },
            Response::Template { template },
            Response::TemplateDetails {
                template: Box::new(turn_core::model::Template::two_shells(T0)),
            },
            Response::Layout {
                session_id: s.id.clone(),
                layout: s.layout.clone(),
            },
            Response::Attached {
                attachment: Box::new(PaneAttachment {
                    session_id: s.id.clone(),
                    pane_id: s.layout.panes()[0].id.clone(),
                    node_id: Some(NodeId::from_stored("proc_a")),
                    stream: PaneStream::Cells,
                    screen: Some(Box::new(Grid::from_lines(&["ready"], 80))),
                    scrollback: Scrollback::default(),
                    replay: TerminalBytes::default(),
                    size: PtySize::new(24, 80),
                    scrollback_truncated: false,
                    bytes_seen: 42,
                    next_seq: 1,
                }),
            },
            Response::Screen {
                session_id: s.id.clone(),
                pane_id: s.layout.panes()[0].id.clone(),
                node_id: Some(NodeId::from_stored("proc_a")),
                next_seq: 12,
                grid: Box::new(Grid::blank(24, 80)),
            },
            Response::PaneHistory {
                session_id: s.id.clone(),
                pane_id: s.layout.panes()[0].id.clone(),
                node_id: Some(NodeId::from_stored("proc_a")),
                grid: Box::new(Grid::from_lines(&["scrolled off the top"], 40)),
            },
            Response::PaneMatches {
                session_id: s.id.clone(),
                pane_id: s.layout.panes()[0].id.clone(),
                node_id: Some(NodeId::from_stored("proc_a")),
                outcome: Box::new(crate::search::SearchOutcome {
                    matches: vec![crate::search::PaneMatch::new(1_240, 4, 5)],
                    truncated: false,
                    scanned_lines: 5_040,
                    total_lines: 5_040,
                    screen_rows: 40,
                    scrollback_len: 5_000,
                }),
            },
            Response::PaneImage {
                session_id: s.id.clone(),
                pane_id: s.layout.panes()[0].id.clone(),
                image: Box::new(
                    crate::images::ImagePayload::new(2, 2, vec![0x40; 16])
                        .expect("a 2x2 image is a valid payload"),
                ),
            },
            Response::Tree {
                session_id: s.id.clone(),
                nodes: Vec::new(),
            },
            Response::Node {
                node: Box::new(TreeNodeView::from_node(
                    &turn_core::model::ProcessNode::agent(s.id.clone(), "claude", "/repo", T0),
                    0,
                    0,
                    T0,
                )),
            },
            Response::PreviewHistory {
                session_id: s.id.clone(),
                node_id: node_id.clone(),
                entries: vec![preview],
            },
            Response::ContextHandoff {
                handoff: Box::new(ContextHandoffView {
                    handoff_id: turn_core::ids::HandoffId::from_stored("handoff_resp001"),
                    session_id: s.id.clone(),
                    source_node_id: node_id.clone(),
                    target_node_id: NodeId::from_stored("proc_target"),
                    mode: crate::ContextHandoffMode::ContinueWith,
                    source_label: "Reviewer".into(),
                    target_label: "Claude".into(),
                    body: crate::ContextHandoffText::new("[Turn context handoff]\nSafe"),
                    preview_count: 1,
                    history_count: 0,
                    repository_included: true,
                    redacted: false,
                }),
            },
            Response::NodePane {
                pane: NodePaneView {
                    binding,
                    capability: crate::NodePaneCapability::PreviewDetails,
                },
            },
            Response::PaneFocus {
                focus: Some(PaneFocusView {
                    surface_id: "window-a".into(),
                    session_id: s.id.clone(),
                    node_id,
                    pane_id,
                    attention_subject_node_id: Some(NodeId::from_stored("proc_reviewer")),
                }),
            },
            Response::Attention { entry: None },
            Response::AttentionList {
                entries: Vec::new(),
            },
            Response::Effects {
                effects: vec![Effect::Cleared {
                    session_id: s.id.clone(),
                }],
            },
            Response::Settings {
                settings: Box::new(crate::view::SettingsView {
                    session_id: Some(s.id),
                    levels: vec![crate::view::SettingsLevel::global()],
                    entries: Vec::new(),
                }),
            },
        ]
    }
}
