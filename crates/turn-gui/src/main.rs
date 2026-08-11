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
    if handle_release_command() {
        return Ok(());
    }
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

/// Small non-window commands used by packaging and the installed updater.
///
/// They are handled before logging, companion discovery and `eframe`, so asking a
/// bundle what it contains can never start a daemon or flash a window.
fn handle_release_command() -> bool {
    let arguments: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    match release_command(&arguments) {
        Ok(None) => false,
        Ok(Some(ReleaseCommand::Version)) => {
            println!("turn {}", env!("CARGO_PKG_VERSION"));
            true
        }
        Ok(Some(ReleaseCommand::BuildInfo)) => {
            println!(
                "component=turn version={} protocol_min={} protocol_max={}",
                env!("CARGO_PKG_VERSION"),
                turn_proto::MIN_PROTOCOL_VERSION,
                turn_proto::PROTOCOL_VERSION
            );
            true
        }
        Ok(Some(ReleaseCommand::UpdateStatus { socket })) => {
            let socket = match socket {
                Some(socket) => absolute_argument(socket),
                None => match socket::startup_paths(None) {
                    Ok(paths) => paths.socket,
                    Err(error) => {
                        eprintln!("turn: could not resolve the daemon socket: {error}");
                        std::process::exit(2);
                    }
                },
            };
            match turn_gui::update::query_update_status(&socket) {
                Ok(report) => println!("{}", update_status_line(&report)),
                Err(error) if error.is_unavailable() => {
                    eprintln!("turn: {error}");
                    std::process::exit(3);
                }
                Err(error) => {
                    eprintln!("turn: {error}");
                    std::process::exit(2);
                }
            }
            true
        }
        Err(error) => {
            eprintln!("turn: {error}");
            std::process::exit(2);
        }
    }
}

fn update_status_line(report: &turn_gui::update::DaemonUpdateReport) -> String {
    format!(
        "daemon_running={} daemon_version={} daemon_pid={} protocol_min={} protocol_max={} active_ptys={}",
        report.daemon_running,
        report.daemon_version,
        report.daemon_pid,
        report.protocol_min,
        report.protocol_max,
        report.active_ptys
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ReleaseCommand {
    Version,
    BuildInfo,
    UpdateStatus { socket: Option<PathBuf> },
}

fn release_command(arguments: &[std::ffi::OsString]) -> Result<Option<ReleaseCommand>, String> {
    if arguments.len() == 1 && matches!(arguments[0].to_str(), Some("-V" | "--version")) {
        return Ok(Some(ReleaseCommand::Version));
    }
    if arguments.len() == 1 && arguments[0] == "--build-info" {
        return Ok(Some(ReleaseCommand::BuildInfo));
    }
    if !arguments
        .iter()
        .any(|argument| argument == "--update-status")
    {
        return Ok(None);
    }

    let mut socket = None;
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        if argument == "--update-status" {
            index += 1;
            continue;
        }
        if argument == "--socket" {
            let value = arguments
                .get(index + 1)
                .ok_or_else(|| "`--socket` needs a path".to_string())?;
            if value.to_string_lossy().starts_with('-') {
                return Err("`--socket` needs a path".to_string());
            }
            socket = Some(PathBuf::from(value));
            index += 2;
            continue;
        }
        if let Some(raw) = argument
            .to_str()
            .and_then(|value| value.strip_prefix("--socket="))
        {
            if raw.is_empty() {
                return Err("`--socket` needs a path".to_string());
            }
            socket = Some(PathBuf::from(raw));
            index += 1;
            continue;
        }
        return Err(format!(
            "unrecognised update-status argument `{}`",
            argument.to_string_lossy()
        ));
    }
    Ok(Some(ReleaseCommand::UpdateStatus { socket }))
}

fn absolute_argument(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map(|directory| directory.join(&path))
            .unwrap_or(path)
    }
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

    #[test]
    fn release_commands_are_windowless_and_update_status_parses_both_socket_spellings() {
        use std::ffi::OsString;

        assert_eq!(
            release_command(&[OsString::from("--build-info")]).unwrap(),
            Some(ReleaseCommand::BuildInfo)
        );

        let status = update_status_line(&turn_gui::update::DaemonUpdateReport {
            daemon_running: true,
            daemon_version: "0.1.0".into(),
            daemon_pid: 42,
            protocol_min: 4,
            protocol_max: 4,
            active_ptys: 3,
        });
        assert_eq!(
            status,
            "daemon_running=true daemon_version=0.1.0 daemon_pid=42 protocol_min=4 protocol_max=4 active_ptys=3"
        );
        assert_eq!(
            release_command(&[
                OsString::from("--update-status"),
                OsString::from("--socket"),
                OsString::from("/tmp/turn.sock"),
            ])
            .unwrap(),
            Some(ReleaseCommand::UpdateStatus {
                socket: Some(PathBuf::from("/tmp/turn.sock")),
            })
        );
        assert_eq!(
            release_command(&[
                OsString::from("--socket=/tmp/turn.sock"),
                OsString::from("--update-status"),
            ])
            .unwrap(),
            Some(ReleaseCommand::UpdateStatus {
                socket: Some(PathBuf::from("/tmp/turn.sock")),
            })
        );
        assert!(release_command(&[
            OsString::from("--update-status"),
            OsString::from("--socket"),
        ])
        .is_err());
        assert_eq!(
            release_command(&[OsString::from("--socket"), OsString::from("/tmp/gui.sock")])
                .unwrap(),
            None,
            "the ordinary GUI socket argument is still handled by startup"
        );
    }
}
