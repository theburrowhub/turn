//! Running a command inside the pane's own shell.
//!
//! A terminal pane's process is the user's shell. An agent is a command that runs *in*
//! that shell, and the reason it matters is what happens when the agent ends: the shell
//! is still there, the prompt comes back, and the pane the user was working in is not a
//! dead rectangle. Quitting Claude Code with `/exit` in iTerm2 does not close the window,
//! and it must not close a Turn pane either.
//!
//! The shape is the plain one: the pane's process is `shell -i`, and the agent's command
//! line is *written to the pty*, exactly as if the user had typed it. Two shapes were
//! tried on real pseudo-terminals before this one was chosen, and the measurement is the
//! reason for the choice:
//!
//! * `shell -i -c '<agent>; exec <shell> -i'` looks tidier — the command is in argv, so
//!   nothing can race the shell's start-up — but **zsh exits with 130 when a command it
//!   was given with `-c` is interrupted**, interactive or not, `exec` waiting after the
//!   semicolon or not. Ctrl-C in the pane would then take the pane down along with the
//!   agent: the same report arriving by a different route. bash and `sh` survive it; zsh
//!   is the default shell on macOS.
//! * Writing the command in works on zsh, bash and `sh`, for both ctrl-C and ctrl-D, and
//!   it does not race: the bytes wait in the terminal's input queue until the shell
//!   starts reading, so they survive the rc file being sourced and the line editor being
//!   set up. Measured with a zero-millisecond delay against a shell with a slow rc.
//!
//! Typing it also has a property the argv shape does not: the command lands in the user's
//! shell history, so starting the agent again is an up-arrow. That is the same answer the
//! product gives — the pane is a terminal, and the way to start something in a terminal
//! is to type it.
//!
//! Everything that reaches the shell is quoted, because a shell is now standing between
//! Turn and the program it means to run. The agent's arguments carry a path Turn
//! generated (`--settings /…/claude-hooks.json`), and Turn's data directory can sit
//! anywhere the user put it: under `My $HOME/`, with a backtick in the name. It must
//! arrive at the agent unchanged, and it must not be able to run anything.
//!
//! What is *not* typed is the agent's environment. One of its values is a URL carrying
//! this node's hook token, and a typed line reaches the pane's screen, its scrollback and
//! the user's history file. A launch into a new shell puts that environment in the
//! shell's own environment, where it is invisible; a launch into a shell that is already
//! running writes it to a file only the user can read and tells the shell to source it.

use std::path::Path;

/// The arguments a pane's shell is started with.
///
/// Interactive, and nothing else. `-i` is what gives the pane a prompt, job control, and
/// a shell that carries on when the command running in it is interrupted.
pub(crate) fn interactive() -> Vec<String> {
    vec!["-i".to_string()]
}

/// One command line for a shell: the program and its arguments, each quoted so the shell
/// passes it through exactly as written.
pub(crate) fn command_line(command: &str, args: &[String]) -> String {
    let mut words = Vec::with_capacity(args.len() + 1);
    words.push(quote(command));
    words.extend(args.iter().map(|arg| quote(arg)));
    words.join(" ")
}

/// The bytes written to a shell's terminal to make it run one command.
///
/// The newline is the point of the function: a command line a shell has not been told to
/// run is a command sitting unexecuted on its input line.
///
/// `env_file`, when given, is sourced first so the command inherits an environment that
/// could not be typed. Separated by `;` rather than `&&` deliberately: a shell that
/// cannot read the file says so on screen and the agent still starts, because degraded
/// detection is better than no agent.
pub(crate) fn typed(command_line: &str, env_file: Option<&Path>) -> Vec<u8> {
    let line = match env_file {
        Some(path) => format!(". {}; {command_line}\n", quote(&path.to_string_lossy())),
        None => format!("{command_line}\n"),
    };
    line.into_bytes()
}

/// A command line that prints one sentence in the pane.
///
/// For the pane that has no agent to run and has to say why. `printf` rather than `echo`
/// because `echo`'s treatment of a leading `-` differs between shells, and the sentence is
/// quoted, so nothing hiding in it can become a command.
pub(crate) fn notice(note: &str) -> String {
    command_line("printf", &["%s\\n".to_string(), note.to_string()])
}

/// A shell script that exports one launch's environment.
///
/// Written to a file rather than typed, because a token that reaches the input line
/// reaches the scrollback and the user's history file with it. A name no shell can assign
/// is refused rather than written: a line a shell cannot read as an assignment is a line
/// it tries to run.
pub(crate) fn env_script(env: &[(String, String)]) -> String {
    let mut script = String::from("# Written by Turn for one agent launch. Safe to delete.\n");
    for (name, value) in env {
        if is_env_name(name) {
            script.push_str(&format!("export {name}={}\n", quote(value)));
        } else {
            tracing::warn!(
                name,
                "an adapter asked for an environment name no shell can assign; skipped"
            );
        }
    }
    script
}

/// A shell word, quoted so a shell reproduces it character for character.
///
/// `shell_words` is already a dependency of the workspace and its quoting is the
/// well-tested kind: this is not a place to hand-roll, because the failure mode of
/// getting it slightly wrong is executing something the user did not ask for.
fn quote(word: &str) -> String {
    shell_words::quote(word).into_owned()
}

/// Whether a name can appear on the left of a shell assignment.
///
/// The POSIX rule: letters, digits and underscores, not starting with a digit.
fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs a script in a directory of its own, and answers with what it printed and
    /// whether anything hiding in the arguments got as far as creating a file.
    fn run(script: &str) -> (String, bool) {
        let sandbox = tempfile::tempdir().expect("somewhere for a shell to litter");
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .current_dir(sandbox.path())
            .output()
            .expect("a shell must run");
        let escaped = sandbox.path().join("pwned").exists();
        (
            String::from_utf8_lossy(&output.stdout).into_owned(),
            escaped,
        )
    }

    /// The whole reason this module quotes: the path Turn injects is a path Turn
    /// generated, and a data directory with a space or a `$` in it must not become two
    /// arguments, a variable expansion, or a command substitution.
    #[test]
    fn an_injected_path_with_spaces_and_shell_metacharacters_stays_one_argument() {
        let hostile = "/Users/x/My $HOME/`id`/$(touch pwned)/claude-hooks.json";
        let line = command_line("claude", &["--settings".to_string(), hostile.to_string()]);
        let parsed = shell_words::split(&line).expect("the line must be one the shell parses");
        assert_eq!(
            parsed,
            vec![
                "claude".to_string(),
                "--settings".to_string(),
                hostile.to_string()
            ],
            "got {line}"
        );
    }

    /// A real shell, not only a parser: what `printf` prints is what the agent would have
    /// received as argv.
    #[test]
    fn a_real_shell_reproduces_the_arguments_exactly() {
        let hostile = "/tmp/turn scratch/$USER/`id`/$(touch pwned)/hooks.json";
        let line = command_line(
            "printf",
            &[
                "%s\n".to_string(),
                "--settings".to_string(),
                hostile.to_string(),
            ],
        );
        let (printed, escaped) = run(&line);
        assert_eq!(
            printed.lines().collect::<Vec<_>>(),
            vec!["--settings", hostile],
            "script was {line}"
        );
        assert!(!escaped, "a command substitution escaped the quoting");
    }

    /// A command a shell has not been told to run is a command sitting on its input line,
    /// which is a pane that looks like it did nothing.
    #[test]
    fn what_is_typed_ends_with_a_newline_that_runs_it() {
        assert_eq!(
            typed("claude --settings /tmp/x.json", None),
            b"claude --settings /tmp/x.json\n"
        );
    }

    /// The environment of a launch into a shell that already exists is sourced, never
    /// typed: a token on the input line is a token in the user's history file.
    #[test]
    fn an_environment_is_sourced_from_a_quoted_path_rather_than_typed() {
        let line = String::from_utf8(typed(
            "claude",
            Some(Path::new("/Users/x/My $HOME/scratch/turn-env.sh")),
        ))
        .expect("valid utf-8");
        assert_eq!(
            line, ". '/Users/x/My $HOME/scratch/turn-env.sh'; claude\n",
            "the path is quoted, and the agent still runs if sourcing fails"
        );
    }

    /// And the file a shell sources really does set the environment, with a token no
    /// shell may reinterpret on the way.
    #[test]
    fn a_sourced_environment_reaches_the_command_with_its_value_intact() {
        let hostile_url = "http://127.0.0.1:9/hook/tok'en$(touch pwned)";
        let script = env_script(&[
            ("TURN_HOOK_URL".to_string(), hostile_url.to_string()),
            ("not a name".to_string(), "refused".to_string()),
        ]);
        let sandbox = tempfile::tempdir().expect("a scratch directory");
        let file = sandbox.path().join("turn-env.sh");
        std::fs::write(&file, &script).expect("the environment file must be written");
        let line = String::from_utf8(typed("printf '%s\\n' \"$TURN_HOOK_URL\"", Some(&file)))
            .expect("valid utf-8");
        let (printed, escaped) = run(line.trim_end());
        assert_eq!(printed.trim_end(), hostile_url);
        assert!(!escaped, "a command substitution escaped the quoting");
        assert!(
            !script.contains("not a name"),
            "a name no shell can assign must not reach the file: {script}"
        );
    }

    /// The pane that has no agent to run still says why, in the place the user is
    /// looking, and the sentence cannot become a command.
    #[test]
    fn a_notice_is_printed_rather_than_executed() {
        let (printed, escaped) = run(&notice("No agent CLI found; $(touch pwned) is not run"));
        assert_eq!(
            printed.trim_end(),
            "No agent CLI found; $(touch pwned) is not run"
        );
        assert!(!escaped, "the sentence must be printed, never run");
    }

    #[test]
    fn an_empty_argument_survives_as_an_empty_argument() {
        let line = command_line("claude", &["--prompt".to_string(), String::new()]);
        let parsed = shell_words::split(&line).expect("the line must parse");
        assert_eq!(
            parsed,
            vec!["claude".to_string(), "--prompt".to_string(), String::new()]
        );
    }

    #[test]
    fn a_pane_shell_is_interactive_because_that_is_what_makes_it_a_terminal() {
        assert_eq!(interactive(), vec!["-i".to_string()]);
    }
}
