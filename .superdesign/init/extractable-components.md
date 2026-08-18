# Reusable components for this task

- `AgentRuntimeStrip`: one above-the-fold row for provider, model, launch profile/permission mode, capacity or honest unavailability, and telemetry freshness.
- `CapacityMeter`: compact labelled remaining percentage/value with semantic colour and a textual fallback; never fabricate a quota.
- `AgentProviderPicker`: provider-owned agent choices (Claude Code, Codex, Gemini CLI, OpenCode) with installation/integration state.
- `LaunchProfilePicker`: Safe, Autonomous, and Custom. Safe is the default. Autonomous resolves provider-specific flags without requiring memory. Custom reveals raw argv as an advanced path.
- `ResolvedCommandPreview`: read-only command/argv preview that makes permission impact explicit before saving.

These are conceptual egui components to implement within the existing Rust view modules. They must not become a canvas, graph node, or separate navigation surface.
