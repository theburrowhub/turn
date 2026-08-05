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

const APP_ID: &str = "io.github.theburrowhub.turn";
const APP_ICON_PNG: &[u8] = include_bytes!("../assets/turn-icon.png");

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

    let options = native_options();
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

fn native_options() -> eframe::NativeOptions {
    eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Turn")
            .with_app_id(APP_ID)
            .with_icon(embedded_app_icon())
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([900.0, 560.0]),
        ..Default::default()
    }
}

/// Decode the checked-in master once during startup. `eframe` uses this for the
/// macOS Dock application icon as well as the platform window/task-switcher icon.
fn embedded_app_icon() -> eframe::egui::IconData {
    eframe::icon_data::from_png_bytes(APP_ICON_PNG)
        .expect("the embedded Turn app icon must be a valid PNG")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_app_icon_is_a_square_rgba_master() {
        let options = native_options();
        let icon = options
            .viewport
            .icon
            .expect("the native window must receive the Turn app icon");

        assert_eq!((icon.width, icon.height), (1024, 1024));
        assert_eq!(icon.rgba.len(), 1024 * 1024 * 4);
        assert_eq!(options.viewport.app_id.as_deref(), Some(APP_ID));
        assert!(
            icon.rgba.chunks_exact(4).any(|pixel| pixel[3] == 0),
            "the icon must keep transparent corners instead of an opaque black canvas"
        );
        assert!(
            icon.rgba.chunks_exact(4).any(|pixel| pixel[3] == 255),
            "the icon artwork must contain fully opaque pixels"
        );
    }
}
