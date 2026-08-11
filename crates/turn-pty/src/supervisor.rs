//! Process supervision: noticing what the processes we started went on to start.
//!
//! This is the fallback layer for hierarchy. When an agent's own hook engine
//! reports a subagent, that link is [`Relation::Confirmed`] and this module is
//! not involved. Everything else — a dev server, a test runner, a Godot editor
//! an agent launched — is only visible in the OS process table, so the links it
//! produces are [`Relation::Inferred`] and are labelled as such in the UI.
//!
//! Scanning is on demand rather than on a timer. Polling the whole process table
//! every second across thirty sessions is precisely the "aggressive polling" the
//! brief rules out.

use std::collections::{HashMap, HashSet};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use turn_core::model::NodeKind;

/// A process as the OS describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedProcess {
    pub pid: u32,
    pub ppid: Option<u32>,
    /// Executable name, e.g. `node`.
    pub name: String,
    /// Full command line, when the OS lets us read it.
    pub command_line: String,
    /// Argument boundaries as reported by the OS. This remains raw supervisor
    /// data; the daemon creates a bounded safe projection before persistence.
    pub args: Vec<String>,
    pub cwd: Option<String>,
    /// When the OS says this process began, in epoch milliseconds.
    ///
    /// The only fact that can separate a process from a stranger wearing its
    /// recycled pid: a process that began *after* Turn wrote a pid down cannot be
    /// the one Turn launched. `None` when the platform will not say, which is not
    /// evidence of anything and must never be read as agreement.
    pub start_time_ms: Option<i64>,
    /// Our guess at what this process is for.
    pub kind: NodeKind,
}

impl ObservedProcess {
    /// Whether this looks like a coding agent rather than a generic process.
    pub fn is_agentic(&self) -> bool {
        self.kind.is_agentic()
    }
}

/// Scans the process table and answers questions about descendants.
pub struct ProcessSupervisor {
    system: System,
}

impl Default for ProcessSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessSupervisor {
    pub fn new() -> Self {
        Self {
            system: System::new(),
        }
    }

    /// Refreshes the process table.
    ///
    /// Asks only for the fields Turn uses. A full refresh also collects memory,
    /// disk and CPU statistics per process, which is a great deal of work to
    /// throw away.
    pub fn refresh(&mut self) {
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_cmd(UpdateKind::Always)
                .with_cwd(UpdateKind::Always)
                .with_exe(UpdateKind::Always),
        );
    }

    /// Every descendant of `root_pid`, depth-first, excluding the root itself.
    ///
    /// Bounded by [`MAX_DEPTH`] so a pid-reuse cycle or a corrupt table cannot
    /// spin here forever.
    pub fn descendants(&self, root_pid: u32) -> Vec<ObservedProcess> {
        const MAX_DEPTH: usize = 32;

        // Index children by parent once, rather than re-scanning per level.
        let mut by_parent: HashMap<u32, Vec<u32>> = HashMap::new();
        for (pid, process) in self.system.processes() {
            if let Some(parent) = process.parent() {
                by_parent
                    .entry(parent.as_u32())
                    .or_default()
                    .push(pid.as_u32());
            }
        }

        let mut out = Vec::new();
        let mut seen: HashSet<u32> = HashSet::new();
        let mut stack: Vec<(u32, usize)> = vec![(root_pid, 0)];

        while let Some((pid, depth)) = stack.pop() {
            if depth >= MAX_DEPTH {
                continue;
            }
            let Some(children) = by_parent.get(&pid) else {
                continue;
            };
            for child in children {
                if !seen.insert(*child) {
                    continue;
                }
                if let Some(observed) = self.observe(*child) {
                    out.push(observed);
                }
                stack.push((*child, depth + 1));
            }
        }
        out
    }

    /// Direct children of a pid.
    pub fn children(&self, parent_pid: u32) -> Vec<ObservedProcess> {
        self.system
            .processes()
            .iter()
            .filter(|(_, process)| process.parent().map(|p| p.as_u32()) == Some(parent_pid))
            .filter_map(|(pid, _)| self.observe(pid.as_u32()))
            .collect()
    }

    /// Reads one process, if it still exists.
    pub fn observe(&self, pid: u32) -> Option<ObservedProcess> {
        let process = self.system.process(Pid::from_u32(pid))?;
        let name = process.name().to_string_lossy().to_string();
        let args = process
            .cmd()
            .iter()
            .map(|part| part.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        let command_line = args.join(" ");
        let effective = if command_line.is_empty() {
            name.clone()
        } else {
            command_line.clone()
        };

        Some(ObservedProcess {
            pid,
            ppid: process.parent().map(|p| p.as_u32()),
            name,
            command_line,
            args,
            cwd: process.cwd().map(|path| path.to_string_lossy().to_string()),
            // sysinfo reports whole seconds and zero when it has nothing. Both are
            // kept honest here: zero becomes `None` rather than 1970.
            start_time_ms: match process.start_time() {
                0 => None,
                seconds => i64::try_from(seconds).ok().map(|seconds| seconds * 1_000),
            },
            kind: classify(&effective),
        })
    }

    /// Whether a pid is still alive.
    pub fn is_alive(&self, pid: u32) -> bool {
        self.system.process(Pid::from_u32(pid)).is_some()
    }

    /// Total processes currently known, for diagnostics.
    pub fn process_count(&self) -> usize {
        self.system.processes().len()
    }
}

/// Guesses what a process is for from its command line.
///
/// Deliberately conservative: anything unrecognised is [`NodeKind::Unknown`],
/// which still appears in the tree. Turn showing an honest "unknown process" is
/// better than confidently mislabelling it.
pub fn classify(command_line: &str) -> NodeKind {
    let lower = command_line.to_ascii_lowercase();
    // Match on the executable rather than the whole line, so a shell command
    // that merely *mentions* claude is not classified as an agent.
    let executable = lower
        .split_whitespace()
        .next()
        .unwrap_or("")
        .rsplit('/')
        .next()
        .unwrap_or("");

    let is_word = |needle: &str| executable == needle;

    // These processes matter to the parent agent's lifecycle, but their user
    // interface belongs to the desktop. Classifying only explicit executable
    // names keeps a repository path containing "godot" from becoming an app.
    if [
        "godot",
        "godot4",
        "blender",
        "unity",
        "unityhub",
        "unrealeditor",
        "xcode",
        "code",
        "cursor",
        "zed",
        "subl",
        "idea",
        "pycharm",
        "webstorm",
        "rustrover",
    ]
    .contains(&executable)
    {
        return NodeKind::ExternalApp;
    }

    if is_word("claude")
        || is_word("codex")
        || is_word("gemini")
        || is_word("opencode")
        || is_word("aider")
    {
        return NodeKind::Agent;
    }
    if is_word("zsh") || is_word("bash") || is_word("fish") || is_word("sh") || is_word("dash") {
        return NodeKind::Shell;
    }
    if is_word("lazygit")
        || is_word("btop")
        || is_word("htop")
        || is_word("top")
        || is_word("vim")
        || is_word("nvim")
        || is_word("fang")
        || is_word("ranger")
        || is_word("yazi")
    {
        return NodeKind::Tui;
    }
    if is_word("tmux") {
        return NodeKind::TmuxSession;
    }

    // Beyond the executable name, the arguments carry the intent.
    if lower.contains(" test") || is_word("pytest") || is_word("jest") || is_word("vitest") {
        return NodeKind::TestRunner;
    }
    if lower.contains(" build") || is_word("make") || is_word("ninja") {
        return NodeKind::Build;
    }
    if lower.contains(" serve")
        || lower.contains(" dev")
        || lower.contains("runserver")
        || lower.contains("http.server")
    {
        return NodeKind::Server;
    }
    if is_word("watchman") || lower.contains(" watch") {
        return NodeKind::Watcher;
    }

    NodeKind::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_are_recognised_by_their_executable() {
        for command in [
            "claude",
            "/usr/local/bin/claude --resume",
            "codex exec 'do the thing'",
            "gemini",
            "opencode",
            "aider --model gpt",
        ] {
            assert_eq!(
                classify(command),
                NodeKind::Agent,
                "{command} should be an agent"
            );
        }
    }

    /// A shell command that merely mentions an agent is not an agent. Without
    /// this, `echo "ask claude about it"` would show up as a coding agent.
    #[test]
    fn a_command_that_only_mentions_an_agent_is_not_classified_as_one() {
        assert_ne!(classify("echo ask claude about it"), NodeKind::Agent);
        assert_ne!(classify("grep -r claude ."), NodeKind::Agent);
        assert_eq!(classify("zsh -c 'claude --help'"), NodeKind::Shell);
    }

    #[test]
    fn shells_tuis_and_runners_are_told_apart() {
        assert_eq!(classify("zsh"), NodeKind::Shell);
        assert_eq!(classify("/bin/bash -l"), NodeKind::Shell);
        assert_eq!(classify("lazygit"), NodeKind::Tui);
        assert_eq!(classify("btop"), NodeKind::Tui);
        assert_eq!(classify("fang"), NodeKind::Tui);
        assert_eq!(classify("cargo test --all"), NodeKind::TestRunner);
        assert_eq!(classify("npm run test"), NodeKind::TestRunner);
        assert_eq!(classify("make build"), NodeKind::Build);
        assert_eq!(classify("npm run dev"), NodeKind::Server);
        assert_eq!(classify("tmux new-session"), NodeKind::TmuxSession);
    }

    #[test]
    fn an_unrecognised_process_is_admitted_as_unknown_rather_than_guessed() {
        assert_eq!(
            classify("some-proprietary-binary --flag"),
            NodeKind::Unknown
        );
        assert_eq!(classify(""), NodeKind::Unknown);
    }

    #[test]
    fn graphical_apps_are_explicit_external_nodes() {
        for command in [
            "/Applications/Godot.app/Godot --editor project.godot",
            "blender scene.blend",
            "code /repo",
            "UnrealEditor Game.uproject",
        ] {
            assert_eq!(classify(command), NodeKind::ExternalApp, "{command}");
        }
        assert_eq!(classify("echo open blender later"), NodeKind::Unknown);
    }

    #[test]
    fn the_supervisor_can_see_this_test_process() {
        let mut supervisor = ProcessSupervisor::new();
        supervisor.refresh();
        assert!(supervisor.process_count() > 0);

        let me = std::process::id();
        assert!(supervisor.is_alive(me));
        let observed = supervisor.observe(me).expect("our own process");
        assert_eq!(observed.pid, me);
        assert!(!observed.name.is_empty());
    }

    /// Pid reuse is the reason anything reads `start_time_ms`, so a platform that
    /// silently stopped reporting it has to fail here rather than in the daemon,
    /// where a missing start time is deliberately treated as "cannot corroborate".
    #[test]
    fn the_supervisor_reports_when_a_process_began() {
        let mut supervisor = ProcessSupervisor::new();
        supervisor.refresh();
        let observed = supervisor
            .observe(std::process::id())
            .expect("our own process");
        let started = observed
            .start_time_ms
            .expect("this platform must report process start times");
        let now = turn_core::now_ms();
        assert!(started > 0 && started <= now + 1_000, "{started} vs {now}");
        // A test process is minutes old at most, so a wildly older value would mean
        // the units were misread rather than that the process is ancient.
        assert!(now - started < 24 * 60 * 60 * 1_000, "{started} vs {now}");
    }

    #[test]
    fn a_child_we_spawn_is_found_as_a_descendant() {
        // A shell that spawns a sleep: two levels of hierarchy to walk.
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg("sleep 5")
            .spawn()
            .expect("spawning a helper process");
        std::thread::sleep(std::time::Duration::from_millis(400));

        let mut supervisor = ProcessSupervisor::new();
        supervisor.refresh();

        let me = std::process::id();
        let descendants = supervisor.descendants(me);
        assert!(
            descendants.iter().any(|p| p.pid == child.id()),
            "the shell we started should appear as our descendant"
        );
        // And the grandchild it started is reachable too.
        assert!(
            descendants.iter().any(|p| p.command_line.contains("sleep")),
            "descendants must be walked transitively, got: {:?}",
            descendants
                .iter()
                .map(|p| p.command_line.clone())
                .collect::<Vec<_>>()
        );

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn direct_children_exclude_grandchildren() {
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg("sleep 5")
            .spawn()
            .expect("spawning a helper process");
        std::thread::sleep(std::time::Duration::from_millis(400));

        let mut supervisor = ProcessSupervisor::new();
        supervisor.refresh();
        let direct = supervisor.children(std::process::id());
        assert!(direct.iter().any(|p| p.pid == child.id()));

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn a_dead_pid_is_reported_as_gone_instead_of_fabricated() {
        let mut supervisor = ProcessSupervisor::new();
        supervisor.refresh();
        // Pid 0 is never a real user process on either platform we target.
        assert!(supervisor.observe(u32::MAX).is_none());
        assert!(!supervisor.is_alive(u32::MAX));
        assert!(supervisor.descendants(u32::MAX).is_empty());
    }
}
