//! The daemon's entry point.
//!
//! Deliberately thin: parse arguments, set up logging, resolve the configuration, start
//! the daemon and hand control to the operating system. Everything else is in the
//! library, which is what lets the integration tests drive a real daemon over a real
//! socket instead of asserting against a mock of one.

use turnd::{logging, Config, DaemonError, Options};

fn main() -> std::process::ExitCode {
    let options = match Options::from_env() {
        Ok(options) => options,
        Err(error) => {
            eprintln!("turnd: {error}");
            return std::process::ExitCode::from(2);
        }
    };

    if options.help {
        print!("{}", turnd::options::USAGE);
        return std::process::ExitCode::SUCCESS;
    }
    if options.version {
        println!("turnd {}", turnd::DAEMON_VERSION);
        return std::process::ExitCode::SUCCESS;
    }
    if options.build_info {
        println!(
            "component=turnd version={} protocol_min={} protocol_max={}",
            turnd::DAEMON_VERSION,
            turn_proto::MIN_PROTOCOL_VERSION,
            turn_proto::PROTOCOL_VERSION
        );
        return std::process::ExitCode::SUCCESS;
    }

    logging::init(options.log_level.as_deref());

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("turnd: could not start the async runtime: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };

    runtime.block_on(async move {
        let config = match Config::from_options(&options) {
            Ok(config) => config,
            Err(error) => return report(error),
        };
        let daemon = match turnd::start(config).await {
            Ok(daemon) => daemon,
            Err(error) => return report(error),
        };
        daemon.run_until_signal().await;
        std::process::ExitCode::SUCCESS
    })
}

/// Prints a start-up failure and picks an exit code that means something.
///
/// Contention gets its own code so a supervisor — or the UI, which starts the daemon on
/// demand — can tell "someone else is already serving, carry on and connect to it" from
/// "this will not work, tell the user".
fn report(error: DaemonError) -> std::process::ExitCode {
    eprintln!("turnd: {error}");
    if error.is_contention() {
        std::process::ExitCode::from(3)
    } else {
        std::process::ExitCode::FAILURE
    }
}
