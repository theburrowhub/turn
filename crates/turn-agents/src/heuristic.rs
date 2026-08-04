//! Inference from terminal output, for tools with no contract to honour.
//!
//! This is the weakest tier of detection and it is written to know that. Three
//! rules constrain everything below, and each of them exists because the failure
//! it prevents is worse than the detection it gives up:
//!
//! 1. **Every event uses [`EventSource::PtyHeuristic`]**, which caps confidence at
//!    [`Confidence::InferredHigh`]. A guess can badge a session; it can never move
//!    the user's focus. There is a test for exactly that.
//! 2. **It stands down completely in the alternate screen.** A TUI repainting
//!    itself produces text that looks like anything you care to match — a `vim`
//!    buffer containing the string "(y/n)" is not a permission prompt.
//! 3. **A quiet terminal is not, on its own, an agent waiting for you.** The
//!    "awaiting input" rule requires a positive marker of an agent's input
//!    affordance, because the alternative — treating silence at a prompt as a
//!    demand for attention — turns every idle shell in the workspace into a
//!    notification. That case is the single most common false positive available,
//!    so it is ruled out by construction rather than tuned away.
//!
//! State lives in [`OutputHeuristic`], one per pane, so the debounce is per
//! terminal and the caller keeps control of when observations happen.

use crate::adapter::{
    AdapterError, AgentAdapter, Capabilities, EventContext, IntegrationLevel, LaunchContext,
    LaunchPlan,
};
use serde_json::Value;
use turn_core::event::{AgentRef, Confidence, EventKind, EventSource, TurnEvent};
use turn_core::state::AwaitingReason;
use turn_pty::ScreenSnapshot;

/// Interactive agent CLIs Turn will run output inference against.
///
/// A closed list on purpose. Inference is only worth its false positives for
/// programs that actually hold a conversation; pointing it at `make` or `vim`
/// would produce confident nonsense, so anything unlisted gets the terminal and
/// no claims (see [`crate::registry`]).
pub const HEURISTIC_EXECUTABLES: &[&str] = &[
    "gemini",
    "aider",
    "cursor-agent",
    "opencode",
    "crush",
    "goose",
    "amp",
    "qwen",
    "copilot",
];

/// Phrases that mean the tool is working and interruptible.
///
/// Taken from the shapes these CLIs actually render: Claude Code and Gemini both
/// print an "esc to interrupt" affordance next to their spinner.
const ACTIVITY_MARKERS: &[&str] = &[
    "esc to interrupt",
    "esc to cancel",
    "ctrl+c to interrupt",
    "press esc to stop",
    "thinking…",
    "thinking...",
    "working…",
    "generating…",
];

/// Phrases that mean a confirmation box is on screen.
///
/// Kept specific. A vague list would match ordinary prose — an agent explaining
/// "you could allow this" is not a prompt — and a heuristic that fires on prose
/// is worse than none.
const CONFIRMATION_MARKERS: &[&str] = &[
    "(y/n)",
    "[y/n]",
    "(y/n/a)",
    "do you want to",
    "do you want me to",
    "allow?",
    "allow this",
    "proceed?",
    "apply this change?",
    "continue? (",
    "yes, and don't ask again",
    "1. yes",
];

/// Markers that a *conversational* input box is waiting, as opposed to a shell.
///
/// These are affordances only an agent CLI prints. Requiring one is what keeps a
/// settled `$` prompt from being reported as an agent awaiting you.
const INPUT_AFFORDANCE_MARKERS: &[&str] = &[
    "? for shortcuts",
    "/help for help",
    "type your message",
    "enter to send",
    "⏎ send",
    "shift+tab to cycle",
    "/help for more",
    "ctrl+j for newline",
];

/// Lines this far back are still considered "on screen" for matching.
///
/// Bounded so a marker that has scrolled out of sight stops counting: once it
/// does the inference becomes [`Inference::Undecided`], and
/// [`OutputHeuristic::observe`] withdraws what it had claimed. Matching the whole
/// visible buffer would keep a resolved permission box alive forever.
const TAIL_LINES: usize = 12;

/// Tuning for [`OutputHeuristic`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeuristicConfig {
    /// How long output must be unchanged before a settled prompt counts as
    /// waiting. Long enough that a slow agent mid-reply is not misread.
    pub idle_after_ms: i64,
    /// How long an inference must hold before it is emitted. This is the
    /// anti-flicker guard: a spinner that briefly clears between frames must not
    /// produce a stream of started/waiting/started events.
    pub debounce_ms: i64,
}

impl Default for HeuristicConfig {
    fn default() -> Self {
        Self {
            idle_after_ms: 2_000,
            debounce_ms: 750,
        }
    }
}

/// What the screen appears to be doing. The heuristic's whole vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inference {
    /// Something is running and says it can be interrupted.
    Working { rule: &'static str },
    /// A confirmation box is on screen.
    AwaitingPermission { rule: &'static str },
    /// A conversational prompt has settled with nothing happening.
    AwaitingInput { rule: &'static str },
    /// Nothing can be said. Includes every case Turn refuses to guess at: the
    /// alternate screen, a plain shell prompt, an empty terminal.
    Undecided,
}

impl Inference {
    fn rule(&self) -> Option<&'static str> {
        match self {
            Inference::Working { rule }
            | Inference::AwaitingPermission { rule }
            | Inference::AwaitingInput { rule } => Some(rule),
            Inference::Undecided => None,
        }
    }

    /// Whether this inference puts the session in the attention queue.
    ///
    /// The distinction matters because only a demand has to be withdrawable: if
    /// nothing was asked of the user, there is nothing to take back.
    fn is_demand(&self) -> bool {
        matches!(
            self,
            Inference::AwaitingPermission { .. } | Inference::AwaitingInput { .. }
        )
    }
}

/// Per-pane inference state.
///
/// Deliberately not a background task: the caller decides when to observe, and
/// passes the time in, so the debounce is testable without sleeping.
#[derive(Debug, Clone)]
pub struct OutputHeuristic {
    config: HeuristicConfig,
    /// Total bytes at the last observation, used to tell "quiet" from "busy"
    /// without diffing screen text.
    last_bytes_seen: u64,
    /// When output last changed.
    last_change_ms: i64,
    /// The inference currently being waited out, and when it first appeared.
    pending: Option<(Inference, i64)>,
    /// The last inference actually turned into an event.
    emitted: Option<Inference>,
    /// When that emission happened, so a rapid alternation still costs one event.
    emitted_at_ms: i64,
    /// Observations discarded because a full-screen application was in control.
    stood_down: u64,
}

impl OutputHeuristic {
    pub fn new() -> Self {
        Self::with_config(HeuristicConfig::default())
    }

    pub fn with_config(config: HeuristicConfig) -> Self {
        Self {
            config,
            last_bytes_seen: 0,
            last_change_ms: i64::MIN,
            pending: None,
            emitted: None,
            emitted_at_ms: i64::MIN,
            stood_down: 0,
        }
    }

    /// How many observations were skipped because a TUI was drawing.
    pub fn stood_down(&self) -> u64 {
        self.stood_down
    }

    /// The last inference that produced an event, for the UI's "why do you think
    /// that" affordance.
    pub fn last_emitted(&self) -> Option<&Inference> {
        self.emitted.as_ref()
    }

    /// Classifies a snapshot without touching the debounce state.
    ///
    /// Separate from [`OutputHeuristic::observe`] so the rules can be tested
    /// against captured screens directly, and so the UI can explain a badge.
    pub fn classify(&self, snapshot: &ScreenSnapshot, now_ms: i64) -> Inference {
        // Rule 2. A full-screen application owns the terminal; nothing on it is
        // agent output.
        if snapshot.alternate_screen {
            return Inference::Undecided;
        }

        let tail = snapshot.tail(TAIL_LINES).join("\n").to_lowercase();

        // A confirmation box outranks activity: a spinner elsewhere on screen
        // does not make a pending question less blocking.
        if let Some(marker) = CONFIRMATION_MARKERS
            .iter()
            .find(|marker| tail.contains(**marker))
        {
            return Inference::AwaitingPermission {
                rule: static_rule("confirmation_box", marker),
            };
        }

        if let Some(marker) = ACTIVITY_MARKERS
            .iter()
            .find(|marker| tail.contains(**marker))
        {
            return Inference::Working {
                rule: static_rule("activity_marker", marker),
            };
        }
        if tail.chars().any(is_spinner_frame) {
            return Inference::Working {
                rule: "spinner_frame",
            };
        }

        // Rule 3. Silence alone says nothing; an agent's own input affordance
        // plus silence says something.
        let quiet_for = now_ms.saturating_sub(self.last_change_ms);
        if quiet_for >= self.config.idle_after_ms
            && INPUT_AFFORDANCE_MARKERS
                .iter()
                .any(|marker| tail.contains(*marker))
        {
            return Inference::AwaitingInput {
                rule: "settled_agent_prompt",
            };
        }

        Inference::Undecided
    }

    /// Observes a snapshot and returns the events it justifies, if any.
    ///
    /// Emits at most one event per call, and only when an inference has held for
    /// the debounce window and differs from what was last reported.
    pub fn observe(
        &mut self,
        snapshot: &ScreenSnapshot,
        now_ms: i64,
        ctx: &EventContext,
    ) -> Vec<TurnEvent> {
        if snapshot.alternate_screen {
            // Standing down means forgetting, too: whatever was pending before
            // the TUI opened is no longer evidence of anything.
            self.stood_down += 1;
            self.pending = None;
            self.last_bytes_seen = snapshot.bytes_seen;
            self.last_change_ms = now_ms;
            return Vec::new();
        }

        if snapshot.bytes_seen != self.last_bytes_seen {
            self.last_bytes_seen = snapshot.bytes_seen;
            self.last_change_ms = now_ms;
        }

        let inference = self.classify(snapshot, now_ms);

        // A tier that can raise a demand has to be able to take one back. The
        // evidence for a guess is a marker on screen, and markers go away — the box
        // was answered, the prompt scrolled past [`TAIL_LINES`]. Without a
        // withdrawal a single false positive would keep a session in the attention
        // queue for as long as the pane lives, which is exactly the failure the
        // bounded tail was supposed to prevent.
        let withdrawing = matches!(inference, Inference::Undecided)
            && self.emitted.as_ref().is_some_and(Inference::is_demand);

        if matches!(inference, Inference::Undecided) && !withdrawing {
            self.pending = None;
            return Vec::new();
        }
        if self.emitted.as_ref() == Some(&inference) {
            // Already reported and still true. Repeating it would only produce
            // duplicate attention for one unchanged situation.
            self.pending = None;
            return Vec::new();
        }

        let first_seen = match &self.pending {
            Some((pending, first_seen)) if *pending == inference => *first_seen,
            _ => {
                self.pending = Some((inference.clone(), now_ms));
                now_ms
            }
        };

        if now_ms.saturating_sub(first_seen) < self.config.debounce_ms {
            return Vec::new();
        }
        if now_ms.saturating_sub(self.emitted_at_ms) < self.config.debounce_ms {
            return Vec::new();
        }

        // A withdrawal says only that what was reported no longer holds. It is not a
        // claim about what the screen is doing instead, because an undecided screen
        // supports no such claim.
        let (kind, rule) = if withdrawing {
            (EventKind::SessionAttentionResolved, "evidence_withdrawn")
        } else {
            let Some(kind) = event_kind(&inference) else {
                return Vec::new();
            };
            (kind, inference.rule().unwrap_or("heuristic"))
        };
        self.pending = None;
        self.emitted = Some(inference);
        self.emitted_at_ms = now_ms;

        vec![TurnEvent::new(
            ctx.session_id.clone(),
            kind,
            EventSource::PtyHeuristic {
                rule: rule.to_string(),
            },
            // Asked for honestly, and capped by the source regardless.
            Confidence::InferredHigh,
            now_ms,
        )
        .with_node(ctx.node_id.clone())
        .with_agent(AgentRef {
            provider: None,
            tool: Some("terminal".into()),
            model: None,
        })]
    }
}

impl Default for OutputHeuristic {
    fn default() -> Self {
        Self::new()
    }
}

fn event_kind(inference: &Inference) -> Option<EventKind> {
    match inference {
        Inference::Working { .. } => Some(EventKind::AgentTurnStarted {
            prompt_excerpt: None,
        }),
        // A guessed permission is reported as "waiting on you for a permission",
        // not as a permission Turn can describe: there is no trustworthy command
        // to show, and Turn will not read one out of screen text.
        Inference::AwaitingPermission { .. } => Some(EventKind::AgentWaitingForUser {
            reason: AwaitingReason::Permission,
            summary: None,
        }),
        Inference::AwaitingInput { .. } => Some(EventKind::AgentWaitingForUser {
            reason: AwaitingReason::Input,
            summary: None,
        }),
        Inference::Undecided => None,
    }
}

/// Braille and block spinner frames, as every modern CLI draws them.
fn is_spinner_frame(c: char) -> bool {
    // U+2800..U+28FF is the braille block; U+280 0 itself is blank and appears in
    // padded output, so it does not count as motion.
    ('\u{2801}'..='\u{28FF}').contains(&c) || matches!(c, '◐' | '◓' | '◑' | '◒' | '✳' | '✽' | '✻')
}

/// Names a rule without allocating a new string per observation.
///
/// The marker that matched is useful in the event log, but the set is fixed, so a
/// lookup keeps the rule name `&'static str` and the hot path allocation-free.
fn static_rule(prefix: &'static str, marker: &&'static str) -> &'static str {
    match (prefix, *marker) {
        ("confirmation_box", "(y/n)") => "confirmation_box:y_n",
        ("confirmation_box", "[y/n]") => "confirmation_box:y_n_bracketed",
        ("confirmation_box", "do you want to") => "confirmation_box:do_you_want_to",
        ("confirmation_box", _) => "confirmation_box",
        ("activity_marker", "esc to interrupt") => "activity:esc_to_interrupt",
        ("activity_marker", _) => "activity_marker",
        _ => prefix,
    }
}

/// The launch side of the heuristic tier.
///
/// There is nothing to configure — that is the whole point of this tier — so
/// `prepare` passes the user's command through untouched and reports honestly
/// that detection will be inferred.
pub struct HeuristicAdapter;

impl Default for HeuristicAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl HeuristicAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl AgentAdapter for HeuristicAdapter {
    fn id(&self) -> &'static str {
        "terminal-heuristic"
    }

    fn provider(&self) -> &'static str {
        "generic"
    }

    fn executables(&self) -> &'static [&'static str] {
        HEURISTIC_EXECUTABLES
    }

    fn best_level(&self) -> IntegrationLevel {
        IntegrationLevel::Heuristic
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            // Turn boundaries and confirmation boxes are guessable from output.
            turn_events: true,
            // Deliberately false: a matched "(y/n)" is not a permission request
            // Turn can describe, rank or resolve, so the UI must not offer to.
            permission_events: false,
            subagent_events: false,
            resumable: false,
            usage_events: false,
            external_session_id: false,
        }
    }

    fn prepare(&self, ctx: &LaunchContext) -> Result<LaunchPlan, AdapterError> {
        Ok(LaunchPlan {
            command: ctx.command.clone(),
            args: ctx.user_args.clone(),
            env: vec![
                ("TURN_SESSION_ID".into(), ctx.session_id.to_string()),
                ("TURN_NODE_ID".into(), ctx.node_id.to_string()),
            ],
            level: IntegrationLevel::Heuristic,
            note: "This tool has no way to report to Turn, so its state is inferred \
                   from what it prints. Inferred states are marked as guesses and \
                   never switch you between sessions."
                .to_string(),
        })
    }

    /// This tier has no structured channel, so there is no payload to translate.
    ///
    /// Inference goes through [`OutputHeuristic::observe`], which takes a screen
    /// snapshot rather than JSON. Returning nothing here is the honest answer: if
    /// something ever posts a payload claiming to be from this adapter, it is not
    /// a signal Turn has any contract for.
    fn normalise(&self, _payload: &Value, _ctx: &EventContext) -> Vec<TurnEvent> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turn_core::event::Severity;
    use turn_core::ids::{NodeId, SessionId};
    use turn_pty::{ScreenSize, TerminalBuffer};

    const T0: i64 = 1_723_000_000_000;

    fn ctx() -> EventContext {
        EventContext {
            session_id: SessionId::from_stored("sess_heur01"),
            node_id: NodeId::from_stored("proc_heur01"),
            timestamp_ms: T0,
        }
    }

    /// Renders text through a real terminal parser, so tests exercise the same
    /// snapshot shape the daemon sees rather than a hand-built struct.
    fn screen(text: &str) -> ScreenSnapshot {
        let mut buffer = TerminalBuffer::new(ScreenSize::new(24, 100));
        buffer.write(text.replace('\n', "\r\n").as_bytes());
        buffer.snapshot()
    }

    fn alternate(text: &str) -> ScreenSnapshot {
        let mut buffer = TerminalBuffer::new(ScreenSize::new(24, 100));
        buffer.write(b"\x1b[?1049h");
        buffer.write(text.replace('\n', "\r\n").as_bytes());
        buffer.snapshot()
    }

    /// Polls the same screen repeatedly, the way the daemon watches a pane.
    ///
    /// Nothing is emitted on the first sight of a pattern — the debounce costs one
    /// extra poll, which is the price of not reacting to a half-drawn frame.
    fn hold(
        heuristic: &mut OutputHeuristic,
        snapshot: &ScreenSnapshot,
        start_ms: i64,
        polls: i64,
    ) -> Vec<TurnEvent> {
        let mut events = Vec::new();
        for poll in 0..polls {
            events.extend(heuristic.observe(snapshot, start_ms + poll * 500, &ctx()));
        }
        events
    }

    /// A Gemini CLI mid-reply, as it actually looks.
    const WORKING_SCREEN: &str = "\
> refactor the parser

⠹ Thinking about your request (esc to interrupt · 12s)
";

    /// A confirmation box, in the shape these tools draw one.
    const CONFIRMATION_SCREEN: &str = "\
╭──────────────────────────────────────────────╮
│  Shell command                               │
│  rm -rf ./target                             │
│                                              │
│  Do you want to proceed?                     │
│  ❯ 1. Yes                                    │
│    2. No, tell Gemini what to do differently │
╰──────────────────────────────────────────────╯
";

    /// An agent that has finished and is showing its input box again.
    const SETTLED_AGENT_SCREEN: &str = "\
✦ Done. The parser now handles trailing commas.

╭────────────────────────────────────────────╮
│ >                                          │
╰────────────────────────────────────────────╯
  ~/repo (main*)                ? for shortcuts
";

    /// An ordinary zsh prompt, sitting idle. The false positive that matters.
    const SHELL_PROMPT_SCREEN: &str = "\
$ cargo build
   Compiling turn-agents v0.1.0
    Finished `dev` profile in 4.21s
jamuriano@studio ~/personal-workspace/turn %
";

    #[test]
    fn the_adapter_claims_only_the_agent_clis_it_knows_by_name() {
        let adapter = HeuristicAdapter::new();
        assert!(adapter.handles("gemini"));
        assert!(adapter.handles("/opt/homebrew/bin/aider --model sonnet"));
        assert!(!adapter.handles("claude"), "claude has a real contract");
        assert!(!adapter.handles("codex"));
        assert!(
            !adapter.handles("zsh"),
            "a shell must not get output inference"
        );
        assert!(!adapter.handles("make test"));
        assert_eq!(adapter.best_level(), IntegrationLevel::Heuristic);
    }

    #[test]
    fn preparing_changes_nothing_about_the_command_and_admits_it_is_guessing() {
        let ctx = LaunchContext {
            session_id: SessionId::from_stored("sess_heur01"),
            node_id: NodeId::from_stored("proc_heur01"),
            cwd: "/repo".into(),
            command: "gemini".into(),
            user_args: vec!["--model".into(), "gemini-3-pro".into()],
            endpoint: crate::adapter::HookEndpoint {
                base_url: "http://127.0.0.1:1".into(),
                token: "t".into(),
                helper_path: None,
            },
            scratch_dir: std::path::PathBuf::from("/tmp/turn-scratch"),
        };
        let plan = HeuristicAdapter::new().prepare(&ctx).unwrap();
        assert_eq!(plan.command, "gemini");
        assert_eq!(plan.args, ctx.user_args);
        assert_eq!(plan.level, IntegrationLevel::Heuristic);
        assert!(plan.note.contains("inferred"));
    }

    /// The rule that makes this tier safe. If this test ever fails, a guess has
    /// become able to move the user's viewport.
    #[test]
    fn an_inferred_event_can_never_steal_focus_however_confident_it_claims_to_be() {
        let mut heuristic = OutputHeuristic::new();
        let events = hold(&mut heuristic, &screen(CONFIRMATION_SCREEN), T0, 4);

        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event.confidence, Confidence::InferredHigh);
        assert!(
            !event.confidence.may_steal_focus(),
            "a pattern match must never take the user out of what they are doing"
        );
        assert!(event.confidence.is_provisional());
        assert!(matches!(event.source, EventSource::PtyHeuristic { .. }));
        assert_eq!(event.severity, Severity::Notice);
    }

    /// And it must hold even if the code asks for more. The cap lives in
    /// turn-core; this proves the two halves are wired together.
    #[test]
    fn asking_for_explicit_confidence_from_a_heuristic_source_is_downgraded() {
        let forged = TurnEvent::new(
            SessionId::from_stored("sess_heur01"),
            EventKind::AgentWaitingForUser {
                reason: AwaitingReason::Permission,
                summary: None,
            },
            EventSource::PtyHeuristic {
                rule: "confirmation_box".into(),
            },
            Confidence::Explicit,
            T0,
        );
        assert_eq!(forged.confidence, Confidence::InferredHigh);
        assert!(!forged.confidence.may_steal_focus());
    }

    #[test]
    fn a_spinner_with_an_interrupt_hint_means_the_tool_is_working() {
        let mut heuristic = OutputHeuristic::new();
        let events = hold(&mut heuristic, &screen(WORKING_SCREEN), T0, 4);

        assert_eq!(events.len(), 1);
        assert!(matches!(events[0].kind, EventKind::AgentTurnStarted { .. }));
        match &events[0].source {
            EventSource::PtyHeuristic { rule } => {
                assert_eq!(rule, "activity:esc_to_interrupt");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn a_bare_spinner_frame_with_no_words_still_reads_as_activity() {
        let heuristic = OutputHeuristic::new();
        let inference = heuristic.classify(&screen("⣾ compiling"), T0);
        assert_eq!(
            inference,
            Inference::Working {
                rule: "spinner_frame"
            }
        );
        // A blank braille cell is padding, not motion.
        assert_eq!(
            heuristic.classify(&screen("\u{2800} nothing happening"), T0),
            Inference::Undecided
        );
    }

    #[test]
    fn a_confirmation_box_reads_as_waiting_on_a_permission() {
        let mut heuristic = OutputHeuristic::new();
        let events = hold(&mut heuristic, &screen(CONFIRMATION_SCREEN), T0, 4);

        assert_eq!(events.len(), 1);
        match &events[0].kind {
            EventKind::AgentWaitingForUser { reason, summary } => {
                assert_eq!(*reason, AwaitingReason::Permission);
                assert!(
                    summary.is_none(),
                    "Turn must not read a command out of screen text and present it as fact"
                );
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(
            events[0].attention_reason(),
            Some(AwaitingReason::Permission)
        );
    }

    #[test]
    fn the_shorter_confirmation_shapes_are_recognised_too() {
        let heuristic = OutputHeuristic::new();
        for text in [
            "Overwrite src/main.rs? (y/n)",
            "Install the missing dependency [Y/n]",
            "Allow this command to run?",
            "Apply this change? 1. Yes  2. No",
        ] {
            assert!(
                matches!(
                    heuristic.classify(&screen(text), T0),
                    Inference::AwaitingPermission { .. }
                ),
                "{text:?} should read as a confirmation"
            );
        }
    }

    /// The most important negative case in this module.
    #[test]
    fn an_idle_shell_prompt_is_never_reported_as_an_agent_waiting_on_you() {
        let mut heuristic = OutputHeuristic::new();
        let mut now = T0;

        // Observe repeatedly, long past any idle window, with nothing changing.
        for _ in 0..20 {
            now += 1_000;
            let events = heuristic.observe(&screen(SHELL_PROMPT_SCREEN), now, &ctx());
            assert!(
                events.is_empty(),
                "a settled shell prompt produced {events:?}"
            );
        }
        assert_eq!(
            heuristic.classify(&screen(SHELL_PROMPT_SCREEN), now),
            Inference::Undecided
        );
        assert!(heuristic.last_emitted().is_none());
    }

    /// Other things that must not be mistaken for an agent's question.
    #[test]
    fn ordinary_output_and_prose_are_left_alone() {
        let heuristic = OutputHeuristic::new();
        for text in [
            "test result: ok. 27 passed; 0 failed",
            "error[E0308]: mismatched types",
            "You can allow it later if you prefer.",
            "",
            "   Compiling serde v1.0.228\n   Compiling axum v0.8.7",
        ] {
            assert_eq!(
                heuristic.classify(&screen(text), T0 + 60_000),
                Inference::Undecided,
                "{text:?} must not produce a claim"
            );
        }
    }

    #[test]
    fn a_settled_agent_prompt_needs_both_silence_and_an_agent_affordance() {
        let mut heuristic = OutputHeuristic::new();
        let snapshot = screen(SETTLED_AGENT_SCREEN);

        // First observation: output just changed, so nothing has settled yet.
        assert!(heuristic.observe(&snapshot, T0, &ctx()).is_empty());
        assert_eq!(
            heuristic.classify(&snapshot, T0 + 500),
            Inference::Undecided
        );

        // Past the idle window, with the affordance on screen, it counts.
        assert_eq!(
            heuristic.classify(&snapshot, T0 + 3_000),
            Inference::AwaitingInput {
                rule: "settled_agent_prompt"
            }
        );
        // Seen once, then confirmed on the next poll.
        assert!(heuristic.observe(&snapshot, T0 + 3_000, &ctx()).is_empty());
        let events = heuristic.observe(&snapshot, T0 + 4_000, &ctx());
        assert_eq!(events.len(), 1);
        match &events[0].kind {
            EventKind::AgentWaitingForUser { reason, .. } => {
                assert_eq!(*reason, AwaitingReason::Input);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    /// Rule 2: while a TUI owns the screen, this module says nothing at all —
    /// even about text that would otherwise match every rule it has.
    #[test]
    fn a_tui_redrawing_itself_is_not_agent_output() {
        let mut heuristic = OutputHeuristic::new();
        let hostile = alternate(
            "Do you want to proceed? (y/n)\n⠹ working (esc to interrupt)\n? for shortcuts",
        );

        for step in 0..10 {
            let events = heuristic.observe(&hostile, T0 + step * 5_000, &ctx());
            assert!(
                events.is_empty(),
                "the alternate screen produced {events:?}"
            );
        }
        assert_eq!(
            heuristic.classify(&hostile, T0 + 90_000),
            Inference::Undecided
        );
        assert_eq!(heuristic.stood_down(), 10);
    }

    /// Leaving a TUI must not release a flood of stale conclusions.
    #[test]
    fn what_was_pending_before_a_tui_opened_is_forgotten() {
        let mut heuristic = OutputHeuristic::new();
        let confirmation = screen(CONFIRMATION_SCREEN);

        // A confirmation appears but has not yet held long enough to be emitted.
        assert!(heuristic.observe(&confirmation, T0, &ctx()).is_empty());
        // The user opens lazygit in the same pane.
        assert!(heuristic
            .observe(&alternate("lazygit"), T0 + 100, &ctx())
            .is_empty());
        // Coming back to a screen with no confirmation on it, nothing is emitted.
        assert!(heuristic
            .observe(&screen("all done"), T0 + 10_000, &ctx())
            .is_empty());
        assert!(heuristic.last_emitted().is_none());
    }

    /// The debounce exists so a flickering pattern costs at most one event.
    #[test]
    fn a_flickering_pattern_does_not_produce_a_storm_of_events() {
        let mut heuristic = OutputHeuristic::with_config(HeuristicConfig {
            idle_after_ms: 2_000,
            debounce_ms: 1_000,
        });
        let working = screen(WORKING_SCREEN);
        let blank = screen("still going");
        let mut emitted = 0;

        // 100 alternating frames 50 ms apart: a spinner clearing between redraws.
        for frame in 0..100 {
            let snapshot = if frame % 2 == 0 { &working } else { &blank };
            emitted += heuristic.observe(snapshot, T0 + frame * 50, &ctx()).len();
        }
        assert_eq!(
            emitted, 0,
            "an inference that never holds for the debounce window must not be reported"
        );
    }

    #[test]
    fn a_state_that_holds_is_reported_exactly_once() {
        let mut heuristic = OutputHeuristic::with_config(HeuristicConfig {
            idle_after_ms: 2_000,
            debounce_ms: 1_000,
        });
        let working = screen(WORKING_SCREEN);
        let mut emitted = 0;

        for step in 0..60 {
            emitted += heuristic.observe(&working, T0 + step * 500, &ctx()).len();
        }
        assert_eq!(
            emitted, 1,
            "one unchanged situation is one event, not thirty"
        );
        assert_eq!(
            heuristic.last_emitted(),
            Some(&Inference::Working {
                rule: "activity:esc_to_interrupt"
            })
        );
    }

    #[test]
    fn a_genuine_transition_from_working_to_waiting_is_reported() {
        let mut heuristic = OutputHeuristic::with_config(HeuristicConfig {
            idle_after_ms: 2_000,
            debounce_ms: 500,
        });
        let mut events = Vec::new();

        // Working for three seconds.
        for step in 0..6 {
            events.extend(heuristic.observe(&screen(WORKING_SCREEN), T0 + step * 500, &ctx()));
        }
        // Then a confirmation box appears and stays.
        for step in 6..12 {
            events.extend(heuristic.observe(&screen(CONFIRMATION_SCREEN), T0 + step * 500, &ctx()));
        }

        assert_eq!(events.len(), 2, "one per real transition, got {events:?}");
        assert!(matches!(events[0].kind, EventKind::AgentTurnStarted { .. }));
        assert!(matches!(
            events[1].kind,
            EventKind::AgentWaitingForUser {
                reason: AwaitingReason::Permission,
                ..
            }
        ));
        // Both remain provisional, whatever they claim to have seen.
        assert!(events.iter().all(|e| e.confidence.is_provisional()));
    }

    /// The other half of the bounded tail, and the one that makes it worth
    /// anything: a guess that has lost its evidence has to be taken back. A
    /// heuristic that can only raise demands turns every false positive into a
    /// session that claims to be waiting on you for the rest of its life.
    #[test]
    fn a_guessed_demand_is_withdrawn_once_its_evidence_leaves_the_screen() {
        let mut heuristic = OutputHeuristic::with_config(HeuristicConfig {
            idle_after_ms: 2_000,
            debounce_ms: 750,
        });
        let confirmation = screen(CONFIRMATION_SCREEN);

        let raised = hold(&mut heuristic, &confirmation, T0, 3);
        assert_eq!(raised.len(), 1, "got {raised:?}");
        assert_eq!(
            raised[0].attention_reason(),
            Some(AwaitingReason::Permission)
        );

        // The user answers, and the box is gone.
        let answered = screen("Applied. 3 files changed.");
        let mut withdrawn = Vec::new();
        for poll in 3..12 {
            withdrawn.extend(heuristic.observe(&answered, T0 + poll * 500, &ctx()));
        }

        assert_eq!(
            withdrawn.len(),
            1,
            "one withdrawal, not silence and not a stream: {withdrawn:?}"
        );
        assert!(matches!(
            withdrawn[0].kind,
            EventKind::SessionAttentionResolved
        ));
        assert_eq!(
            withdrawn[0].confidence,
            Confidence::InferredHigh,
            "taking a guess back is still a guess"
        );
        assert!(!withdrawn[0].confidence.may_steal_focus());
        match &withdrawn[0].source {
            EventSource::PtyHeuristic { rule } => assert_eq!(rule, "evidence_withdrawn"),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(
            withdrawn[0].node_id.as_ref().map(|node| node.as_str()),
            Some("proc_heur01"),
            "a withdrawal must name the node whose demand it retracts"
        );

        // And the pane is not left mute: a box that comes back is reported again.
        let again = hold(&mut heuristic, &confirmation, T0 + 6_000, 3);
        assert_eq!(again.len(), 1, "got {again:?}");
        assert_eq!(
            again[0].attention_reason(),
            Some(AwaitingReason::Permission)
        );
    }

    /// Withdrawal is for demands only. "The tool was working and now the screen
    /// says nothing" asks nothing of the user, so there is nothing to retract and
    /// an event would be noise.
    #[test]
    fn a_quiet_screen_after_activity_withdraws_nothing_because_nothing_was_demanded() {
        let mut heuristic = OutputHeuristic::with_config(HeuristicConfig {
            idle_after_ms: 2_000,
            debounce_ms: 750,
        });
        let working = hold(&mut heuristic, &screen(WORKING_SCREEN), T0, 3);
        assert_eq!(working.len(), 1);
        assert_eq!(working[0].attention_reason(), None);

        let quiet = screen("done");
        for poll in 3..12 {
            assert!(
                heuristic
                    .observe(&quiet, T0 + poll * 500, &ctx())
                    .is_empty(),
                "a screen with nothing on it is not an event"
            );
        }
    }

    /// A marker that has scrolled out of view must stop counting, or a resolved
    /// permission would keep the session marked as blocked forever.
    #[test]
    fn a_marker_that_has_scrolled_away_no_longer_counts() {
        let mut buffer = TerminalBuffer::new(ScreenSize::new(24, 100));
        buffer.write(b"Do you want to proceed?\r\n");
        for i in 0..40 {
            buffer.write(format!("output line {i}\r\n").as_bytes());
        }
        let heuristic = OutputHeuristic::new();
        assert_eq!(
            heuristic.classify(&buffer.snapshot(), T0 + 60_000),
            Inference::Undecided
        );
    }

    /// There is no structured payload at this tier; posting one must not invent
    /// an event.
    #[test]
    fn the_heuristic_adapter_has_no_payload_contract_and_claims_none() {
        let adapter = HeuristicAdapter::new();
        for payload in [
            serde_json::json!({ "hook_event_name": "Stop" }),
            serde_json::json!({}),
            serde_json::json!(null),
        ] {
            assert!(adapter.normalise(&payload, &ctx()).is_empty());
        }
        assert!(
            !adapter.capabilities().permission_events,
            "a guessed confirmation is not a permission the UI may offer to resolve"
        );
    }

    #[test]
    fn observing_an_empty_terminal_is_harmless() {
        let mut heuristic = OutputHeuristic::new();
        let empty = screen("");
        for step in 0..10 {
            assert!(heuristic
                .observe(&empty, T0 + step * 1_000, &ctx())
                .is_empty());
        }
    }
}
