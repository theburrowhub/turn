//! Entry point for the `turn-hook` helper.
//!
//! The whole binary is "do the thing, then exit 0". See the crate docs in
//! `lib.rs` for why the exit code is unconditional.

use turn_hook::{run, Options};

fn main() {
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
