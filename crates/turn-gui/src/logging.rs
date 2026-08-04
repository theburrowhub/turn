//! Where the window's log goes.
//!
//! Off unless asked for. A desktop application that wrote a line per frame to stderr
//! would be unusable from a terminal, and `RUST_LOG=turn_gui=debug` is what somebody
//! debugging a connection actually reaches for.

/// Installs the log subscriber. Safe to call once; a second call is ignored.
pub fn install() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    // `try_init` rather than `init`: a test binary may already have one, and taking the
    // window down over a log subscriber would be absurd.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .try_init();
}
