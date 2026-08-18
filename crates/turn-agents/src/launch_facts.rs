//! Privacy-safe launch receipts for agent nodes.
//!
//! A launch has three different truths: what the operator requested, what the
//! adapter actually prepared, and what the running provider later reports.  This
//! module projects the first two from real argv without retaining arbitrary argv
//! values.  Provider-owned launch profiles remain the authority for permission
//! posture; this code consumes their resolved receipt instead of reimplementing
//! which flags make a provider autonomous.

use crate::{AgentAdapter, LaunchPermissionPosture, LaunchProfileRole, ResolvedLaunchProfile};
use turn_core::model::LaunchConfiguration;

/// The two launch facts known synchronously at process creation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchFactConfigurations {
    pub requested: LaunchConfiguration,
    pub effective: LaunchConfiguration,
}

/// Projects request and adapter-plan argv into values safe for the operator UI.
///
/// Only the value of a recognised `--model`/`-m` option is retained. Every other
/// argv item is reduced to a syntactically valid flag name; positional arguments,
/// config values, paths, prompt text, tokens and credentials are discarded.
pub fn launch_fact_configurations(
    adapter: &dyn AgentAdapter,
    requested_args: &[String],
    effective_args: &[String],
    profile: &ResolvedLaunchProfile,
) -> LaunchFactConfigurations {
    let requested = adapter.launch_configuration(requested_args, profile);
    let mut effective = adapter.launch_configuration(effective_args, profile);
    for flag in &profile.effective_flag_names {
        if let Some(flag) = safe_flag_name(flag) {
            push_unique(&mut effective.safe_flags, flag);
        }
    }
    LaunchFactConfigurations {
        requested,
        effective,
    }
}

/// Common privacy boundary used by provider-owned launch inspectors.
///
/// Providers opt into the `-m` spelling only when their CLI owns it. Policy
/// options deliberately stay out of this generic layer: their meaning belongs to
/// the adapter that already validates and resolves them.
pub(crate) fn base_launch_configuration(
    args: &[String],
    profile: &ResolvedLaunchProfile,
    short_model: bool,
) -> LaunchConfiguration {
    let mut configuration = LaunchConfiguration {
        model: model_argument(args, short_model),
        model_display_name: None,
        permission_mode: profile.role.is_none().then(|| "Custom".into()),
        approval_mode: None,
        sandbox_mode: None,
        effort_level: None,
        thinking_enabled: None,
        safe_flags: safe_flag_names(args),
    };
    apply_profile_posture(&mut configuration, profile);
    configuration
}

fn apply_profile_posture(configuration: &mut LaunchConfiguration, profile: &ResolvedLaunchProfile) {
    configuration.permission_mode = Some(
        match (profile.role, profile.posture) {
            (Some(LaunchProfileRole::Safe), LaunchPermissionPosture::StandardSafeguards) => {
                "Safe · standard safeguards"
            }
            (Some(LaunchProfileRole::Autonomous), LaunchPermissionPosture::BypassPermissions) => {
                "Autonomous · bypass permissions"
            }
            (
                Some(LaunchProfileRole::Autonomous),
                LaunchPermissionPosture::BypassApprovalsAndSandbox,
            ) => "Autonomous · bypass approvals and sandbox",
            (Some(LaunchProfileRole::Autonomous), LaunchPermissionPosture::YoloApprovalMode) => {
                "Autonomous · yolo approval mode"
            }
            (
                Some(LaunchProfileRole::Autonomous),
                LaunchPermissionPosture::AutoApproveUnlessDenied,
            ) => "Autonomous · auto-approve unless denied",
            // A custom/legacy argv has no provider-owned policy promise. Keep that
            // explicit even when a known policy option below can be described.
            (None, LaunchPermissionPosture::Custom) => return,
            // Impossible for the built-in catalogues, but honest if a future adapter
            // constructs an inconsistent receipt: show the role without inventing a
            // posture it did not claim.
            (Some(LaunchProfileRole::Safe), _) => "Safe",
            (Some(LaunchProfileRole::Autonomous), _) => "Autonomous",
            (None, _) => return,
        }
        .to_string(),
    );

    match profile.posture {
        LaunchPermissionPosture::BypassApprovalsAndSandbox => {
            configuration.approval_mode = Some("bypassed".into());
            configuration.sandbox_mode = Some("disabled".into());
        }
        LaunchPermissionPosture::YoloApprovalMode => {
            configuration.approval_mode = Some("yolo".into());
        }
        LaunchPermissionPosture::AutoApproveUnlessDenied => {
            configuration.approval_mode = Some("auto unless denied".into());
        }
        LaunchPermissionPosture::Custom
        | LaunchPermissionPosture::StandardSafeguards
        | LaunchPermissionPosture::BypassPermissions => {}
    }
}

fn model_argument(args: &[String], short_model: bool) -> Option<String> {
    let names: &[&str] = if short_model {
        &["--model", "-m"]
    } else {
        &["--model"]
    };
    option_value(args, names).and_then(safe_model_name)
}

fn option_value<'a>(args: &'a [String], names: &[&str]) -> Option<&'a str> {
    let mut found = None;
    for (index, arg) in args.iter().enumerate() {
        if names.contains(&arg.as_str()) {
            found = args.get(index + 1).map(String::as_str);
            continue;
        }
        for name in names {
            if let Some(value) = arg.strip_prefix(&format!("{name}=")) {
                found = Some(value);
            }
        }
    }
    found
}

pub(crate) fn known_option_value<'a>(
    args: &'a [String],
    names: &[&str],
    allowed: &[&str],
) -> Option<&'a str> {
    option_value(args, names).filter(|value| allowed.contains(value))
}

/// Accepts provider/model identifiers while refusing credentials, paths and
/// option-like values. Used for both launch argv and authenticated runtime events.
pub fn safe_model_name(value: &str) -> Option<String> {
    let value = value.trim();
    let lower = value.to_ascii_lowercase();
    let credential_like = [
        "sk-",
        "ghp_",
        "github_pat_",
        "xoxb-",
        "xoxp-",
        "aiza",
        "ya29.",
        "eyj",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix));
    let path_like = value.starts_with(['/', '.', '~', '-'])
        || value.contains("..")
        || value.contains('\\')
        || value.contains("//");
    let valid = !value.is_empty()
        && value.len() <= 128
        && value.bytes().any(|byte| byte.is_ascii_alphabetic())
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':' | b'/')
        });
    (valid && !credential_like && !path_like).then(|| value.to_string())
}

fn safe_flag_names(args: &[String]) -> Vec<String> {
    let mut flags = Vec::new();
    for arg in args {
        if let Some(flag) = safe_flag_name(arg) {
            push_unique(&mut flags, flag);
        }
    }
    flags
}

fn safe_flag_name(argument: &str) -> Option<String> {
    let (prefix, body) = if let Some(body) = argument.strip_prefix("--") {
        ("--", body)
    } else {
        ("-", argument.strip_prefix('-')?)
    };
    let body = body.split_once('=').map_or(body, |(name, _)| name);
    let valid = !body.is_empty()
        && body
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphabetic())
        && body
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    valid.then(|| format!("{prefix}{body}"))
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AdapterError, AgentAdapter, ClaudeCodeAdapter, CodexAdapter, GeminiCliAdapter,
        OpenCodeAdapter, AUTONOMOUS_PROFILE_ID, SAFE_PROFILE_ID,
    };

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn every_provider_profile_reports_safe_or_its_exact_autonomous_posture() {
        let cases: Vec<(Box<dyn AgentAdapter>, &str, &str)> = vec![
            (
                Box::new(ClaudeCodeAdapter::new()),
                "Safe · standard safeguards",
                "Autonomous · bypass permissions",
            ),
            (
                Box::new(CodexAdapter::new()),
                "Safe · standard safeguards",
                "Autonomous · bypass approvals and sandbox",
            ),
            (
                Box::new(GeminiCliAdapter::new()),
                "Safe · standard safeguards",
                "Autonomous · yolo approval mode",
            ),
            (
                Box::new(OpenCodeAdapter::new()),
                "Safe · standard safeguards",
                "Autonomous · auto-approve unless denied",
            ),
        ];

        for (adapter, safe_label, autonomous_label) in cases {
            let requested = strings(&["--model", "provider/model"]);
            let safe = adapter
                .resolve_launch_profile(SAFE_PROFILE_ID, &requested)
                .expect("safe profile");
            let facts = launch_fact_configurations(adapter.as_ref(), &requested, &safe.args, &safe);
            assert_eq!(
                facts.effective.permission_mode.as_deref(),
                Some(safe_label),
                "{} Safe",
                adapter.id()
            );

            let autonomous = adapter
                .resolve_launch_profile(AUTONOMOUS_PROFILE_ID, &requested)
                .expect("autonomous profile");
            let facts = launch_fact_configurations(
                adapter.as_ref(),
                &requested,
                &autonomous.args,
                &autonomous,
            );
            assert_eq!(
                facts.requested.permission_mode.as_deref(),
                Some(autonomous_label),
                "{} requested Autonomous",
                adapter.id()
            );
            assert_eq!(
                facts.effective.permission_mode.as_deref(),
                Some(autonomous_label),
                "{} effective Autonomous",
                adapter.id()
            );
            assert!(autonomous
                .effective_flag_names
                .iter()
                .all(|flag| facts.effective.safe_flags.contains(flag)));
        }
    }

    #[test]
    fn custom_policy_modes_are_described_without_retaining_other_values() {
        type CustomCase = (
            Box<dyn AgentAdapter>,
            Vec<String>,
            &'static str,
            Option<&'static str>,
            Option<&'static str>,
        );
        let cases: Vec<CustomCase> = vec![
            (
                Box::new(ClaudeCodeAdapter::new()),
                strings(&[
                    "--permission-mode=acceptEdits",
                    "--settings",
                    "/private/hooks.json",
                ]),
                "Custom · acceptEdits",
                None,
                None,
            ),
            (
                Box::new(CodexAdapter::new()),
                strings(&["--ask-for-approval", "never", "--sandbox=workspace-write"]),
                "Custom",
                Some("never"),
                Some("workspace-write"),
            ),
            (
                Box::new(GeminiCliAdapter::new()),
                strings(&["--approval-mode", "yolo"]),
                "Custom · yolo approval mode",
                Some("yolo"),
                None,
            ),
            (
                Box::new(OpenCodeAdapter::new()),
                strings(&["--auto"]),
                "Custom · auto-approve unless denied",
                Some("auto unless denied"),
                None,
            ),
        ];

        for (adapter, args, permission, approval, sandbox) in cases {
            let profile = ResolvedLaunchProfile {
                adapter_id: adapter.id().into(),
                profile_id: "custom".into(),
                role: None,
                posture: LaunchPermissionPosture::Custom,
                args: args.clone(),
                effective_flag_names: Vec::new(),
            };
            let facts = launch_fact_configurations(adapter.as_ref(), &args, &args, &profile);
            assert_eq!(facts.effective.permission_mode.as_deref(), Some(permission));
            assert_eq!(facts.effective.approval_mode.as_deref(), approval);
            assert_eq!(facts.effective.sandbox_mode.as_deref(), sandbox);
            let serialised = serde_json::to_string(&facts.effective).unwrap();
            assert!(!serialised.contains("/private/hooks.json"));
        }
    }

    #[test]
    fn model_is_the_only_argv_value_retained_and_credentials_or_paths_never_leak() {
        let args = strings(&[
            "--model=openrouter/anthropic/claude-3.7",
            "--api-key",
            "turn-secret-value",
            "--config=/Users/operator/private.toml",
            "prompt containing private words",
            "--token=ghp_not-a-real-token",
        ]);
        let profile = ResolvedLaunchProfile {
            adapter_id: "opencode".into(),
            profile_id: "custom".into(),
            role: None,
            posture: LaunchPermissionPosture::Custom,
            args: args.clone(),
            effective_flag_names: Vec::new(),
        };
        let adapter = OpenCodeAdapter::new();
        let facts = launch_fact_configurations(&adapter, &args, &args, &profile);
        assert_eq!(
            facts.requested.model.as_deref(),
            Some("openrouter/anthropic/claude-3.7")
        );
        assert_eq!(
            facts.requested.safe_flags,
            strings(&["--model", "--api-key", "--config", "--token"])
        );
        let serialised = format!(
            "{}{}",
            serde_json::to_string(&facts.requested).unwrap(),
            serde_json::to_string(&facts.effective).unwrap()
        );
        for secret in [
            "turn-secret-value",
            "/Users/operator/private.toml",
            "prompt containing private words",
            "ghp_not-a-real-token",
        ] {
            assert!(!serialised.contains(secret), "leaked {secret:?}");
        }
    }

    #[test]
    fn short_model_is_provider_specific_and_hostile_model_values_are_refused() {
        let args = strings(&["-m", "gpt-5.6-sol"]);
        let profile = ResolvedLaunchProfile {
            adapter_id: "codex".into(),
            profile_id: "custom".into(),
            role: None,
            posture: LaunchPermissionPosture::Custom,
            args: args.clone(),
            effective_flag_names: Vec::new(),
        };
        let codex = CodexAdapter::new();
        let claude = ClaudeCodeAdapter::new();
        assert_eq!(
            launch_fact_configurations(&codex, &args, &args, &profile)
                .effective
                .model
                .as_deref(),
            Some("gpt-5.6-sol")
        );
        assert!(launch_fact_configurations(&claude, &args, &args, &profile)
            .effective
            .model
            .is_none());

        for hostile in [
            "/Users/me/model",
            "../../secret",
            "sk-secret-value",
            "--api-key",
        ] {
            let hostile_args = strings(&["--model", hostile]);
            assert!(
                launch_fact_configurations(&codex, &hostile_args, &hostile_args, &profile)
                    .effective
                    .model
                    .is_none()
            );
        }
    }

    #[test]
    fn conflicting_profile_policy_is_rejected_by_the_provider_owner() {
        let cases: Vec<(Box<dyn AgentAdapter>, Vec<String>)> = vec![
            (
                Box::new(ClaudeCodeAdapter::new()),
                strings(&["--permission-mode", "plan"]),
            ),
            (
                Box::new(CodexAdapter::new()),
                strings(&["--sandbox", "read-only"]),
            ),
            (
                Box::new(GeminiCliAdapter::new()),
                strings(&["--approval-mode", "plan"]),
            ),
        ];
        for (adapter, args) in cases {
            assert!(matches!(
                adapter.resolve_launch_profile(AUTONOMOUS_PROFILE_ID, &args),
                Err(AdapterError::LaunchProfileConflict { .. })
            ));
        }
        assert!(matches!(
            OpenCodeAdapter::new().resolve_launch_profile(SAFE_PROFILE_ID, &strings(&["--auto"])),
            Err(AdapterError::LaunchProfileConflict { .. })
        ));
    }
}
