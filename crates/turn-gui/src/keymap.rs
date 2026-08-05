//! The keyboard map: what the commands are, how a chord is written down, and how a
//! keystroke resolves to a command.
//!
//! Four decisions worth explaining.
//!
//! **A terminal has priority.** While a terminal pane has focus almost every
//! keystroke belongs to the process. `Ctrl+C`, `Ctrl+D`, `Ctrl+U`, `Ctrl+R` are not
//! the window's to take. Only bindings marked [`Binding::overrides_terminal`] are
//! intercepted there, and the set is small and listed.
//!
//! **`Mod` rather than `Cmd` or `Ctrl`.** One table, correct on both platforms,
//! rendered as `⌘` on a Mac and `Ctrl` elsewhere. Two full copies would drift.
//!
//! **`Mod` is not symmetric, and a handful of bindings say so.** On a Mac `Mod` is
//! Command, which no terminal application can see. On Linux it *is* Control, where
//! `Mod+key` is a control character the process needs: `Mod+[` is ESC — the one byte
//! that takes `vim` out of insert mode — `Mod+K` is kill-line, `Mod+N` is
//! next-history, `Mod+]` is GS, `Mod+/` is undo. Those carry a [`Binding::pc`]
//! alternative, and
//! [`a_default_binding_never_shadows_a_control_character_a_program_needs`](tests) fails
//! if a new one appears without it. A previous frontend shipped exactly that bug,
//! with a version of the test that checked nine keys and not the tenth; this one
//! checks every key `Ctrl` turns into a C0 byte.
//!
//! **Chords are values, not strings.** The default table is built from typed
//! constructors, so a default binding cannot fail to parse. Parsing exists only for
//! a user's own overrides, which is the only place a chord arrives as text.

use egui::{Key, Modifiers};
use std::collections::BTreeMap;
use std::fmt;

/// Everything the window can be asked to do.
///
/// One flat enum: the command palette, the keymap and the menu all enumerate the
/// same list, and a command that existed in only two of the three would be a
/// feature the user cannot find.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Command {
    OpenPalette,
    ShowKeyboardShortcuts,
    OpenSettings,

    NewWorkspace,
    NewSession,
    QuickNewSession,
    SwitchSession,
    NextSession,
    PreviousSession,
    RenameSession,
    ArchiveSession,
    CloseSession,
    SaveLayoutAsTemplate,

    NextAttention,
    ToggleAttentionPanel,

    SplitHorizontal,
    SplitVertical,
    ClosePane,
    ZoomPane,
    CyclePane,
    CyclePaneBack,
    FocusPaneLeft,
    FocusPaneRight,
    FocusPaneUp,
    FocusPaneDown,

    LaunchAgent,
    LaunchShell,
    LaunchTui,
    PassContext,

    InterruptProcess,
    StopProcess,

    FocusWorkspaceTree,

    CopySelection,
    PasteClipboard,
}

impl Command {
    /// Every command, in the order the palette offers them.
    pub const ALL: &'static [Command] = &[
        Command::OpenPalette,
        Command::NewWorkspace,
        Command::NewSession,
        Command::QuickNewSession,
        Command::SwitchSession,
        Command::NextSession,
        Command::PreviousSession,
        Command::NextAttention,
        Command::ToggleAttentionPanel,
        Command::SplitHorizontal,
        Command::SplitVertical,
        Command::ClosePane,
        Command::ZoomPane,
        Command::CyclePane,
        Command::CyclePaneBack,
        Command::FocusPaneLeft,
        Command::FocusPaneRight,
        Command::FocusPaneUp,
        Command::FocusPaneDown,
        Command::LaunchAgent,
        Command::LaunchShell,
        Command::LaunchTui,
        Command::PassContext,
        Command::SaveLayoutAsTemplate,
        Command::RenameSession,
        Command::ArchiveSession,
        Command::CloseSession,
        Command::InterruptProcess,
        Command::StopProcess,
        Command::FocusWorkspaceTree,
        Command::CopySelection,
        Command::PasteClipboard,
        Command::OpenSettings,
        Command::ShowKeyboardShortcuts,
    ];

    /// The stable identifier, which is what a user's settings file names.
    pub fn id(&self) -> &'static str {
        match self {
            Command::OpenPalette => "palette.open",
            Command::ShowKeyboardShortcuts => "help.keys",
            Command::OpenSettings => "app.settings",
            Command::NewWorkspace => "workspace.new",
            Command::NewSession => "session.new",
            Command::QuickNewSession => "session.quickNew",
            Command::SwitchSession => "session.switch",
            Command::NextSession => "session.next",
            Command::PreviousSession => "session.previous",
            Command::RenameSession => "session.rename",
            Command::ArchiveSession => "session.archive",
            Command::CloseSession => "session.close",
            Command::SaveLayoutAsTemplate => "layout.saveAsTemplate",
            Command::NextAttention => "attention.next",
            Command::ToggleAttentionPanel => "attention.togglePanel",
            Command::SplitHorizontal => "pane.splitHorizontal",
            Command::SplitVertical => "pane.splitVertical",
            Command::ClosePane => "pane.close",
            Command::ZoomPane => "pane.zoom",
            Command::CyclePane => "pane.cycle",
            Command::CyclePaneBack => "pane.cycleBack",
            Command::FocusPaneLeft => "pane.focusLeft",
            Command::FocusPaneRight => "pane.focusRight",
            Command::FocusPaneUp => "pane.focusUp",
            Command::FocusPaneDown => "pane.focusDown",
            Command::LaunchAgent => "launch.agent",
            Command::LaunchShell => "launch.shell",
            Command::LaunchTui => "launch.tui",
            Command::PassContext => "agent.passContext",
            Command::InterruptProcess => "process.interrupt",
            Command::StopProcess => "process.stop",
            Command::FocusWorkspaceTree => "view.focusTree",
            Command::CopySelection => "edit.copy",
            Command::PasteClipboard => "edit.paste",
        }
    }

    /// The one line the palette and the shortcut sheet show.
    pub fn title(&self) -> &'static str {
        match self {
            Command::OpenPalette => "Command palette",
            Command::ShowKeyboardShortcuts => "Keyboard shortcuts",
            Command::OpenSettings => "Settings",
            Command::NewWorkspace => "New workspace",
            Command::NewSession => "New session — pick a template",
            Command::QuickNewSession => "Quick new session — the workspace default",
            Command::SwitchSession => "Switch session",
            Command::NextSession => "Next session",
            Command::PreviousSession => "Previous session",
            Command::RenameSession => "Rename session",
            Command::ArchiveSession => "Archive session",
            Command::CloseSession => "End session — confirm before stopping its processes",
            Command::SaveLayoutAsTemplate => "Save this layout as a template",
            Command::NextAttention => "Go to the next session that needs you",
            Command::ToggleAttentionPanel => "Show or hide the attention queue",
            Command::SplitHorizontal => "Split pane left / right",
            Command::SplitVertical => "Split pane top / bottom",
            Command::ClosePane => "Close pane",
            Command::ZoomPane => "Maximise pane (toggle)",
            Command::CyclePane => "Cycle panes",
            Command::CyclePaneBack => "Cycle panes backwards",
            Command::FocusPaneLeft => "Focus the pane to the left",
            Command::FocusPaneRight => "Focus the pane to the right",
            Command::FocusPaneUp => "Focus the pane above",
            Command::FocusPaneDown => "Focus the pane below",
            Command::LaunchAgent => "Launch an agent in this pane",
            Command::LaunchShell => "Launch a shell in this pane",
            Command::LaunchTui => "Launch a full-screen tool in this pane",
            Command::PassContext => "Pass context from the selected Agent…",
            Command::InterruptProcess => "Interrupt the process in this pane",
            Command::StopProcess => "Stop the process in this pane",
            Command::FocusWorkspaceTree => "Focus the workspace tree",
            Command::CopySelection => "Copy the selection",
            Command::PasteClipboard => "Paste",
        }
    }

    /// The palette's grouping heading.
    pub fn group(&self) -> &'static str {
        match self {
            Command::OpenPalette
            | Command::ShowKeyboardShortcuts
            | Command::OpenSettings
            | Command::FocusWorkspaceTree => "View",
            Command::NewWorkspace => "Workspace",
            Command::NewSession
            | Command::QuickNewSession
            | Command::SwitchSession
            | Command::NextSession
            | Command::PreviousSession
            | Command::RenameSession
            | Command::ArchiveSession
            | Command::CloseSession
            | Command::SaveLayoutAsTemplate => "Session",
            Command::NextAttention | Command::ToggleAttentionPanel => "Attention",
            Command::SplitHorizontal
            | Command::SplitVertical
            | Command::ClosePane
            | Command::ZoomPane
            | Command::CyclePane
            | Command::CyclePaneBack
            | Command::FocusPaneLeft
            | Command::FocusPaneRight
            | Command::FocusPaneUp
            | Command::FocusPaneDown => "Pane",
            Command::LaunchAgent
            | Command::LaunchShell
            | Command::LaunchTui
            | Command::PassContext
            | Command::InterruptProcess
            | Command::StopProcess => "Process",
            Command::CopySelection | Command::PasteClipboard => "Edit",
        }
    }

    /// Looks a command up by the identifier a settings file uses.
    pub fn from_id(id: &str) -> Option<Command> {
        Command::ALL.iter().copied().find(|c| c.id() == id)
    }
}

/// Which key `Mod` means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Platform {
    /// True where `Mod` is the Command key and Control is a modifier of its own.
    pub uses_command_key: bool,
}

impl Platform {
    pub const MAC: Platform = Platform {
        uses_command_key: true,
    };
    pub const PC: Platform = Platform {
        uses_command_key: false,
    };

    /// The platform this build is running on.
    pub fn detect() -> Platform {
        if cfg!(target_os = "macos") {
            Platform::MAC
        } else {
            Platform::PC
        }
    }
}

/// A chord: modifiers plus one key.
///
/// `command` is the abstract "the platform's own modifier"; `ctrl` is Control named
/// explicitly, which is only distinguishable from `command` on a Mac.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Chord {
    pub command: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: Key,
}

impl Chord {
    pub const fn plain(key: Key) -> Chord {
        Chord {
            command: false,
            ctrl: false,
            alt: false,
            shift: false,
            key,
        }
    }

    pub const fn cmd(key: Key) -> Chord {
        Chord {
            command: true,
            ..Chord::plain(key)
        }
    }

    pub const fn cmd_shift(key: Key) -> Chord {
        Chord {
            command: true,
            shift: true,
            ..Chord::plain(key)
        }
    }

    pub const fn cmd_alt(key: Key) -> Chord {
        Chord {
            command: true,
            alt: true,
            ..Chord::plain(key)
        }
    }

    pub const fn cmd_alt_shift(key: Key) -> Chord {
        Chord {
            command: true,
            alt: true,
            shift: true,
            ..Chord::plain(key)
        }
    }

    pub const fn alt(key: Key) -> Chord {
        Chord {
            alt: true,
            ..Chord::plain(key)
        }
    }

    /// Parses a chord written as text, for a user's own overrides.
    ///
    /// Case-insensitive in its modifiers and its key names, and it accepts both the
    /// punctuation and the name — `Mod+/` and `mod+Slash` are the same chord — so a
    /// user editing their settings does not have to know egui's spelling.
    pub fn parse(text: &str) -> Option<Chord> {
        let mut chord = Chord {
            command: false,
            ctrl: false,
            alt: false,
            shift: false,
            key: Key::Space,
        };
        let mut key: Option<Key> = None;
        for part in text.split('+').map(str::trim).filter(|p| !p.is_empty()) {
            match part.to_ascii_lowercase().as_str() {
                "mod" | "cmd" | "command" | "super" | "meta" | "win" => chord.command = true,
                "ctrl" | "control" => chord.ctrl = true,
                "alt" | "option" | "opt" => chord.alt = true,
                "shift" => chord.shift = true,
                _ => {
                    if key.is_some() {
                        // Two non-modifier keys is not a chord this map can express.
                        return None;
                    }
                    key = parse_key(part);
                    key?;
                }
            }
        }
        let key = key?;
        Some(Chord { key, ..chord })
    }

    /// Whether this chord, on this platform, arrives as a control character a
    /// running program would otherwise read.
    ///
    /// Used by the shortcut sheet to warn, and by the test that keeps the default
    /// table honest. On a Mac the answer is always false: Command is invisible to a
    /// terminal application, which is exactly why the two platforms need different
    /// answers rather than one table and hope.
    pub fn shadows_control_character(&self, platform: Platform) -> bool {
        let control_held = if platform.uses_command_key {
            self.ctrl && !self.command
        } else {
            self.ctrl || self.command
        };
        control_held && !self.alt && !self.shift && is_control_character_key(self.key)
    }

    /// Whether a keystroke satisfies this chord.
    ///
    /// On a Mac, `Mod` is Command and Control is separate, so a chord asking for
    /// `Mod` requires Command and forbids Control. Everywhere else they are the same
    /// physical key, so a chord naming either is satisfied by Control alone.
    pub fn matches(&self, key: Key, modifiers: &Modifiers, platform: Platform) -> bool {
        if self.key != key {
            return false;
        }
        let (wants_meta, wants_ctrl) = if platform.uses_command_key {
            (self.command, self.ctrl)
        } else {
            (false, self.ctrl || self.command)
        };
        wants_meta == modifiers.mac_cmd
            && wants_ctrl == modifiers.ctrl
            && self.alt == modifiers.alt
            && self.shift == modifiers.shift
    }

    /// The chord written out, in the canonical order, for a settings file.
    pub fn canonical(&self) -> String {
        let mut out = String::new();
        for (flag, name) in [
            (self.command, "Mod"),
            (self.ctrl, "Ctrl"),
            (self.alt, "Alt"),
            (self.shift, "Shift"),
        ] {
            if flag {
                out.push_str(name);
                out.push('+');
            }
        }
        out.push_str(self.key.name());
        out
    }

    /// The chord as the user should read it: glyphs on a Mac, words elsewhere.
    pub fn describe(&self, platform: Platform) -> String {
        if platform.uses_command_key {
            let mut out = String::new();
            if self.ctrl {
                out.push('⌃');
            }
            if self.alt {
                out.push('⌥');
            }
            if self.shift {
                out.push('⇧');
            }
            if self.command {
                out.push('⌘');
            }
            out.push_str(self.key.symbol_or_name());
            out
        } else {
            let mut parts: Vec<&str> = Vec::new();
            if self.command || self.ctrl {
                parts.push("Ctrl");
            }
            if self.alt {
                parts.push("Alt");
            }
            if self.shift {
                parts.push("Shift");
            }
            let key = self.key.symbol_or_name();
            parts.push(key);
            parts.join("+")
        }
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.canonical())
    }
}

/// Accepts the punctuation spellings a person writes as well as egui's own names.
fn parse_key(part: &str) -> Option<Key> {
    if let Some(key) = Key::from_name(part) {
        return Some(key);
    }
    let alias = match part.to_ascii_lowercase().as_str() {
        "bracketleft" | "openbracket" => Key::OpenBracket,
        "bracketright" | "closebracket" => Key::CloseBracket,
        "return" => Key::Enter,
        "esc" => Key::Escape,
        "backquote" | "grave" => Key::Backtick,
        "equal" => Key::Equals,
        "apostrophe" => Key::Quote,
        "spacebar" => Key::Space,
        _ => return None,
    };
    Some(alias)
}

/// Every key that Control turns into an ASCII control character a terminal
/// application reads.
///
/// The 26 letters plus the punctuation that maps into the C0 range: `Ctrl+[` is ESC,
/// `Ctrl+\` is FS, `Ctrl+]` is GS, `Ctrl+/` and `Ctrl+-` arrive as US, `Ctrl+Space`
/// is NUL. Enter, Tab and Backspace are control characters *without* Control, so
/// they are a different question and are deliberately not here — nothing binds them
/// unmodified.
pub const CONTROL_CHARACTER_KEYS: &[Key] = &[
    Key::A,
    Key::B,
    Key::C,
    Key::D,
    Key::E,
    Key::F,
    Key::G,
    Key::H,
    Key::I,
    Key::J,
    Key::K,
    Key::L,
    Key::M,
    Key::N,
    Key::O,
    Key::P,
    Key::Q,
    Key::R,
    Key::S,
    Key::T,
    Key::U,
    Key::V,
    Key::W,
    Key::X,
    Key::Y,
    Key::Z,
    Key::OpenBracket,
    Key::CloseBracket,
    Key::Backslash,
    Key::Slash,
    Key::Minus,
    Key::Space,
];

fn is_control_character_key(key: Key) -> bool {
    CONTROL_CHARACTER_KEYS.contains(&key)
}

/// One entry of the default table.
#[derive(Debug, Clone, Copy)]
pub struct Binding {
    pub command: Command,
    /// The chord where `Mod` is Command — that is, on a Mac.
    pub accelerator: Chord,
    /// What to use instead where `Mod` is Control, for the few chords whose `Mod+key`
    /// form is a control character a running program needs. Absent for everything
    /// else, which is the large majority.
    pub pc: Option<Chord>,
    /// Whether this binding is taken even when a terminal has focus. Keep the set
    /// short: everything here is a keystroke the running process will never receive.
    pub overrides_terminal: bool,
}

const fn shared(command: Command, accelerator: Chord) -> Binding {
    Binding {
        command,
        accelerator,
        pc: None,
        overrides_terminal: true,
    }
}

const fn split(command: Command, accelerator: Chord, pc: Chord) -> Binding {
    Binding {
        command,
        accelerator,
        pc: Some(pc),
        overrides_terminal: true,
    }
}

/// The default map.
///
/// `Mod+Shift+…` for anything that changes the world, plain `Mod+…` for navigation,
/// and `Alt+arrow` for moving between panes because the arrows are what a user
/// reaches for and Alt is the modifier a terminal application is least likely to
/// want.
pub const DEFAULT_BINDINGS: &[Binding] = &[
    // Ctrl+K is kill-line in readline and every shell that uses it.
    split(
        Command::OpenPalette,
        Chord::cmd(Key::K),
        Chord::cmd_shift(Key::P),
    ),
    // Ctrl+N is next-history, and next-match in half the TUIs there are.
    split(
        Command::NewSession,
        Chord::cmd(Key::N),
        Chord::cmd_alt(Key::N),
    ),
    shared(Command::QuickNewSession, Chord::cmd_shift(Key::N)),
    // Ctrl+P is previous-history.
    split(
        Command::SwitchSession,
        Chord::cmd(Key::P),
        Chord::cmd_alt(Key::P),
    ),
    shared(Command::NextSession, Chord::cmd_alt(Key::ArrowDown)),
    shared(Command::PreviousSession, Chord::cmd_alt(Key::ArrowUp)),
    shared(Command::NextAttention, Chord::cmd(Key::Enter)),
    shared(Command::ToggleAttentionPanel, Chord::cmd_shift(Key::A)),
    shared(Command::SplitHorizontal, Chord::cmd_shift(Key::Backslash)),
    shared(Command::SplitVertical, Chord::cmd_shift(Key::Minus)),
    shared(Command::ClosePane, Chord::cmd_shift(Key::W)),
    shared(Command::ZoomPane, Chord::cmd_shift(Key::Z)),
    // Ctrl+] is GS, and the tag jump in vim.
    split(
        Command::CyclePane,
        Chord::cmd(Key::CloseBracket),
        Chord::cmd_shift(Key::CloseBracket),
    ),
    // Ctrl+[ *is* ESC. Taking it from a Linux user breaks vim, every readline
    // vi-mode shell, and every TUI that reads an escape sequence — which is all of
    // them.
    split(
        Command::CyclePaneBack,
        Chord::cmd(Key::OpenBracket),
        Chord::cmd_shift(Key::OpenBracket),
    ),
    shared(Command::FocusPaneLeft, Chord::alt(Key::ArrowLeft)),
    shared(Command::FocusPaneRight, Chord::alt(Key::ArrowRight)),
    shared(Command::FocusPaneUp, Chord::alt(Key::ArrowUp)),
    shared(Command::FocusPaneDown, Chord::alt(Key::ArrowDown)),
    shared(Command::LaunchAgent, Chord::cmd_shift(Key::J)),
    shared(Command::LaunchShell, Chord::cmd_shift(Key::L)),
    shared(Command::LaunchTui, Chord::cmd_shift(Key::U)),
    shared(Command::SaveLayoutAsTemplate, Chord::cmd_shift(Key::S)),
    shared(Command::RenameSession, Chord::cmd_shift(Key::R)),
    shared(Command::ArchiveSession, Chord::cmd_shift(Key::Y)),
    shared(Command::CloseSession, Chord::cmd_shift(Key::K)),
    // Not Ctrl+C: that belongs to the process and always will. This sends the
    // interrupt through the tty to the whole foreground group.
    shared(Command::InterruptProcess, Chord::cmd_shift(Key::Period)),
    shared(Command::StopProcess, Chord::cmd_shift(Key::Comma)),
    shared(Command::FocusWorkspaceTree, Chord::cmd_shift(Key::T)),
    // Copy and paste: `Mod+C` is Command+C on a Mac, which no program sees, but
    // Ctrl+C elsewhere, which is the interrupt. Every terminal on Linux solves this
    // the same way, with Shift.
    split(
        Command::CopySelection,
        Chord::cmd(Key::C),
        Chord::cmd_shift(Key::C),
    ),
    split(
        Command::PasteClipboard,
        Chord::cmd(Key::V),
        Chord::cmd_shift(Key::V),
    ),
    shared(Command::OpenSettings, Chord::cmd_shift(Key::Semicolon)),
    // Ctrl+/ arrives as US, which is undo in readline and in emacs.
    split(
        Command::ShowKeyboardShortcuts,
        Chord::cmd(Key::Slash),
        Chord::cmd_shift(Key::Slash),
    ),
];

/// A user's overrides: a command bound to a different chord, or to nothing.
#[derive(Debug, Clone, Default)]
pub struct Overrides {
    entries: BTreeMap<Command, Option<Chord>>,
}

/// Something wrong with a user's settings, reported rather than silently ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeymapProblem {
    UnknownCommand { id: String },
    UnreadableChord { id: String, chord: String },
}

impl fmt::Display for KeymapProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeymapProblem::UnknownCommand { id } => {
                write!(f, "there is no command called {id}")
            }
            KeymapProblem::UnreadableChord { id, chord } => {
                write!(f, "{id} is bound to {chord}, which is not a chord")
            }
        }
    }
}

impl Overrides {
    pub fn new() -> Overrides {
        Overrides::default()
    }

    pub fn bind(mut self, command: Command, chord: Chord) -> Overrides {
        self.entries.insert(command, Some(chord));
        self
    }

    pub fn unbind(mut self, command: Command) -> Overrides {
        self.entries.insert(command, None);
        self
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Reads overrides out of the settings shape: command id to chord, or to
    /// nothing to unbind.
    ///
    /// Problems are returned rather than thrown away, because a binding that
    /// silently did not load looks to the user exactly like a binding that did not
    /// save.
    pub fn from_settings<I, K, V>(pairs: I) -> (Overrides, Vec<KeymapProblem>)
    where
        I: IntoIterator<Item = (K, Option<V>)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let mut overrides = Overrides::new();
        let mut problems = Vec::new();
        for (id, chord) in pairs {
            let id = id.as_ref();
            let Some(command) = Command::from_id(id) else {
                problems.push(KeymapProblem::UnknownCommand { id: id.to_string() });
                continue;
            };
            match chord {
                None => {
                    overrides = overrides.unbind(command);
                }
                Some(text) => {
                    let text = text.as_ref();
                    // An empty string unbinds, which is how a settings editor with a
                    // text field expresses "none".
                    if text.trim().is_empty() {
                        overrides = overrides.unbind(command);
                    } else if let Some(chord) = Chord::parse(text) {
                        overrides = overrides.bind(command, chord);
                    } else {
                        problems.push(KeymapProblem::UnreadableChord {
                            id: id.to_string(),
                            chord: text.to_string(),
                        });
                    }
                }
            }
        }
        (overrides, problems)
    }
}

/// One binding with its chord settled for the keyboard in front of the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bound {
    pub command: Command,
    pub chord: Chord,
    pub overrides_terminal: bool,
    /// True when the user chose this chord rather than inheriting the default.
    pub customised: bool,
}

/// The map in force, for one platform and one set of overrides.
#[derive(Debug, Clone)]
pub struct Keymap {
    platform: Platform,
    bindings: Vec<Bound>,
}

impl Keymap {
    /// Builds the map for a platform from the defaults plus a user's overrides.
    ///
    /// The platform is resolved here rather than at every read, so `resolve`, the
    /// shortcut sheet and the palette all read one field and the conflict list is
    /// the conflict list for the keyboard the user actually has. A user's own
    /// override wins on both platforms — they named a chord, and substituting a
    /// different one would be the map arguing with them.
    pub fn build(overrides: &Overrides, platform: Platform) -> Keymap {
        let mut bindings = Vec::with_capacity(DEFAULT_BINDINGS.len());
        for binding in DEFAULT_BINDINGS {
            let chosen = match overrides.entries.get(&binding.command) {
                Some(None) => continue,
                Some(Some(chord)) => (*chord, true),
                None => (
                    match (platform.uses_command_key, binding.pc) {
                        (false, Some(pc)) => pc,
                        _ => binding.accelerator,
                    },
                    false,
                ),
            };
            bindings.push(Bound {
                command: binding.command,
                chord: chosen.0,
                overrides_terminal: binding.overrides_terminal,
                customised: chosen.1,
            });
        }
        Keymap { platform, bindings }
    }

    /// The default map for this build's platform.
    pub fn defaults() -> Keymap {
        Keymap::build(&Overrides::new(), Platform::detect())
    }

    pub fn platform(&self) -> Platform {
        self.platform
    }

    pub fn bindings(&self) -> &[Bound] {
        &self.bindings
    }

    /// Two commands on one chord, so the settings screen can say so.
    ///
    /// Reported rather than resolved: a clash is a mistake the user should see, and
    /// quietly dropping one of the two would look like the binding did not save.
    pub fn conflicts(&self) -> Vec<(Chord, Vec<Command>)> {
        let mut by_chord: BTreeMap<Chord, Vec<Command>> = BTreeMap::new();
        for bound in &self.bindings {
            by_chord.entry(bound.chord).or_default().push(bound.command);
        }
        by_chord
            .into_iter()
            .filter(|(_, commands)| commands.len() > 1)
            .collect()
    }

    /// Bindings that will be invisible to a program running in a terminal, so the
    /// shortcut sheet can warn about a chord the user chose themselves.
    pub fn shadowing_the_terminal(&self) -> Vec<Bound> {
        self.bindings
            .iter()
            .copied()
            .filter(|bound| bound.chord.shadows_control_character(self.platform))
            .collect()
    }

    /// The chord bound to a command, for showing next to a menu entry.
    pub fn chord_for(&self, command: Command) -> Option<Chord> {
        self.bindings
            .iter()
            .find(|bound| bound.command == command)
            .map(|bound| bound.chord)
    }

    /// The command a keystroke triggers, or `None`.
    ///
    /// `None` for a keystroke that must be left alone — either because nothing is
    /// bound to it, or because a terminal has focus and the binding is not one of
    /// the few allowed to take a key away from a running process.
    pub fn resolve(&self, key: Key, modifiers: &Modifiers, in_terminal: bool) -> Option<Command> {
        for bound in &self.bindings {
            if !bound.chord.matches(key, modifiers, self.platform) {
                continue;
            }
            if in_terminal && !bound.overrides_terminal {
                return None;
            }
            return Some(bound.command);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Control held on a PC, where egui reports `ctrl` and `command` together
    /// because they are the same physical key.
    fn control() -> Modifiers {
        Modifiers {
            ctrl: true,
            command: true,
            ..Modifiers::default()
        }
    }

    fn mac_command() -> Modifiers {
        Modifiers {
            mac_cmd: true,
            command: true,
            ..Modifiers::default()
        }
    }

    fn mac_control() -> Modifiers {
        Modifiers {
            ctrl: true,
            ..Modifiers::default()
        }
    }

    #[test]
    fn every_command_is_bound_exactly_once_and_appears_in_the_palette() {
        for platform in [Platform::MAC, Platform::PC] {
            let keymap = Keymap::build(&Overrides::new(), platform);
            assert_eq!(
                keymap.bindings().len(),
                DEFAULT_BINDINGS.len(),
                "every default must survive being built"
            );
            assert!(
                keymap.conflicts().is_empty(),
                "clashing chords on {platform:?}: {:?}",
                keymap.conflicts()
            );
        }
        let mut ids: Vec<&str> = Command::ALL.iter().map(Command::id).collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count, "two commands share an id");
        for binding in DEFAULT_BINDINGS {
            assert!(
                Command::ALL.contains(&binding.command),
                "{:?} is bound but not offered in the palette",
                binding.command
            );
        }
        for command in Command::ALL {
            assert!(command.title().len() > 3, "{command:?} has no description");
            assert!(!command.group().is_empty());
            assert_eq!(Command::from_id(command.id()), Some(*command));
        }
    }

    #[test]
    fn the_removed_session_overview_has_no_command_palette_or_shortcut_entry() {
        assert!(Command::from_id("view.overview").is_none());
        assert!(Command::from_id("view.agentTree").is_none());
        assert!(Command::from_id("view.eventLog").is_none());
        assert_eq!(
            Command::from_id("view.focusTree"),
            Some(Command::FocusWorkspaceTree)
        );
        assert!(Command::ALL
            .iter()
            .all(|command| command.id() != "view.overview"));
        for platform in [Platform::MAC, Platform::PC] {
            let keymap = Keymap::build(&Overrides::new(), platform);
            let modifiers = if platform.uses_command_key {
                Modifiers {
                    mac_cmd: true,
                    command: true,
                    shift: true,
                    ..Modifiers::default()
                }
            } else {
                Modifiers {
                    ctrl: true,
                    command: true,
                    shift: true,
                    ..Modifiers::default()
                }
            };
            assert_eq!(keymap.resolve(Key::G, &modifiers, false), None);
        }
    }

    /// The rule that keeps a terminal a terminal, checked against every key Control
    /// turns into a C0 byte rather than against a sample of nine.
    ///
    /// The nine-key version of this test passed while a previous frontend's default
    /// map took `Ctrl+[` — ESC — away from every Linux user, because the bracket was
    /// not one of the nine.
    #[test]
    fn a_default_binding_never_shadows_a_control_character_a_program_needs() {
        for platform in [Platform::MAC, Platform::PC] {
            let keymap = Keymap::build(&Overrides::new(), platform);
            for key in CONTROL_CHARACTER_KEYS {
                let modifiers = if platform.uses_command_key {
                    mac_control()
                } else {
                    control()
                };
                assert_eq!(
                    keymap.resolve(*key, &modifiers, true),
                    None,
                    "Ctrl+{} must reach the process on {platform:?}",
                    key.name()
                );
                // Not only inside a terminal: taking ESC anywhere in the window is a
                // surprise, and the pane the user is about to click into is a terminal.
                assert_eq!(
                    keymap.resolve(*key, &modifiers, false),
                    None,
                    "Ctrl+{} must stay unbound on {platform:?}",
                    key.name()
                );
            }
            assert!(
                keymap.shadowing_the_terminal().is_empty(),
                "the defaults must not shadow anything: {:?}",
                keymap.shadowing_the_terminal()
            );
        }
    }

    /// The exceptions are exceptions, not a licence for a second map: a `pc`
    /// alternative is only allowed where the shared chord would be a control
    /// character.
    #[test]
    fn a_platform_specific_chord_exists_only_where_mod_would_be_a_control_character() {
        for binding in DEFAULT_BINDINGS {
            let shared_on_pc = binding.accelerator.shadows_control_character(Platform::PC);
            match binding.pc {
                Some(alternative) => {
                    assert!(
                        shared_on_pc,
                        "{:?} has a PC chord it does not need",
                        binding.command
                    );
                    assert!(
                        !alternative.shadows_control_character(Platform::PC),
                        "{:?}'s PC chord is itself a control character",
                        binding.command
                    );
                }
                None => assert!(
                    !shared_on_pc,
                    "{:?} would eat {} on Linux and has no alternative",
                    binding.command,
                    binding.accelerator.canonical()
                ),
            }
        }
    }

    /// The asymmetry itself: the same command reaches the user through Command on a
    /// Mac and through a shifted chord on Linux.
    #[test]
    fn cycling_panes_backwards_uses_command_on_a_mac_and_never_takes_escape_on_linux() {
        let mac = Keymap::build(&Overrides::new(), Platform::MAC);
        assert_eq!(
            mac.resolve(Key::OpenBracket, &mac_command(), true),
            Some(Command::CyclePaneBack),
            "Command+[ is invisible to a terminal program, so it is free to take"
        );

        let pc = Keymap::build(&Overrides::new(), Platform::PC);
        assert_eq!(
            pc.resolve(Key::OpenBracket, &control(), true),
            None,
            "Ctrl+[ is ESC and belongs to the process"
        );
        assert_eq!(
            pc.resolve(
                Key::OpenBracket,
                &Modifiers {
                    ctrl: true,
                    command: true,
                    shift: true,
                    ..Modifiers::default()
                },
                true
            ),
            Some(Command::CyclePaneBack),
            "the shifted chord is the Linux binding"
        );
    }

    #[test]
    fn control_and_command_are_not_confused_on_a_mac() {
        let mac = Keymap::build(&Overrides::new(), Platform::MAC);
        assert_eq!(
            mac.resolve(Key::K, &mac_command(), true),
            Some(Command::OpenPalette)
        );
        assert_eq!(
            mac.resolve(Key::K, &mac_control(), true),
            None,
            "Control+K is kill-line and is not the palette"
        );
    }

    #[test]
    fn command_n_is_the_new_session_command_even_when_a_terminal_has_focus() {
        let mac = Keymap::build(&Overrides::new(), Platform::MAC);
        assert_eq!(
            mac.resolve(Key::N, &mac_command(), true),
            Some(Command::NewSession)
        );
    }

    #[test]
    fn a_users_override_wins_on_both_platforms_and_is_marked_as_theirs() {
        let overrides = Overrides::new().bind(Command::OpenPalette, Chord::cmd_shift(Key::Space));
        for platform in [Platform::MAC, Platform::PC] {
            let keymap = Keymap::build(&overrides, platform);
            let bound = keymap
                .bindings()
                .iter()
                .find(|b| b.command == Command::OpenPalette)
                .copied()
                .expect("the palette is still bound");
            assert_eq!(bound.chord, Chord::cmd_shift(Key::Space));
            assert!(
                bound.customised,
                "the settings screen must be able to say so"
            );
        }
    }

    #[test]
    fn an_unbound_command_resolves_to_nothing_but_stays_in_the_palette() {
        let keymap = Keymap::build(&Overrides::new().unbind(Command::ZoomPane), Platform::MAC);
        assert_eq!(keymap.chord_for(Command::ZoomPane), None);
        assert_eq!(
            keymap.resolve(
                Key::Z,
                &Modifiers {
                    mac_cmd: true,
                    command: true,
                    shift: true,
                    ..Modifiers::default()
                },
                true
            ),
            None
        );
        assert!(
            Command::ALL.contains(&Command::ZoomPane),
            "unbinding a shortcut must not remove the command from the palette"
        );
    }

    /// A user is allowed to bind a control character — they asked — but the shortcut
    /// sheet has to be able to tell them what it will cost.
    #[test]
    fn an_override_that_steals_a_control_character_is_allowed_and_reported() {
        let overrides = Overrides::new().bind(Command::ZoomPane, Chord::cmd(Key::R));
        let keymap = Keymap::build(&overrides, Platform::PC);
        assert_eq!(
            keymap.resolve(Key::R, &control(), true),
            Some(Command::ZoomPane),
            "the user's own choice is honoured"
        );
        let shadowing = keymap.shadowing_the_terminal();
        assert_eq!(shadowing.len(), 1);
        assert_eq!(shadowing[0].command, Command::ZoomPane);
    }

    #[test]
    fn chords_parse_from_the_spellings_a_person_writes() {
        assert_eq!(Chord::parse("Mod+K"), Some(Chord::cmd(Key::K)));
        assert_eq!(Chord::parse("mod+shift+p"), Some(Chord::cmd_shift(Key::P)));
        assert_eq!(Chord::parse("MOD+SHIFT+P"), Some(Chord::cmd_shift(Key::P)));
        assert_eq!(Chord::parse("Mod+/"), Some(Chord::cmd(Key::Slash)));
        assert_eq!(Chord::parse("Mod+Slash"), Some(Chord::cmd(Key::Slash)));
        assert_eq!(
            Chord::parse("mod+BracketLeft"),
            Some(Chord::cmd(Key::OpenBracket))
        );
        assert_eq!(Chord::parse("Alt+Left"), Some(Chord::alt(Key::ArrowLeft)));
        assert_eq!(Chord::parse("F5"), Some(Chord::plain(Key::F5)));
        // Two keys is not a chord, and neither is a chord with no key.
        assert_eq!(Chord::parse("Mod+A+B"), None);
        assert_eq!(Chord::parse("Mod+Shift"), None);
        assert_eq!(Chord::parse(""), None);
        assert_eq!(Chord::parse("Mod+Nonsense"), None);
    }

    #[test]
    fn a_chord_round_trips_through_its_canonical_spelling() {
        for chord in [
            Chord::cmd(Key::K),
            Chord::cmd_alt_shift(Key::ArrowUp),
            Chord::plain(Key::F12),
            Chord::alt(Key::Slash),
        ] {
            assert_eq!(
                Chord::parse(&chord.canonical()),
                Some(chord),
                "{} did not survive being written down",
                chord.canonical()
            );
        }
    }

    #[test]
    fn a_chord_reads_as_glyphs_on_a_mac_and_as_words_elsewhere() {
        let chord = Chord::cmd_shift(Key::P);
        assert_eq!(chord.describe(Platform::MAC), "⇧⌘P");
        assert_eq!(chord.describe(Platform::PC), "Ctrl+Shift+P");
    }

    #[test]
    fn unreadable_settings_are_reported_rather_than_silently_dropped() {
        let (overrides, problems) = Overrides::from_settings([
            ("pane.zoom", Some("Mod+Shift+Q")),
            ("pane.nonsense", Some("Mod+Q")),
            ("pane.close", Some("Mod+Shift+@@@")),
        ]);
        assert_eq!(
            overrides.entries.get(&Command::ZoomPane),
            Some(&Some(Chord::cmd_shift(Key::Q)))
        );
        assert_eq!(
            problems,
            vec![
                KeymapProblem::UnknownCommand {
                    id: "pane.nonsense".into()
                },
                KeymapProblem::UnreadableChord {
                    id: "pane.close".into(),
                    chord: "Mod+Shift+@@@".into()
                }
            ]
        );
        // And each problem says which binding it is about, so a user can fix it.
        for problem in &problems {
            assert!(problem.to_string().contains("pane."), "{problem}");
        }
    }

    #[test]
    fn an_empty_chord_in_the_settings_means_unbind_rather_than_a_broken_binding() {
        let (overrides, problems) = Overrides::from_settings([("pane.zoom", Some("  "))]);
        assert!(problems.is_empty());
        let keymap = Keymap::build(&overrides, Platform::MAC);
        assert_eq!(keymap.chord_for(Command::ZoomPane), None);
    }

    /// Everything that changes the world takes a Shift, so a mistyped navigation
    /// chord cannot archive a session.
    #[test]
    fn the_destructive_commands_all_need_a_shift() {
        let keymap = Keymap::build(&Overrides::new(), Platform::MAC);
        for command in [
            Command::CloseSession,
            Command::ArchiveSession,
            Command::ClosePane,
            Command::StopProcess,
            Command::InterruptProcess,
        ] {
            let chord = keymap
                .chord_for(command)
                .unwrap_or_else(|| panic!("{command:?} must be bound"));
            assert!(chord.shift, "{command:?} is one keystroke from a mistake");
        }
    }
}
