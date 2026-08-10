//! The pane's own context menu.
//!
//! Copy and paste worked before this module existed, and no user could have known: the
//! only way to reach them was a chord nothing in the window mentioned. A feature that is
//! implemented and undiscoverable is, from where the user sits, a feature that is absent.
//! The tree rows have had context menus for a long time; the thing people spend the day
//! inside had none.
//!
//! Three rules shape it, and they are the reasons it is not just a list of buttons.
//!
//! ## Every item shows its chord
//!
//! The menu is where a user learns the keyboard, so an item with a shortcut always shows
//! it. Five of the nine are window commands and their chords come from
//! [`crate::keymap::Keymap`] — including the user's own overrides, so the menu never
//! teaches a chord the user has rebound. The remaining four are the terminal's own
//! ([`PANE_CHORDS`]), because the window's command list has no entry for them; the test
//! `the_terminal_owns_no_chord_the_window_has_already_taken` keeps the two tables from
//! colliding on either platform.
//!
//! ## Unavailable says why
//!
//! An item that cannot be used is **greyed with its reason next to it**, never hidden. A
//! menu that silently loses "Copy" leaves the user wondering whether the terminal can copy
//! at all; a greyed "Copy" that says *nothing is selected* has taught them how to use it.
//! The reason is also the item's accessible name, because a screen-reader user cannot hover
//! a tooltip.
//!
//! ## The menu is not the only way in
//!
//! It opens on right-click, and on [`MENU_CHORD`] — Shift+F10, the platform standard —
//! because a context menu reachable only with a pointer is unusable for exactly the people
//! who need a menu most. Arrow keys move through it, Enter chooses, Escape closes: that is
//! `egui`'s own focus handling, and the pane only has to put the first item in focus when
//! the menu was opened from the keyboard.

use egui::{Key, Modifiers, RichText, Ui};

use crate::cells::Grid;
use crate::keymap::{Chord, Command, Keymap, Platform};
use crate::theme::Theme;

use super::links::{normalise_url, LinkMap, LinkRequest, LinkTarget};
use super::selection::{CellPos, Selection};

/// Everything the pane's context menu can do.
///
/// One list, in the order the menu shows them: the edit verbs, then the two that act on
/// what is under the pointer, then the structural ones. Splits and Close come last because
/// they are the items with consequences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneCommand {
    Copy,
    Paste,
    SelectAll,
    ClearBuffer,
    SearchSelection,
    OpenLink,
    SplitHorizontally,
    SplitVertically,
    ClosePane,
}

impl PaneCommand {
    /// Every command, in menu order.
    pub const ALL: &'static [PaneCommand] = &[
        PaneCommand::Copy,
        PaneCommand::Paste,
        PaneCommand::SelectAll,
        PaneCommand::ClearBuffer,
        PaneCommand::SearchSelection,
        PaneCommand::OpenLink,
        PaneCommand::SplitHorizontally,
        PaneCommand::SplitVertically,
        PaneCommand::ClosePane,
    ];

    /// The words on the item.
    pub fn label(&self) -> &'static str {
        match self {
            PaneCommand::Copy => "Copy",
            PaneCommand::Paste => "Paste",
            PaneCommand::SelectAll => "Select All",
            PaneCommand::ClearBuffer => "Clear Buffer",
            PaneCommand::SearchSelection => "Search for Selection",
            PaneCommand::OpenLink => "Open Link",
            PaneCommand::SplitHorizontally => "Split Horizontally",
            PaneCommand::SplitVertically => "Split Vertically",
            PaneCommand::ClosePane => "Close Pane",
        }
    }

    /// The window command this item is, for the four-fifths of the menu the window's
    /// keymap already names.
    ///
    /// `None` for the items the terminal owns itself. Those are the ones with no entry in
    /// [`Command`], and inventing one here would be a second command list for the palette
    /// and the shortcut sheet to disagree with.
    pub fn window_command(&self) -> Option<Command> {
        match self {
            PaneCommand::Copy => Some(Command::CopySelection),
            PaneCommand::Paste => Some(Command::PasteClipboard),
            PaneCommand::SplitHorizontally => Some(Command::SplitHorizontal),
            PaneCommand::SplitVertically => Some(Command::SplitVertical),
            PaneCommand::ClosePane => Some(Command::ClosePane),
            PaneCommand::SelectAll
            | PaneCommand::ClearBuffer
            | PaneCommand::SearchSelection
            | PaneCommand::OpenLink => None,
        }
    }

    /// Whether the pane itself handles this keystroke.
    ///
    /// The pane may only take a chord the window has not already consumed. Copy and paste
    /// are the interesting exclusions: their key events are taken by the window's keymap
    /// before a pane is drawn, and the pane hears about them as `egui`'s own
    /// [`egui::Event::Copy`] and [`egui::Event::Paste`] — which is also what makes the
    /// platform's own menu bar work.
    pub fn owned_by_pane(&self) -> bool {
        self.window_command().is_none()
    }

    /// Whether choosing this item acts on the pane it was opened on, so the pane has to be
    /// focused first.
    ///
    /// Right-clicking does not move focus — a menu that stole focus on the way to being
    /// dismissed would be its own bug — but *choosing* an item is an explicit act, and
    /// "Split Horizontally" has to split the pane the user pointed at rather than whichever
    /// one happened to be focused.
    pub fn needs_focus(&self) -> bool {
        matches!(
            self,
            PaneCommand::Paste
                | PaneCommand::SelectAll
                | PaneCommand::ClearBuffer
                | PaneCommand::SplitHorizontally
                | PaneCommand::SplitVertically
                | PaneCommand::ClosePane
        )
    }

    /// Whether a rule is drawn above this item, grouping the menu into edit, pointer and
    /// structure.
    fn starts_a_group(&self) -> bool {
        matches!(
            self,
            PaneCommand::SearchSelection | PaneCommand::SplitHorizontally
        )
    }
}

/// The chords for the four items the window's command list does not name.
///
/// Each is `Mod+Shift+…` on a free letter, which puts them in the same shape as every
/// other pane binding — and on a Mac `Mod` is Command, which a program running in the
/// terminal never sees at all. The mnemonic letters were taken: `A` is the attention panel
/// and `Mod+A` on a PC is *beginning-of-line*, `C` is copy, `S` is save-as-template. So
/// "select **e**verything", "clear the **b**uffer", "**f**ind", "**o**pen".
pub const PANE_CHORDS: &[(PaneCommand, Chord)] = &[
    (PaneCommand::SelectAll, Chord::cmd_shift(Key::E)),
    (PaneCommand::ClearBuffer, Chord::cmd_shift(Key::B)),
    (PaneCommand::SearchSelection, Chord::cmd_shift(Key::F)),
    (PaneCommand::OpenLink, Chord::cmd_shift(Key::O)),
];

/// The keystroke that opens the pane menu without a pointer.
///
/// Shift+F10 is the context-menu key on every platform that has one, which is the whole
/// reason to spend a function key on it: it is the chord a keyboard user already knows,
/// and a program in the pane can still receive plain F10 and every other modified form.
pub const MENU_CHORD: Chord = Chord {
    command: false,
    ctrl: false,
    alt: false,
    shift: true,
    key: Key::F10,
};

/// The keystroke that enters and leaves keyboard selection.
///
/// Selection needs a mode of its own because in a terminal the arrow keys belong to the
/// program: there is no spare gesture for "move a selection cursor". `tmux` and `less`
/// solve it the same way, so a terminal user already has the idea.
pub const SELECTION_MODE_CHORD: Chord = Chord::cmd_shift(Key::Space);

/// What the window knows about a pane that the pane cannot see for itself.
///
/// Every field is the *reason* an item is unavailable rather than a boolean, because the
/// menu has to say why and only the caller knows: whether this is the last pane in the
/// layout, whether a write lease is being recovered, whether the pane has a process behind
/// it at all. `None` means available.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaneContext {
    pub split_unavailable: Option<String>,
    pub close_unavailable: Option<String>,
    pub paste_unavailable: Option<String>,
    pub search_unavailable: Option<String>,
}

/// One row of the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItem {
    pub command: PaneCommand,
    /// The chord, written the way the user reads it. `None` when nothing is bound.
    pub shortcut: Option<String>,
    /// Why this item cannot be used, or `None` when it can.
    pub unavailable: Option<String>,
    pub separator_before: bool,
}

impl MenuItem {
    pub fn label(&self) -> &'static str {
        self.command.label()
    }

    pub fn enabled(&self) -> bool {
        self.unavailable.is_none()
    }

    /// What a screen reader says.
    ///
    /// The chord and the reason are both in it, because neither a shortcut column nor a
    /// tooltip exists for somebody who is listening to the menu rather than looking at it.
    pub fn accessible_name(&self) -> String {
        let mut name = self.label().to_string();
        if let Some(shortcut) = &self.shortcut {
            name.push_str(&format!(", {shortcut}"));
        }
        if let Some(reason) = &self.unavailable {
            name.push_str(&format!(", unavailable: {reason}"));
        }
        name
    }
}

/// The chords the menu shows and the pane's keyboard handler obeys.
///
/// Built from the window's keymap so a user's own override reaches the menu, plus the
/// terminal's own table for the items the keymap has no command for.
#[derive(Debug, Clone)]
pub struct PaneShortcuts {
    platform: Platform,
    chords: Vec<(PaneCommand, Option<Chord>)>,
}

impl PaneShortcuts {
    pub fn from_keymap(keymap: &Keymap) -> Self {
        let chords = PaneCommand::ALL
            .iter()
            .map(|command| {
                let chord = match command.window_command() {
                    Some(window) => keymap.chord_for(window),
                    None => PANE_CHORDS
                        .iter()
                        .find(|(candidate, _)| candidate == command)
                        .map(|(_, chord)| *chord),
                };
                (*command, chord)
            })
            .collect();
        Self {
            platform: keymap.platform(),
            chords,
        }
    }

    /// The table for this build's platform and no overrides.
    pub fn defaults() -> Self {
        Self::from_keymap(&Keymap::defaults())
    }

    pub fn platform(&self) -> Platform {
        self.platform
    }

    pub fn chord(&self, command: PaneCommand) -> Option<Chord> {
        self.chords
            .iter()
            .find(|(candidate, _)| *candidate == command)
            .and_then(|(_, chord)| *chord)
    }

    /// The chord as the user reads it, for the menu's shortcut column.
    pub fn describe(&self, command: PaneCommand) -> Option<String> {
        self.chord(command)
            .map(|chord| chord.describe(self.platform))
    }

    /// The command a keystroke asks the *pane* for.
    ///
    /// Only ever one of the four the pane owns. A window command's key event never reaches
    /// a pane — the keymap consumed it — so matching one here would mean handling it twice.
    pub fn resolve(&self, key: Key, modifiers: &Modifiers) -> Option<PaneCommand> {
        self.chords
            .iter()
            .filter(|(command, _)| command.owned_by_pane())
            .find_map(|(command, chord)| {
                chord
                    .filter(|chord| chord.matches(key, modifiers, self.platform))
                    .map(|_| *command)
            })
    }

    /// Whether this keystroke opens the menu.
    pub fn opens_menu(&self, key: Key, modifiers: &Modifiers) -> bool {
        MENU_CHORD.matches(key, modifiers, self.platform)
    }

    /// Whether this keystroke enters or leaves keyboard selection.
    pub fn toggles_selection_mode(&self, key: Key, modifiers: &Modifiers) -> bool {
        SELECTION_MODE_CHORD.matches(key, modifiers, self.platform)
    }
}

/// Everything the menu needs in order to decide what it offers.
///
/// Borrowed rather than owned so building it costs nothing: it is assembled every frame the
/// menu is open, and the grid behind it is the largest thing on screen.
pub struct PaneMenu<'a> {
    pub grid: &'a Grid,
    /// The cell the menu was opened over. What "under the pointer" means for Open Link.
    pub at: CellPos,
    pub selection: Option<&'a Selection>,
    pub context: &'a PaneContext,
    pub shortcuts: &'a PaneShortcuts,
    /// The links this pane's grid holds, so "Open Link" answers with the one the user is
    /// pointing at rather than a second, weaker reading of the same text.
    ///
    /// `None` for a caller that has not scanned for links; the item is then only offered for
    /// a selection that is itself a URL.
    pub links: Option<&'a LinkMap>,
}

impl PaneMenu<'_> {
    /// The selected text, or `None` when the selection covers nothing worth copying.
    pub fn selected_text(&self) -> Option<String> {
        let text = self.selection?.text(self.grid);
        if text.trim().is_empty() {
            None
        } else {
            Some(text)
        }
    }

    /// The link this menu would follow: the one under the pointer, or the one the selection
    /// is.
    ///
    /// The pointer first, because that is what the user aimed at — and it comes from the
    /// pane's own [`LinkMap`], so a program's OSC 8 hyperlink, a detected URL and a path a
    /// compiler named are all found by the one scanner rather than by a second guess made
    /// here. A selection that happens to be a URL is the fallback, so choosing the item after
    /// selecting a link by hand does what it looks like it will do.
    ///
    /// A hand-selected URL goes through [`normalise_url`], which is where the scheme
    /// allow-list lives: the answer is a link Turn is willing to open or no link at all.
    pub fn link(&self) -> Option<LinkRequest> {
        if let Some(link) = self
            .links
            .and_then(|links| links.at(self.at.row, self.at.col))
        {
            return Some(link.request());
        }
        let text = self.selected_text()?;
        let url = normalise_url(text.trim())?;
        Some(LinkRequest {
            target: LinkTarget::Url(url.clone()),
            display: url,
            text,
            // A selection the user made by hand cannot misrepresent itself: the text *is*
            // the target, which is the one case that never needs a confirmation.
            warning: None,
        })
    }

    /// The menu, as data.
    ///
    /// Separate from drawing it so every availability rule is testable without a window,
    /// and so a snapshot can render the list directly.
    pub fn items(&self) -> Vec<MenuItem> {
        let selected = self.selected_text();
        let link = self.link();
        PaneCommand::ALL
            .iter()
            .map(|command| MenuItem {
                command: *command,
                shortcut: self.shortcuts.describe(*command),
                unavailable: self.unavailable(*command, selected.is_some(), link.is_some()),
                separator_before: command.starts_a_group(),
            })
            .collect()
    }

    /// Why an item cannot be used, in words the user can act on.
    fn unavailable(
        &self,
        command: PaneCommand,
        has_selection: bool,
        has_link: bool,
    ) -> Option<String> {
        match command {
            PaneCommand::Copy if !has_selection => {
                Some("nothing is selected — drag across the text, or double-click a word".into())
            }
            PaneCommand::SearchSelection if !has_selection => {
                Some("select the text to search for first".into())
            }
            PaneCommand::SearchSelection => self.context.search_unavailable.clone(),
            PaneCommand::Paste => self.context.paste_unavailable.clone(),
            PaneCommand::SelectAll if self.screen_is_empty() => {
                Some("this pane has nothing on it yet".into())
            }
            PaneCommand::ClearBuffer if self.grid.alternate_screen => Some(
                "a full-screen program is managing this screen, so Turn keeps no history for it"
                    .into(),
            ),
            PaneCommand::ClearBuffer if self.grid.scrollback_len == 0 => {
                Some("nothing has scrolled off this pane yet".into())
            }
            PaneCommand::OpenLink if !has_link => Some("there is no link under the pointer".into()),
            PaneCommand::SplitHorizontally | PaneCommand::SplitVertically => {
                self.context.split_unavailable.clone()
            }
            PaneCommand::ClosePane => self.context.close_unavailable.clone(),
            _ => None,
        }
    }

    fn screen_is_empty(&self) -> bool {
        self.grid.scrollback_len == 0
            && (0..self.grid.rows).all(|row| self.grid.row_text(row).trim().is_empty())
    }

    /// Draws the menu and reports what was chosen.
    pub fn show(&self, ui: &mut Ui, theme: &Theme) -> Option<PaneCommand> {
        show_items(ui, theme, &self.items())
    }
}

/// Draws a list of menu items and reports the one chosen.
///
/// Public so a snapshot can render the menu without opening a popup, which is the only way
/// to get a reviewable image of a menu with items disabled.
pub fn show_items(ui: &mut Ui, theme: &Theme, items: &[MenuItem]) -> Option<PaneCommand> {
    show_items_focusing(ui, theme, items, false)
}

/// The same, optionally putting the first usable item in focus.
///
/// `focus_first` is for a menu opened from the keyboard. `egui`'s arrow-key navigation moves
/// between *focused* widgets, so with nothing focused the arrows would have nothing to move
/// from and the menu would be visible but inoperable.
pub fn show_items_focusing(
    ui: &mut Ui,
    theme: &Theme,
    items: &[MenuItem],
    focus_first: bool,
) -> Option<PaneCommand> {
    let mut chosen = None;
    let mut focused_one = !focus_first || ui.memory(|memory| memory.focused()).is_some();
    for item in items {
        if item.separator_before {
            ui.separator();
        }
        let mut button = egui::Button::new(item.label());
        if let Some(shortcut) = &item.shortcut {
            button = button.shortcut_text(shortcut);
        }
        let response = ui.add_enabled(item.enabled(), button);
        if !focused_one && item.enabled() {
            response.request_focus();
            focused_one = true;
        }
        if let Some(reason) = &item.unavailable {
            // Written under the item rather than left to a tooltip: a greyed item with no
            // visible explanation is the thing this menu exists to avoid, and a hover is
            // not available to somebody using a keyboard.
            ui.label(
                RichText::new(reason)
                    .color(theme.text_faint)
                    .size(theme.ui_font.size - 2.0),
            );
        }
        // The accessible name carries the chord and the reason, neither of which a screen
        // reader would otherwise reach.
        let name = item.accessible_name();
        ui.ctx().accesskit_node_builder(response.id, |node| {
            node.set_label(name);
        });
        if response.clicked() {
            chosen = Some(item.command);
            ui.close();
        }
    }
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::links::PathResolver;
    use crate::terminal::selection::SelectionKind;

    /// A resolver that finds nothing on disk, so these tests are about links in the text
    /// rather than about this machine's filesystem.
    struct NoPaths;

    impl PathResolver for NoPaths {
        fn resolve(&mut self, _candidate: &str) -> Option<std::path::PathBuf> {
            None
        }
    }

    fn shortcuts() -> PaneShortcuts {
        PaneShortcuts::from_keymap(&Keymap::build(
            &crate::keymap::Overrides::new(),
            Platform::MAC,
        ))
    }

    fn menu<'a>(
        grid: &'a Grid,
        at: CellPos,
        selection: Option<&'a Selection>,
        context: &'a PaneContext,
        shortcuts: &'a PaneShortcuts,
    ) -> PaneMenu<'a> {
        PaneMenu {
            grid,
            at,
            selection,
            context,
            shortcuts,
            links: None,
        }
    }

    /// A menu over a pane whose links have been scanned, which is how the window will build
    /// it: `links::LinkMap` is the one scanner, and the menu reads its answer.
    fn linked_menu<'a>(
        grid: &'a Grid,
        at: CellPos,
        selection: Option<&'a Selection>,
        context: &'a PaneContext,
        shortcuts: &'a PaneShortcuts,
        links: &'a LinkMap,
    ) -> PaneMenu<'a> {
        PaneMenu {
            grid,
            at,
            selection,
            context,
            shortcuts,
            links: Some(links),
        }
    }

    fn item(items: &[MenuItem], command: PaneCommand) -> &MenuItem {
        items
            .iter()
            .find(|item| item.command == command)
            .unwrap_or_else(|| panic!("the menu must always offer {}", command.label()))
    }

    /// The menu is a fixed list. An item that disappeared when it could not be used would
    /// leave the user unable to learn it exists.
    #[test]
    fn every_command_is_offered_every_time_whether_it_can_be_used_or_not() {
        let grid = Grid::blank(24, 80);
        let context = PaneContext::default();
        let shortcuts = shortcuts();
        let items = menu(&grid, CellPos::new(0, 0), None, &context, &shortcuts).items();

        assert_eq!(items.len(), PaneCommand::ALL.len());
        for command in PaneCommand::ALL {
            assert_eq!(item(&items, *command).command, *command);
        }
        assert!(
            !item(&items, PaneCommand::Copy).enabled(),
            "nothing is selected"
        );
        assert!(
            item(&items, PaneCommand::Copy).unavailable.is_some(),
            "and the item says so rather than vanishing"
        );
    }

    /// The rule the whole module is for: unavailable is explained, not silent.
    #[test]
    fn an_item_that_cannot_be_used_explains_itself() {
        let mut grid = Grid::from_lines(&["output"], 20);
        grid.alternate_screen = true;
        let context = PaneContext {
            split_unavailable: Some("this pane has no process to split from".into()),
            close_unavailable: Some("the last pane of a session cannot be closed".into()),
            ..PaneContext::default()
        };
        let shortcuts = shortcuts();
        let items = menu(&grid, CellPos::new(0, 0), None, &context, &shortcuts).items();

        for (command, fragment) in [
            (PaneCommand::Copy, "nothing is selected"),
            (PaneCommand::SearchSelection, "select the text"),
            (PaneCommand::OpenLink, "no link under the pointer"),
            (PaneCommand::ClearBuffer, "full-screen program"),
            (PaneCommand::SplitHorizontally, "no process"),
            (PaneCommand::SplitVertically, "no process"),
            (PaneCommand::ClosePane, "last pane"),
        ] {
            let item = item(&items, command);
            let reason = item
                .unavailable
                .as_deref()
                .unwrap_or_else(|| panic!("{} must be unavailable", command.label()));
            assert!(
                reason.contains(fragment),
                "{} says {reason:?}, which does not mention {fragment:?}",
                command.label()
            );
            assert!(
                item.accessible_name().contains(reason),
                "a screen reader must hear the reason too: {}",
                item.accessible_name()
            );
        }
    }

    #[test]
    fn a_selection_enables_copy_and_search_and_says_nothing_about_them() {
        let grid = Grid::from_lines(&["cargo test -p turn-gui"], 30);
        let mut selection = Selection::new(CellPos::new(0, 0), SelectionKind::Linear);
        selection.extend_to(CellPos::new(0, 5));
        let context = PaneContext::default();
        let shortcuts = shortcuts();
        let menu = menu(
            &grid,
            CellPos::new(0, 0),
            Some(&selection),
            &context,
            &shortcuts,
        );

        assert_eq!(menu.selected_text().as_deref(), Some("cargo"));
        let items = menu.items();
        assert!(item(&items, PaneCommand::Copy).enabled());
        assert!(item(&items, PaneCommand::SearchSelection).enabled());
        assert_eq!(item(&items, PaneCommand::Copy).unavailable, None);
    }

    /// A selection of nothing but padding is not something to copy: a stray drag must not
    /// offer to replace the clipboard with spaces.
    #[test]
    fn a_selection_of_blank_cells_does_not_count_as_something_to_copy() {
        let grid = Grid::from_lines(&["a       b"], 10);
        let mut selection = Selection::new(CellPos::new(0, 2), SelectionKind::Linear);
        selection.extend_to(CellPos::new(0, 6));
        let context = PaneContext::default();
        let shortcuts = shortcuts();
        let menu = menu(
            &grid,
            CellPos::new(0, 0),
            Some(&selection),
            &context,
            &shortcuts,
        );
        assert_eq!(menu.selected_text(), None);
        assert!(!item(&menu.items(), PaneCommand::Copy).enabled());
    }

    #[test]
    fn clear_buffer_is_offered_only_when_there_is_history_to_clear() {
        let mut grid = Grid::from_lines(&["output"], 20);
        let context = PaneContext::default();
        let shortcuts = shortcuts();

        let empty = menu(&grid, CellPos::new(0, 0), None, &context, &shortcuts).items();
        assert!(
            item(&empty, PaneCommand::ClearBuffer)
                .unavailable
                .as_deref()
                .is_some_and(|reason| reason.contains("scrolled off")),
            "with no history the item has to say what would make it usable"
        );

        grid.scrollback_len = 120;
        let with_history = menu(&grid, CellPos::new(0, 0), None, &context, &shortcuts).items();
        assert!(item(&with_history, PaneCommand::ClearBuffer).enabled());
    }

    #[test]
    fn open_link_follows_the_cell_the_menu_was_opened_over() {
        let grid = Grid::from_lines(&["see https://example.com/x for more"], 40);
        let context = PaneContext::default();
        let shortcuts = shortcuts();
        let links = LinkMap::find(&grid, &mut NoPaths);

        let over_link = linked_menu(
            &grid,
            CellPos::new(0, 10),
            None,
            &context,
            &shortcuts,
            &links,
        );
        assert_eq!(
            over_link.link().map(|link| link.display),
            Some("https://example.com/x".to_string())
        );
        assert!(item(&over_link.items(), PaneCommand::OpenLink).enabled());

        let over_prose = linked_menu(
            &grid,
            CellPos::new(0, 1),
            None,
            &context,
            &shortcuts,
            &links,
        );
        assert_eq!(over_prose.link(), None);
        assert!(!item(&over_prose.items(), PaneCommand::OpenLink).enabled());
    }

    /// A link the *program* declared can say one thing and point at another. The menu carries
    /// the warning rather than deciding it, so the window can ask before leaving Turn.
    #[test]
    fn a_declared_link_that_names_another_host_arrives_with_its_warning() {
        let mut grid = Grid::from_lines(&["visit github.com now"], 24);
        assert!(grid.set_row_meta(
            0,
            turn_proto::cells::RowMeta {
                wrapped: false,
                links: vec![turn_proto::cells::RowLink::new(
                    6,
                    16,
                    "https://evil.example/login"
                )],
            }
        ));
        let context = PaneContext::default();
        let shortcuts = shortcuts();
        let links = LinkMap::find(&grid, &mut NoPaths);
        let request = linked_menu(
            &grid,
            CellPos::new(0, 8),
            None,
            &context,
            &shortcuts,
            &links,
        )
        .link()
        .expect("the declared link is found");
        assert!(
            request.needs_confirmation(),
            "the text names github.com and the target does not: {request:?}"
        );
    }

    /// Selecting a link by hand and choosing Open Link has to work, or the item looks
    /// broken in the one case the user was explicit about.
    #[test]
    fn a_selected_url_is_openable_even_when_the_pointer_is_elsewhere() {
        let grid = Grid::from_lines(&["https://example.com/a  plain"], 30);
        let mut selection = Selection::new(CellPos::new(0, 0), SelectionKind::Linear);
        selection.extend_to(CellPos::new(0, 21));
        let context = PaneContext::default();
        let shortcuts = shortcuts();
        let over_selection = menu(
            &grid,
            CellPos::new(0, 25),
            Some(&selection),
            &context,
            &shortcuts,
        );
        let request = over_selection.link().expect("the selection is a URL");
        assert_eq!(request.display, "https://example.com/a");
        assert!(
            !request.needs_confirmation(),
            "a selection the user made by hand cannot misrepresent itself"
        );

        // A selection that is not a URL is not something to open, and a scheme Turn will not
        // hand to the platform is refused here rather than at the point of no return.
        let hostile = Grid::from_lines(&["javascript:alert(1)"], 24);
        let mut all = Selection::new(CellPos::new(0, 0), SelectionKind::Linear);
        all.extend_to(CellPos::new(0, 19));
        assert_eq!(
            menu(
                &hostile,
                CellPos::new(0, 22),
                Some(&all),
                &context,
                &shortcuts
            )
            .link(),
            None,
            "the scheme allow-list is the one gate, and it is closed"
        );
    }

    /// The menu teaches the keyboard, so every item that has a chord shows it — and shows
    /// the one in force rather than the default.
    #[test]
    fn each_item_shows_the_chord_that_is_actually_bound() {
        let shortcuts = shortcuts();
        for command in PaneCommand::ALL {
            let described = shortcuts.describe(*command);
            assert!(
                described.is_some(),
                "{} has no chord to teach",
                command.label()
            );
        }
        assert_eq!(
            shortcuts.describe(PaneCommand::Copy).as_deref(),
            Some("Cmd+C"),
            "on a Mac the window's own copy chord"
        );
        assert_eq!(
            shortcuts.describe(PaneCommand::SelectAll).as_deref(),
            Some("Shift+Cmd+E")
        );

        // A user who rebound copy must be taught their own chord, not the default.
        let overrides =
            crate::keymap::Overrides::new().bind(Command::CopySelection, Chord::cmd(Key::Y));
        let rebound = PaneShortcuts::from_keymap(&Keymap::build(&overrides, Platform::MAC));
        assert_eq!(
            rebound.describe(PaneCommand::Copy).as_deref(),
            Some("Cmd+Y"),
            "the menu must never teach a chord the user has replaced"
        );

        // And a user who unbound it is shown no chord rather than a lie.
        let unbound = PaneShortcuts::from_keymap(&Keymap::build(
            &crate::keymap::Overrides::new().unbind(Command::ClosePane),
            Platform::MAC,
        ));
        assert_eq!(unbound.describe(PaneCommand::ClosePane), None);
    }

    /// A chord the window has already taken would never reach the pane: the keymap
    /// consumes the key event before a pane is drawn. This is the test that keeps the two
    /// tables apart, on both keyboards.
    #[test]
    fn the_terminal_owns_no_chord_the_window_has_already_taken() {
        for platform in [Platform::MAC, Platform::PC] {
            let keymap = Keymap::build(&crate::keymap::Overrides::new(), platform);
            let taken: Vec<Chord> = keymap.bindings().iter().map(|bound| bound.chord).collect();
            let mut mine: Vec<Chord> = PANE_CHORDS.iter().map(|(_, chord)| *chord).collect();
            mine.push(MENU_CHORD);
            mine.push(SELECTION_MODE_CHORD);

            for chord in &mine {
                assert!(
                    !taken.contains(chord),
                    "{} is bound in the window's keymap on {platform:?}, so the pane \
                     would never see it",
                    chord.describe(platform)
                );
                assert!(
                    !chord.shadows_control_character(platform),
                    "{} arrives as a control character a program is entitled to receive",
                    chord.describe(platform)
                );
            }

            // And none of the pane's own chords collide with each other.
            let mut sorted = mine.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(sorted.len(), mine.len(), "two pane chords are the same");
        }
    }

    /// The pane resolves only the commands it owns. Resolving the window's would handle
    /// copy twice: once here and once from `egui`'s own clipboard event.
    #[test]
    fn the_pane_resolves_its_own_chords_and_leaves_the_windows_alone() {
        let shortcuts = shortcuts();
        let modifiers = Modifiers {
            mac_cmd: true,
            command: true,
            shift: true,
            ..Modifiers::default()
        };
        assert_eq!(
            shortcuts.resolve(Key::E, &modifiers),
            Some(PaneCommand::SelectAll)
        );
        assert_eq!(
            shortcuts.resolve(Key::B, &modifiers),
            Some(PaneCommand::ClearBuffer)
        );
        assert_eq!(
            shortcuts.resolve(Key::F, &modifiers),
            Some(PaneCommand::SearchSelection)
        );
        assert_eq!(
            shortcuts.resolve(Key::O, &modifiers),
            Some(PaneCommand::OpenLink)
        );

        let plain_cmd = Modifiers {
            mac_cmd: true,
            command: true,
            ..Modifiers::default()
        };
        assert_eq!(
            shortcuts.resolve(Key::C, &plain_cmd),
            None,
            "copy belongs to the window's keymap and arrives as a clipboard event"
        );
        assert_eq!(shortcuts.resolve(Key::A, &Modifiers::default()), None);
    }

    #[test]
    fn the_menu_and_selection_mode_have_their_own_keystrokes() {
        let shortcuts = shortcuts();
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        assert!(shortcuts.opens_menu(Key::F10, &shift));
        assert!(
            !shortcuts.opens_menu(Key::F10, &Modifiers::default()),
            "plain F10 belongs to the program in the pane"
        );

        let cmd_shift = Modifiers {
            mac_cmd: true,
            command: true,
            shift: true,
            ..Modifiers::default()
        };
        assert!(shortcuts.toggles_selection_mode(Key::Space, &cmd_shift));
        assert!(!shortcuts.toggles_selection_mode(Key::Space, &Modifiers::default()));
    }

    /// Choosing an item that acts on this pane focuses it first, because the menu was
    /// opened on a pane that may not have been focused.
    #[test]
    fn the_items_that_act_on_this_pane_are_the_ones_that_need_it_focused() {
        assert!(PaneCommand::SplitHorizontally.needs_focus());
        assert!(PaneCommand::ClosePane.needs_focus());
        assert!(PaneCommand::Paste.needs_focus());
        assert!(
            !PaneCommand::Copy.needs_focus(),
            "copying out of a pane is not a reason to move focus into it"
        );
        assert!(!PaneCommand::OpenLink.needs_focus());
        assert!(!PaneCommand::SearchSelection.needs_focus());
    }
}
