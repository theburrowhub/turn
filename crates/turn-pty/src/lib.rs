//! PTY management, process supervision and terminal buffers.
pub mod buffer;
pub mod process;
pub mod supervisor;

pub use buffer::{
    is_display_safe, sanitise_label, ScreenSize, ScreenSnapshot, TerminalBuffer, MAX_TITLE_CHARS,
};
pub use process::{ExitInfo, OutputChunk, ProcessSpec, PtyError, PtyProcess};
pub use supervisor::{classify, ObservedProcess, ProcessSupervisor};
