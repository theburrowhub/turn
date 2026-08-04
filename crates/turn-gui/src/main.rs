//! The Turn desktop application: a native window, drawn on the GPU.
//!
//! This binary does two things and no more: it decides where the daemon's socket is, and
//! it opens a window. It deliberately does **not** start `turnd` — the daemon's lifetime
//! is longer than the window's, which is the whole reason the daemon exists, so the
//! window connects to whatever is there and waits when nothing is.

use std::path::PathBuf;

use turn_gui::keymap::{Keymap, KeymapProblem, Overrides, Platform};
use turn_gui::transport::socket;

fn main() -> eframe::Result {
    turn_gui::logging::install();

    let socket = socket_from_arguments().unwrap_or_else(socket::socket_path_from_env);
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
            Ok(Box::new(turn_gui::app::TurnApp::new(
                &cc.egui_ctx,
                socket,
                keymap,
            )))
        }),
    )
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
