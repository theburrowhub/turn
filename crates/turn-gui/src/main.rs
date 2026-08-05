//! The Turn desktop application: a native window, drawn on the GPU.
//!
//! This binary is the product entry point. It resolves the daemon's socket, starts a
//! separate `turnd` companion when nobody is listening, and opens the window. The
//! companion is deliberately not owned by the window: closing the UI must leave its PTYs
//! and agents alive.

use std::path::PathBuf;

use turn_gui::companion::{self, EnsureOutcome};
use turn_gui::keymap::{Keymap, KeymapProblem, Overrides, Platform};
use turn_gui::transport::socket;

fn main() -> eframe::Result {
    turn_gui::logging::install();

    let explicit_socket = socket_from_arguments();
    let paths = socket::startup_paths(explicit_socket.as_deref());
    let (socket, startup_error, companion_monitor) = match paths {
        Ok(paths) => match companion::ensure(&paths.socket, &paths.data_dir) {
            Ok(EnsureOutcome::EndpointOccupied) => {
                tracing::debug!(socket = %paths.socket.display(), "using the occupied daemon endpoint");
                (paths.socket, None, None)
            }
            Ok(EnsureOutcome::Started(launch)) => {
                tracing::info!(
                    launcher_pid = launch.started.launcher_pid,
                    source = %launch.started.source,
                    program = %launch.started.program.display(),
                    log = %launch.started.log_path.display(),
                    "started the daemon companion"
                );
                (paths.socket, None, Some(launch.monitor))
            }
            Err(error) => {
                tracing::error!(%error, socket = %paths.socket.display(), "could not ensure the daemon companion");
                (
                    paths.socket,
                    Some(format!("Could not start the Turn daemon: {error}")),
                    None,
                )
            }
        },
        Err(error) => {
            tracing::error!(%error, "could not resolve Turn's startup paths");
            let socket = fallback_socket(explicit_socket.as_deref());
            (
                socket,
                Some(format!("Could not resolve Turn's storage: {error}")),
                None,
            )
        }
    };
    let (overrides, problems) = load_overrides();
    for problem in &problems {
        // Reported rather than swallowed: a binding that silently did not load looks
        // exactly like one that did not save.
        tracing::warn!(%problem, "a keyboard binding in the settings could not be read");
    }
    let keymap = Keymap::build(&overrides, Platform::detect());
    tracing::info!(socket = %socket.display(), "starting Turn");

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Turn")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([900.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Turn",
        options,
        Box::new(move |cc| {
            Ok(Box::new(turn_gui::app::TurnApp::new_with_companion(
                &cc.egui_ctx,
                socket,
                keymap,
                startup_error,
                companion_monitor,
            )))
        }),
    )
}

/// Keeps the window diagnostic-capable when the platform has no data directory. An
/// explicit socket may still name a manually managed daemon; otherwise the unique,
/// nonexistent endpoint simply lets the normal reconnect state remain visible.
fn fallback_socket(explicit: Option<&std::path::Path>) -> PathBuf {
    let raw = socket::resolve_socket_path(
        explicit,
        std::env::var_os(socket::SOCKET_ENV).as_deref(),
        &std::env::temp_dir().join(format!("turn-unresolved-{}", std::process::id())),
    );
    if raw.is_absolute() {
        raw
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(&raw))
            .unwrap_or(raw)
    }
}

/// `turn --socket /path/to/turnd.sock`, for a second daemon or a test one.
fn socket_from_arguments() -> Option<PathBuf> {
    let mut arguments = std::env::args_os().skip(1);
    while let Some(argument) = arguments.next() {
        if argument == "--socket" {
            return arguments.next().map(PathBuf::from);
        }
    }
    None
}

/// The user's keyboard overrides, from `keymap.json` in Turn's configuration directory.
///
/// A missing file is the normal case and not a problem. A file that is there but cannot
/// be read is reported, because the user wrote it on purpose.
fn load_overrides() -> (Overrides, Vec<KeymapProblem>) {
    let Some(path) = keymap_path() else {
        return (Overrides::new(), Vec::new());
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return (Overrides::new(), Vec::new());
    };
    match serde_json::from_str::<std::collections::BTreeMap<String, Option<String>>>(&text) {
        Ok(map) => Overrides::from_settings(map),
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "could not read the keyboard settings");
            (Overrides::new(), Vec::new())
        }
    }
}

fn keymap_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("dev", "turn", "turn")
        .map(|dirs| dirs.config_dir().join("keymap.json"))
}
