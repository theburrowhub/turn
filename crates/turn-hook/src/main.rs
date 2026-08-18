//! Entry point for the `turn-hook` helper.
//!
//! The whole binary is "do the thing, then exit 0". See the crate docs in
//! `lib.rs` for why the exit code is unconditional.

use std::time::Duration;
use turn_hook::{run, run_status_line, Options, StatusLineOptions, DEFAULT_TIMEOUT_MS};

fn main() {
    let arguments: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if arguments.len() == 1 && matches!(arguments[0].to_str(), Some("-V" | "--version")) {
        println!("turn-hook {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if arguments.len() == 1 && arguments[0] == "--build-info" {
        println!("component=turn-hook version={}", env!("CARGO_PKG_VERSION"));
        return;
    }

    if arguments.first().and_then(|arg| arg.to_str()) == Some("--statusline-forward") {
        let timeout = std::env::var("TURN_STATUSLINE_TIMEOUT_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(DEFAULT_TIMEOUT_MS)
            .clamp(1, 30_000);
        let options = Options::parse(
            arguments
                .into_iter()
                .skip(1)
                .filter_map(|arg| arg.into_string().ok()),
            std::env::var("TURN_STATUSLINE_URL").ok(),
            std::env::var_os("TURN_HOOK_DEBUG").is_some(),
        );
        let options = Options {
            timeout: Duration::from_millis(timeout),
            ..options
        };
        let mut stdin = std::io::stdin().lock();
        if let Err(failure) = run(&options, &mut stdin) {
            if options.debug {
                eprintln!("turn-hook: status-line forward failed: {failure}");
            }
        }
        std::process::exit(0);
    }

    if arguments.first().and_then(|arg| arg.to_str()) == Some("--statusline") {
        let original_script = arguments.get(1).map(std::path::PathBuf::from);
        let options = StatusLineOptions {
            url: std::env::var("TURN_STATUSLINE_URL").ok(),
            original_script,
            forwarder_exe: std::env::current_exe().unwrap_or_else(|_| "turn-hook".into()),
            timeout: Duration::from_millis(100),
            debug: std::env::var_os("TURN_HOOK_DEBUG").is_some(),
        };
        let mut stdin = std::io::stdin().lock();
        let mut stdout = std::io::stdout().lock();
        match run_status_line(&options, &mut stdin, &mut stdout) {
            Ok(code) => std::process::exit(code),
            Err(failure) => {
                if options.debug {
                    eprintln!("turn-hook: status-line fan-out failed: {failure}");
                }
                std::process::exit(1);
            }
        }
    }

    let options = Options::from_process();
    let mut stdin = std::io::stdin().lock();

    if let Err(failure) = run(&options, &mut stdin) {
        if options.debug {
            // Only ever on request: unsolicited stderr from a hook lands in the
            // middle of the user's agent output.
            eprintln!("turn-hook: {failure}");
        }
    }

    // Explicit and unconditional. An agent that treats a failing hook as a
    // problem must never have one because Turn's daemon was not running.
    std::process::exit(0);
}
