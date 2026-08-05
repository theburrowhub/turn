//! Turn's desktop interface, drawn on the GPU.
//!
//! There is no webview here. The window is native, the widgets are drawn by
//! `egui` through `wgpu`, and the same code runs on macOS and Linux — which makes
//! platform parity a property of the build rather than of discipline.
//!
//! ## Why the daemon sends cells, not bytes
//!
//! The daemon already keeps an authoritative parsed screen per Pane for attached
//! clients and output heuristics. A Rust client can consume that directly, so a terminal Pane paints a grid
//! of cells with their colours and attributes rather than re-parsing an escape
//! stream. That removes the second VT emulator the previous frontend needed, and
//! with it the whole class of "the two screens disagree" bug.

pub mod activity;
pub mod announce;
pub mod app;
pub mod companion;
pub mod desk;
pub mod keymap;
pub mod logging;
pub mod palette;
pub mod panes;
pub mod repaint;
pub mod terminal;
pub mod theme;
pub mod transport;
pub mod view;

pub use theme::Theme;
pub use turn_proto::cells;
pub use turn_proto::{Cell, CellAttrs, Grid, Modes, MouseMode, Rgb};
pub use view::TurnView;
