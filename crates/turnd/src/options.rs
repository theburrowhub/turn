//! Command-line parsing.
//!
//! Hand-rolled rather than pulling in an argument parser. The daemon has five
//! flags, a dependency would be the largest thing in this crate's tree, and the
//! parsing rules worth having — a missing value is an error rather than a silently
//! consumed next flag — fit in one function that can be tested directly.

use crate::error::{DaemonError, Result};
use std::path::PathBuf;

/// What the operator asked for on the command line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Options {
    /// Overrides the data directory (and, unless `--socket` is given, the socket's
    /// directory on platforms with no runtime directory).
    pub data_dir: Option<PathBuf>,
    /// Overrides the socket path.
    pub socket: Option<PathBuf>,
    /// Tracing filter: `error`, `warn`, `info`, `debug`, `trace`, or any
    /// `RUST_LOG`-style directive.
    pub log_level: Option<String>,
    /// Run with an in-memory store. Nothing survives the process.
    pub no_persist: bool,
    /// Print help and exit.
    pub help: bool,
    /// Print the version and exit.
    pub version: bool,
}

/// The usage text, printed for `--help` and for a bad invocation.
pub const USAGE: &str = "\
turnd — the Turn daemon: owns every pty, all state and the attention queue

Usage: turnd [options]

Options:
      --socket <PATH>     Unix socket to listen on (default: $TURN_SOCKET, or
                          turnd.sock in the runtime or data directory)
      --data-dir <PATH>   Where the database and scratch config live
                          (default: $TURN_DATA_DIR, or the platform data directory)
      --log-level <LEVEL> error | warn | info | debug | trace, or a RUST_LOG filter
                          (default: $RUST_LOG, or info)
      --no-persist        Keep state in memory only; nothing survives this process
  -h, --help              Print this help
  -V, --version           Print the version
";

impl Options {
    /// Parses arguments, excluding the program name.
    pub fn parse<I, S>(args: I) -> Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut options = Options::default();
        let mut args = args.into_iter().map(Into::into);

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--socket" => options.socket = Some(PathBuf::from(value(&mut args, "--socket")?)),
                "--data-dir" => {
                    options.data_dir = Some(PathBuf::from(value(&mut args, "--data-dir")?))
                }
                "--log-level" => options.log_level = Some(value(&mut args, "--log-level")?),
                "--no-persist" => options.no_persist = true,
                "-h" | "--help" => options.help = true,
                "-V" | "--version" => options.version = true,
                // `--flag=value` as well as `--flag value`, because both are what
                // people type and rejecting one is a pointless surprise.
                other => match other.split_once('=') {
                    Some(("--socket", raw)) => options.socket = Some(PathBuf::from(raw)),
                    Some(("--data-dir", raw)) => options.data_dir = Some(PathBuf::from(raw)),
                    Some(("--log-level", raw)) => options.log_level = Some(raw.to_string()),
                    _ => {
                        return Err(DaemonError::usage(format!(
                            "unrecognised argument `{other}`\n\n{USAGE}"
                        )))
                    }
                },
            }
        }

        Ok(options)
    }

    /// Parses the current process's arguments.
    pub fn from_env() -> Result<Self> {
        Self::parse(std::env::args().skip(1))
    }
}

/// Takes the value that follows a flag, refusing to swallow the next flag.
fn value<I: Iterator<Item = String>>(args: &mut I, flag: &str) -> Result<String> {
    match args.next() {
        Some(raw) if !raw.starts_with('-') => Ok(raw),
        Some(raw) => Err(DaemonError::usage(format!(
            "`{flag}` needs a value, but was followed by `{raw}`"
        ))),
        None => Err(DaemonError::usage(format!("`{flag}` needs a value"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_flag_parses_in_both_spellings() {
        let spaced = Options::parse([
            "--socket",
            "/tmp/a.sock",
            "--data-dir",
            "/tmp/d",
            "--log-level",
            "debug",
        ])
        .unwrap();
        let joined = Options::parse([
            "--socket=/tmp/a.sock",
            "--data-dir=/tmp/d",
            "--log-level=debug",
        ])
        .unwrap();
        assert_eq!(spaced, joined);
        assert_eq!(spaced.socket, Some(PathBuf::from("/tmp/a.sock")));
        assert_eq!(spaced.data_dir, Some(PathBuf::from("/tmp/d")));
        assert_eq!(spaced.log_level.as_deref(), Some("debug"));
    }

    #[test]
    fn a_flag_missing_its_value_does_not_swallow_the_next_flag() {
        // `turnd --socket --no-persist` must not end up listening on a socket
        // called "--no-persist".
        let error = Options::parse(["--socket", "--no-persist"]).expect_err("must be refused");
        assert!(error.to_string().contains("needs a value"), "{error}");
        assert!(Options::parse(["--log-level"]).is_err());
    }

    #[test]
    fn an_unknown_flag_is_refused_with_the_usage_text() {
        let error = Options::parse(["--dance"]).expect_err("must be refused");
        let message = error.to_string();
        assert!(message.contains("--dance"));
        assert!(
            message.contains("Usage: turnd"),
            "usage must be shown: {message}"
        );
    }

    #[test]
    fn help_and_version_are_recognised_short_and_long() {
        assert!(Options::parse(["-h"]).unwrap().help);
        assert!(Options::parse(["--help"]).unwrap().help);
        assert!(Options::parse(["-V"]).unwrap().version);
        assert!(Options::parse(["--version"]).unwrap().version);
        assert!(Options::parse(["--no-persist"]).unwrap().no_persist);
    }

    #[test]
    fn no_arguments_means_every_default() {
        let options = Options::parse(Vec::<String>::new()).unwrap();
        assert_eq!(options, Options::default());
        assert!(!options.no_persist);
    }
}
