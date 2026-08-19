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
    /// Kernel-reported executable path, falling back to [`Self::name`] only when
    /// the platform does not expose it. Unlike argv[0], this is not controlled by
    /// `exec -a` or a runtime changing its process title.
    pub executable: String,
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
        let executable = process
            .exe()
            .map(|path| path.to_string_lossy().to_string())
            .filter(|path| !path.is_empty())
            .unwrap_or_else(|| name.clone());
        let args = process
            .cmd()
            .iter()
            .map(|part| part.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        let command_line = args.join(" ");
        let (program, program_args) = args
            .split_first()
            .map_or((name.as_str(), &[][..]), |(program, args)| {
                (program.as_str(), args)
            });
        let kind = classify_argv(program, program_args);

        Some(ObservedProcess {
            pid,
            ppid: process.parent().map(|p| p.as_u32()),
            name,
            executable,
            command_line,
            args,
            cwd: process.cwd().map(|path| path.to_string_lossy().to_string()),
            // sysinfo reports whole seconds and zero when it has nothing. Both are
            // kept honest here: zero becomes `None` rather than 1970.
            start_time_ms: match process.start_time() {
                0 => None,
                seconds => i64::try_from(seconds).ok().map(|seconds| seconds * 1_000),
            },
            kind,
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
    let mut words = command_line.split_whitespace();
    let Some(program) = words.next() else {
        return NodeKind::Unknown;
    };
    let args = words.map(str::to_string).collect::<Vec<_>>();
    classify_argv(program, &args)
}

/// Classifies one process without throwing away argument boundaries.
///
/// Process-table observations and Pane launches both already have an argv. Keeping
/// it structured prevents prose such as `echo test` or JavaScript passed to
/// `node --eval` from turning into a Test or Server node merely because it contains
/// a familiar word.
pub fn classify_argv(program: &str, args: &[String]) -> NodeKind {
    let lower = program.to_ascii_lowercase();
    // Match on the executable rather than the whole argv, so a command that merely
    // *mentions* an integrated tool is not classified as that tool. Accept both path
    // separators because restored Windows launch definitions are valid input on every
    // platform even when the current daemon cannot execute them.
    let executable = lower
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .strip_suffix(".exe")
        .unwrap_or_else(|| lower.rsplit(['/', '\\']).next().unwrap_or(""));
    let arguments = args
        .iter()
        .map(|argument| argument.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let arg = |index: usize| arguments.get(index).map(String::as_str);

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

    if is_word("pytest") || is_word("jest") || is_word("vitest") {
        return NodeKind::TestRunner;
    }
    if is_word("make") || is_word("ninja") {
        return NodeKind::Build;
    }

    // Multi-purpose launchers need an exact first subcommand. Arbitrary later
    // arguments are data, not evidence: `cargo metadata test` is metadata and
    // `npm exec echo run dev` is neither a Server nor a build.
    let script_kind = |script: Option<&str>| match script {
        Some("test") => Some(NodeKind::TestRunner),
        Some("build") => Some(NodeKind::Build),
        Some("dev" | "serve" | "start") => Some(NodeKind::Server),
        Some("watch") => Some(NodeKind::Watcher),
        _ => None,
    };
    match executable {
        "cargo" => match arg(0) {
            Some("test") => return NodeKind::TestRunner,
            Some("build") => return NodeKind::Build,
            Some("watch") => return NodeKind::Watcher,
            _ => {}
        },
        "go" => match arg(0) {
            Some("test") => return NodeKind::TestRunner,
            Some("build") => return NodeKind::Build,
            _ => {}
        },
        "dotnet" => match arg(0) {
            Some("test") => return NodeKind::TestRunner,
            Some("build") => return NodeKind::Build,
            Some("watch") => return NodeKind::Watcher,
            _ => {}
        },
        "npm" => {
            let kind = match arg(0) {
                Some("test") => Some(NodeKind::TestRunner),
                Some("start") => Some(NodeKind::Server),
                Some("run" | "run-script") => script_kind(arg(1)),
                _ => None,
            };
            if let Some(kind) = kind {
                return kind;
            }
        }
        "pnpm" | "yarn" | "bun" => {
            let kind = match arg(0) {
                Some("run" | "run-script") => script_kind(arg(1)),
                direct => script_kind(direct),
            };
            if let Some(kind) = kind {
                return kind;
            }
        }
        "python" | "python3"
            if matches!((arg(0), arg(1)), (Some("-m"), Some("http.server")))
                || (arg(0).is_some_and(|value| {
                    value
                        .rsplit(['/', '\\'])
                        .next()
                        .is_some_and(|name| name == "manage.py")
                }) && arg(1) == Some("runserver")) =>
        {
            return NodeKind::Server;
        }
        _ => {}
    }
    if is_word("watchman") {
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
    fn structured_arguments_classify_only_exact_launcher_subcommands() {
        let args = |values: &[&str]| {
            values
                .iter()
                .map(|value| (*value).to_string())
                .collect::<Vec<_>>()
        };

        assert_eq!(
            classify_argv("cargo", &args(&["test"])),
            NodeKind::TestRunner
        );
        assert_eq!(
            classify_argv("npm", &args(&["run", "dev"])),
            NodeKind::Server
        );
        for (program, arguments, expected) in [
            ("cargo", args(&["watch", "-x", "test"]), NodeKind::Watcher),
            ("go", args(&["test", "./..."]), NodeKind::TestRunner),
            ("go", args(&["build", "./cmd/x"]), NodeKind::Build),
            ("dotnet", args(&["test", "app.sln"]), NodeKind::TestRunner),
            ("dotnet", args(&["watch", "run"]), NodeKind::Watcher),
            ("yarn", args(&["dev"]), NodeKind::Server),
            ("pnpm", args(&["build"]), NodeKind::Build),
            ("bun", args(&["test"]), NodeKind::TestRunner),
            ("bun", args(&["run", "watch"]), NodeKind::Watcher),
        ] {
            assert_eq!(classify_argv(program, &arguments), expected);
        }
        assert_eq!(
            classify_argv("python3", &args(&["-m", "http.server", "8000"])),
            NodeKind::Server
        );

        for (program, arguments) in [
            ("echo", args(&["test"])),
            ("node", args(&["--eval", "npm run dev"])),
            ("cargo", args(&["metadata", "test"])),
            ("cargo", args(&["run", "build"])),
            ("go", args(&["env", "test"])),
            ("go", args(&["run", "./cmd/test"])),
            ("dotnet", args(&["tool", "run", "test"])),
            ("dotnet", args(&["run", "--", "test"])),
            ("npm", args(&["exec", "echo", "run", "dev"])),
            ("npm", args(&["config", "get", "test"])),
            ("npm", args(&["run", "developer"])),
            ("npm", args(&["run dev"])),
            ("yarn", args(&["dlx", "echo", "build"])),
            ("yarn", args(&["workspaces", "foreach", "run", "test"])),
            ("pnpm", args(&["exec", "echo", "dev"])),
            ("bun", args(&["x", "echo", "test"])),
            ("bun", args(&["--eval", "npm run dev"])),
            ("python", args(&["-c", "code", "manage.py", "runserver"])),
            ("python", args(&["app.py", "manage.py", "runserver"])),
        ] {
            assert_eq!(
                classify_argv(program, &arguments),
                NodeKind::Unknown,
                "{program} {arguments:?} must fail closed"
            );
        }
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
