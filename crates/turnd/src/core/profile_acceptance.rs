//! Acceptance coverage for the zero-memory agent launch path.
//!
//! These tests deliberately enter through the wire request understood by the
//! daemon. The only fake is the executable at the final process boundary: the
//! built-in provider adapter still owns profile resolution and builds the launch
//! plan, while `/usr/bin/true` keeps the suite offline and account-free.

use super::testing::Harness;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use turn_agents::{
    AdapterError, AdapterRegistry, AgentAdapter, Capabilities, ClaudeCodeAdapter, CodexAdapter,
    EventContext, GeminiCliAdapter, IntegrationLevel, LaunchContext, LaunchPlan,
    LaunchProfileDefinition, OpenCodeAdapter,
};
use turn_core::event::TurnEvent;
use turn_core::ids::{CheckoutId, PaneId, SessionId};
use turn_core::model::{
    AgentLaunchProfileRef, Layout, Pane, PaneKind, PanePlacement, Session, SessionMode, Workspace,
    WorkspaceCheckout,
};
use turn_proto::{NewPane, Request};

const NOW: i64 = 1_787_000_000_000;

#[derive(Debug, Clone)]
struct CapturedLaunch {
    profile: Option<AgentLaunchProfileRef>,
    user_args: Vec<String>,
    plan: LaunchPlan,
}

/// Delegates every product decision to a built-in adapter, replacing only the
/// executable after its launch plan has been captured. This makes PATH,
/// credentials and network access irrelevant without copying a single provider
/// flag into the harness.
struct CapturingBuiltinAdapter {
    inner: Arc<dyn AgentAdapter>,
    captured: Arc<Mutex<Vec<CapturedLaunch>>>,
}

impl AgentAdapter for CapturingBuiltinAdapter {
    fn id(&self) -> &'static str {
        self.inner.id()
    }

    fn provider(&self) -> &'static str {
        self.inner.provider()
    }

    fn executables(&self) -> &'static [&'static str] {
        self.inner.executables()
    }

    fn best_level(&self) -> IntegrationLevel {
        self.inner.best_level()
    }

    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    fn launch_profiles(&self) -> Vec<LaunchProfileDefinition> {
        self.inner.launch_profiles()
    }

    fn resolve_launch_profile(
        &self,
        profile_id: &str,
        user_args: &[String],
    ) -> Result<turn_agents::ResolvedLaunchProfile, AdapterError> {
        self.inner.resolve_launch_profile(profile_id, user_args)
    }

    fn detect(&self, _executable: &str) -> Option<PathBuf> {
        turn_agents::adapter::which("true")
    }

    fn prepare(&self, ctx: &LaunchContext) -> Result<LaunchPlan, AdapterError> {
        let plan = self.inner.prepare(ctx)?;
        self.captured
            .lock()
            .expect("capture lock")
            .push(CapturedLaunch {
                profile: ctx.launch_profile.clone(),
                user_args: ctx.user_args.clone(),
                plan: plan.clone(),
            });

        let mut harmless = plan;
        harmless.command = turn_agents::adapter::which("true")
            .expect("the POSIX true executable")
            .to_string_lossy()
            .into_owned();
        Ok(harmless)
    }

    fn normalise(&self, payload: &serde_json::Value, ctx: &EventContext) -> Vec<TurnEvent> {
        self.inner.normalise(payload, ctx)
    }

    fn resume_args(&self, external_id: &str) -> Option<Vec<String>> {
        self.inner.resume_args(external_id)
    }
}

struct ProviderCase {
    adapter: Arc<dyn AgentAdapter>,
    adapter_id: &'static str,
    command: &'static str,
    autonomous_flag: &'static str,
    autonomous_value: Option<&'static str>,
    conflict_profile: &'static str,
    conflicting_args: &'static [&'static str],
}

fn provider_cases() -> Vec<ProviderCase> {
    vec![
        ProviderCase {
            adapter: Arc::new(ClaudeCodeAdapter::new()),
            adapter_id: "claude-code",
            command: "claude",
            autonomous_flag: "--dangerously-skip-permissions",
            autonomous_value: None,
            conflict_profile: "autonomous",
            conflicting_args: &["--permission-mode", "acceptEdits"],
        },
        ProviderCase {
            adapter: Arc::new(CodexAdapter::new()),
            adapter_id: "codex",
            command: "codex",
            autonomous_flag: "--dangerously-bypass-approvals-and-sandbox",
            autonomous_value: None,
            conflict_profile: "autonomous",
            conflicting_args: &["--ask-for-approval", "never"],
        },
        ProviderCase {
            adapter: Arc::new(GeminiCliAdapter::new()),
            adapter_id: "gemini-cli",
            command: "gemini",
            autonomous_flag: "--approval-mode",
            autonomous_value: Some("yolo"),
            conflict_profile: "autonomous",
            conflicting_args: &["--approval-mode", "plan"],
        },
        ProviderCase {
            adapter: Arc::new(OpenCodeAdapter::new()),
            adapter_id: "opencode",
            command: "opencode",
            autonomous_flag: "--auto",
            autonomous_value: None,
            conflict_profile: "safe",
            conflicting_args: &["--auto"],
        },
    ]
}

fn profile_is_present(args: &[String], flag: &str, value: Option<&str>) -> bool {
    let Some(index) = args.iter().position(|arg| arg == flag) else {
        return false;
    };
    value.is_none_or(|value| args.get(index + 1).is_some_and(|arg| arg == value))
}

/// Builds a fully registered isolated checkout. Request dispatch therefore
/// crosses the same cwd, persistence and launch-authority gates as production.
fn add_isolated_session(harness: &mut Harness, suffix: &str) -> (SessionId, PaneId) {
    let primary = harness._dir.path().join(format!("primary-{suffix}"));
    let isolated = harness._dir.path().join(format!("worktree-{suffix}"));
    std::fs::create_dir_all(&primary).expect("primary checkout");
    std::fs::create_dir_all(&isolated).expect("isolated checkout");

    let requested_workspace = Workspace::new(
        format!("workspace-{suffix}"),
        primary.to_string_lossy().into_owned(),
        NOW,
    );
    harness
        .core
        .store
        .workspaces()
        .save(&requested_workspace)
        .expect("workspace creation");
    let workspace = harness
        .core
        .store
        .workspaces()
        .get(&requested_workspace.id)
        .expect("workspace read")
        .expect("created workspace");
    harness
        .core
        .workspaces
        .insert(workspace.id.clone(), workspace.clone());

    let target = Pane::new(PaneKind::Shell);
    let target_id = target.id.clone();
    let canonical = std::fs::canonicalize(&isolated).expect("canonical worktree");
    let mut session = Session::new(
        workspace.id.clone(),
        format!("session-{suffix}"),
        canonical.to_string_lossy(),
        Layout::single(target),
        NOW,
    );
    session.mode = SessionMode::IsolatedWorktree;
    session.checkout_id = CheckoutId::new();
    session.worktree_path = Some(canonical.to_string_lossy().into_owned());
    session.git_branch = Some(format!("test/{suffix}"));
    let checkout = WorkspaceCheckout {
        id: session.checkout_id.clone(),
        workspace_id: workspace.id,
        path: canonical.to_string_lossy().into_owned(),
        canonical_path: canonical.to_string_lossy().into_owned(),
        branch: session.git_branch.clone(),
        primary: false,
        shared_resources: Vec::new(),
        created_ms: NOW,
    };
    harness
        .core
        .store
        .hierarchy()
        .create_worktree_session(&session, &checkout)
        .expect("registered isolated session");
    let session_id = session.id.clone();
    harness.core.sessions.insert(session_id.clone(), session);
    (session_id, target_id)
}

fn wire_round_trip(request: Request) -> Request {
    let wire = serde_json::to_vec(&request).expect("request serialises");
    serde_json::from_slice(&wire).expect("request deserialises")
}

#[tokio::test]
async fn every_quick_profile_crosses_the_wire_and_reaches_the_builtin_spawn_plan() {
    for case in provider_cases() {
        for profile_id in ["safe", "autonomous"] {
            let suffix = format!("{}-{profile_id}", case.adapter_id);
            let captured = Arc::new(Mutex::new(Vec::new()));
            let mut registry = AdapterRegistry::bare();
            registry.register(Arc::new(CapturingBuiltinAdapter {
                inner: Arc::clone(&case.adapter),
                captured: Arc::clone(&captured),
            }));

            let mut harness = Harness::new().await;
            harness.core.registry = registry;
            let (session_id, target_id) = add_isolated_session(&mut harness, &suffix);
            let (client, _frames) = harness.add_client(64);
            let requested_profile = AgentLaunchProfileRef::new(case.adapter_id, profile_id);
            let mut pane = NewPane::new(PaneKind::Agent).with_command(case.command);
            pane.launch_profile = Some(requested_profile.clone());
            assert!(
                pane.args.is_empty(),
                "quick profiles require no remembered flags"
            );

            let request = wire_round_trip(Request::CreatePane {
                session_id: session_id.clone(),
                target_pane_id: target_id,
                placement: PanePlacement::SplitRight,
                pane,
            });
            harness
                .core
                .dispatch(client, request, NOW + 1)
                .unwrap_or_else(|error| panic!("{} {profile_id}: {error}", case.adapter_id));

            let launches = captured.lock().expect("capture lock").clone();
            assert_eq!(launches.len(), 1, "{} {profile_id}", case.adapter_id);
            let launch = &launches[0];
            assert_eq!(launch.profile.as_ref(), Some(&requested_profile));
            assert!(
                launch.user_args.is_empty(),
                "{} {profile_id} must reach the adapter without manual flags",
                case.adapter_id
            );
            assert_eq!(launch.plan.command, case.command);

            let generated = profile_is_present(
                &launch.plan.args,
                case.autonomous_flag,
                case.autonomous_value,
            );
            assert_eq!(
                generated,
                profile_id == "autonomous",
                "{} {profile_id}: {:?}",
                case.adapter_id,
                launch.plan.args
            );

            let live = harness
                .core
                .sessions
                .get(&session_id)
                .expect("live session");
            let created = live
                .layout
                .panes()
                .into_iter()
                .find(|pane| pane.command.as_deref() == Some(case.command))
                .expect("request-created pane");
            assert_eq!(created.launch_profile.as_ref(), Some(&requested_profile));
            let agent = live
                .tree
                .iter()
                .find(|node| node.kind == turn_core::model::NodeKind::Agent)
                .expect("daemon materialised the captured plan");
            assert_eq!(agent.args, launch.plan.args);

            let durable = harness
                .core
                .store
                .sessions()
                .get(&session_id)
                .expect("session read")
                .expect("durable session");
            let durable_pane = durable
                .layout
                .panes()
                .into_iter()
                .find(|pane| pane.command.as_deref() == Some(case.command))
                .expect("durable request-created pane");
            assert_eq!(
                durable_pane.launch_profile.as_ref(),
                Some(&requested_profile)
            );
        }
    }
}

#[tokio::test]
async fn conflicting_manual_policy_never_reaches_a_process_for_any_provider() {
    for case in provider_cases() {
        let suffix = format!("{}-conflict", case.adapter_id);
        let captured = Arc::new(Mutex::new(Vec::new()));
        let mut registry = AdapterRegistry::bare();
        registry.register(Arc::new(CapturingBuiltinAdapter {
            inner: Arc::clone(&case.adapter),
            captured: Arc::clone(&captured),
        }));

        let mut harness = Harness::new().await;
        harness.core.registry = registry;
        let (session_id, target_id) = add_isolated_session(&mut harness, &suffix);
        let (client, _frames) = harness.add_client(64);
        let requested_profile = AgentLaunchProfileRef::new(case.adapter_id, case.conflict_profile);
        let mut pane = NewPane::new(PaneKind::Agent).with_command(case.command);
        pane.args = case
            .conflicting_args
            .iter()
            .map(|arg| (*arg).to_string())
            .collect();
        pane.launch_profile = Some(requested_profile.clone());

        harness
            .core
            .dispatch(
                client,
                wire_round_trip(Request::CreatePane {
                    session_id: session_id.clone(),
                    target_pane_id: target_id,
                    placement: PanePlacement::SplitRight,
                    pane,
                }),
                NOW + 1,
            )
            .unwrap_or_else(|error| panic!("{} conflict request: {error}", case.adapter_id));

        assert!(
            captured.lock().expect("capture lock").is_empty(),
            "{} must reject the conflict before producing a launch plan",
            case.adapter_id
        );
        let live = harness
            .core
            .sessions
            .get(&session_id)
            .expect("live session");
        let created = live
            .layout
            .panes()
            .into_iter()
            .find(|pane| pane.command.as_deref() == Some(case.command))
            .expect("the semantic request remains available to correct");
        assert_eq!(created.launch_profile.as_ref(), Some(&requested_profile));
        assert!(created.node_id.is_none(), "no shell or agent may start");
        assert!(
            live.tree.is_empty(),
            "no runtime node may claim a failed launch"
        );
    }
}
