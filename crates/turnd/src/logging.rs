//! Tracing setup.
//!
//! Structured, to stderr. Stderr rather than stdout because a daemon started by the
//! UI may have its stdout read as protocol by something later, and mixing a log line
//! into a byte stream is a bug that takes a long afternoon to find.

use tracing_subscriber::EnvFilter;

/// Installs the subscriber. Called once, from `main`.
///
/// Precedence is `--log-level`, then `RUST_LOG`, then `info`. A `--log-level` value
/// that is not a valid filter falls back to `info` with a warning rather than
/// refusing to start: losing the requested verbosity is a smaller problem than a
/// daemon that will not run because of a typo in a log setting.
pub fn init(requested: Option<&str>) {
    let filter = match requested {
        Some(level) => EnvFilter::try_new(level).unwrap_or_else(|error| {
            eprintln!("turnd: ignoring --log-level {level:?} ({error}); using info");
            EnvFilter::new("info")
        }),
        None => EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
    };

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(true)
        .init();
}
