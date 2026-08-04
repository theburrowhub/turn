//! The command palette: what it offers, and in what order.
//!
//! A palette is only as good as its ranking. The logic is therefore a value rather
//! than something buried in a draw function: [`Palette::matches`] takes a query and
//! returns commands in the order they should appear, so the ordering can be tested
//! against the cases that actually annoy people.
//!
//! Three rules:
//!
//! * **A prefix beats a subsequence.** Typing `zoom` must put "Maximise pane" ahead of
//!   anything that merely contains those letters scattered about.
//! * **Initials work.** `sn` finds "Session — new" because people type initials, and a
//!   palette that only did substrings would make them type the whole word.
//! * **Nothing is hidden.** An empty query lists everything, grouped, so the palette
//!   doubles as the place to discover what the window can do. A command with no
//!   keyboard shortcut is still there.

use crate::keymap::{Command, Keymap};

/// One row of the palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub command: Command,
    /// Where the query's characters landed, for highlighting them in the row.
    pub highlights: Vec<usize>,
    /// Higher sorts first. Exposed so a test can assert on relative order rather than
    /// on absolute numbers.
    pub score: i32,
}

/// The palette's state: what has been typed and which row is selected.
#[derive(Debug, Clone, Default)]
pub struct Palette {
    pub query: String,
    /// The index into the current match list. Kept rather than stored as a command so
    /// that typing another character selects the new best match rather than chasing a
    /// command that has dropped out of the list.
    pub selected: usize,
    pub open: bool,
}

impl Palette {
    pub fn new() -> Self {
        Self::default()
    }

    /// Opens the palette, empty, with the first row selected.
    pub fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.selected = 0;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.query.clear();
        self.selected = 0;
    }

    /// Records a change to the query, resetting the selection to the best match.
    pub fn set_query(&mut self, query: impl Into<String>) {
        self.query = query.into();
        self.selected = 0;
    }

    /// Moves the selection, wrapping. A palette that stopped at the ends would make
    /// getting to the last row a matter of counting.
    pub fn move_selection(&mut self, delta: i32, count: usize) {
        if count == 0 {
            self.selected = 0;
            return;
        }
        let count = count as i32;
        let next = (self.selected as i32 + delta).rem_euclid(count);
        self.selected = next as usize;
    }

    /// The commands that match the query, best first.
    pub fn matches(&self) -> Vec<Match> {
        matches_for(&self.query)
    }

    /// The command the user would run by pressing Enter.
    pub fn chosen(&self) -> Option<Command> {
        self.matches().get(self.selected).map(|found| found.command)
    }
}

/// The commands matching a query, best first.
///
/// A free function so the ranking can be tested without a palette to hold it.
pub fn matches_for(query: &str) -> Vec<Match> {
    let query = query.trim();
    if query.is_empty() {
        // Everything, in the order the command list declares — which is grouped by
        // area, so an empty palette reads as a menu rather than as an alphabetical dump.
        return Command::ALL
            .iter()
            .enumerate()
            .map(|(index, command)| Match {
                command: *command,
                highlights: Vec::new(),
                // Descending, so the declared order survives a stable sort by score.
                score: (Command::ALL.len() - index) as i32,
            })
            .collect();
    }

    let mut found: Vec<Match> = Command::ALL
        .iter()
        .filter_map(|command| score(*command, query))
        .collect();
    // Stable, and descending, so commands with equal scores keep the declared order
    // rather than shuffling between keystrokes.
    found.sort_by_key(|found| std::cmp::Reverse(found.score));
    found
}

/// How well a command matches, or `None` when it does not.
///
/// The text searched is the title, the group and the command's own identifier, with the
/// dot in the identifier read as a space. The identifier is in there because a title is
/// written for reading and an identifier for naming, and the two do not always use the
/// same word: `pane.zoom` is titled "Maximise pane", and a user who types `zoom` must
/// not be told there is no such command.
///
/// Positions are **character** positions, not byte offsets, and the highlights that
/// come back are clipped to the title. Two reasons, both of which would otherwise be a
/// panic in a draw function: some titles contain an em dash, so byte offsets and
/// character offsets diverge; and a match found in the identifier lies past the end of
/// the title, where a renderer highlighting it would index off the end of the string.
/// A match with nothing to highlight is honest — nothing in the visible title matched.
fn score(command: Command, query: &str) -> Option<Match> {
    let title: Vec<char> = command.title().to_lowercase().chars().collect();
    let identity: Vec<char> = words_of_id(command.id()).chars().collect();
    let haystack: Vec<char> = format!(
        "{} {} {}",
        command.title(),
        command.group(),
        words_of_id(command.id())
    )
    .to_lowercase()
    .chars()
    .collect();
    let needle: Vec<char> = query.to_lowercase().chars().collect();
    if needle.is_empty() {
        return None;
    }

    let found = |score: i32, positions: Vec<usize>| {
        Some(Match {
            command,
            highlights: positions
                .into_iter()
                .filter(|at| *at < title.len())
                .collect(),
            score,
        })
    };

    // A prefix of the title is the strongest signal there is.
    if title.starts_with(&needle) {
        return found(1_000, (0..needle.len()).collect());
    }
    // A *whole* word of the command's own identifier. Ranked above a word of the title,
    // because the identifier is what the command *is* while a title may mention a word in
    // passing: "template" is the whole point of `layout.saveAsTemplate` and an aside in
    // "New session — pick a template".
    //
    // Whole word rather than a prefix, deliberately. A prefix here would let `pane` match
    // the "panel" in `attention.togglePanel` and outrank every actual pane command —
    // which is exactly what it did before this was tightened.
    if whole_word_match(&identity, &needle) {
        return found(900, Vec::new());
    }
    // A whole word starting with the query — "split" in "Split pane left / right".
    if let Some(at) = word_start_match(&haystack, &needle) {
        return found(800 - at as i32, (at..at + needle.len()).collect());
    }
    // Initials: `sn` for "Session · New session".
    if let Some(positions) = initials_match(&haystack, &needle) {
        return found(600, positions);
    }
    // Anywhere in the text.
    if let Some(at) = find_chars(&haystack, &needle) {
        return found(400 - at as i32, (at..at + needle.len()).collect());
    }
    // Characters in order but not adjacent, which is what makes a fuzzy palette
    // forgiving of a typo in the middle of a word.
    let positions = subsequence_match(&haystack, &needle)?;
    let score = 200 - highlights_span(&positions);
    found(score, positions)
}

/// A command identifier as searchable words.
///
/// `layout.saveAsTemplate` becomes `layout save as template`: the dot and the camel
/// humps are both word boundaries, which is what lets a query match a segment of an
/// identifier rather than only its start.
fn words_of_id(id: &str) -> String {
    let mut out = String::with_capacity(id.len() + 4);
    for ch in id.chars() {
        if ch == '.' {
            out.push(' ');
        } else if ch.is_uppercase() {
            out.push(' ');
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// The first character position where `needle` appears in `haystack`.
fn find_chars(haystack: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|at| &haystack[*at..*at + needle.len()] == needle)
}

/// Whether the query is a whole word of the haystack, bounded on both sides.
fn whole_word_match(haystack: &[char], needle: &[char]) -> bool {
    match word_start_match(haystack, needle) {
        Some(at) => haystack
            .get(at + needle.len())
            .is_none_or(|after| !after.is_alphanumeric()),
        None => false,
    }
}

/// Whether a character position begins a word.
fn is_word_start(haystack: &[char], at: usize) -> bool {
    match at.checked_sub(1).and_then(|before| haystack.get(before)) {
        None => true,
        Some(previous) => !previous.is_alphanumeric(),
    }
}

/// Where the query matches the start of a word, if it does.
fn word_start_match(haystack: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    (0..=haystack.len() - needle.len())
        .find(|at| &haystack[*at..*at + needle.len()] == needle && is_word_start(haystack, *at))
}

/// Whether the query is the initials of consecutive words.
fn initials_match(haystack: &[char], needle: &[char]) -> Option<Vec<usize>> {
    if needle.len() < 2 {
        return None;
    }
    let initials: Vec<(usize, char)> = haystack
        .iter()
        .enumerate()
        .filter(|(at, ch)| ch.is_alphanumeric() && is_word_start(haystack, *at))
        .map(|(at, ch)| (at, *ch))
        .collect();

    // Consecutive words, so `qn` matches "Quick New" but not two initials with an
    // unrelated word between them.
    (0..initials.len())
        .filter(|start| start + needle.len() <= initials.len())
        .find(|start| {
            initials[*start..start + needle.len()]
                .iter()
                .zip(needle)
                .all(|((_, ch), want)| ch == want)
        })
        .map(|start| {
            initials[start..start + needle.len()]
                .iter()
                .map(|(at, _)| *at)
                .collect()
        })
}

/// Where the query's characters land, in order, if they all do.
fn subsequence_match(haystack: &[char], needle: &[char]) -> Option<Vec<usize>> {
    let mut positions = Vec::with_capacity(needle.len());
    let mut at = 0;
    for wanted in needle {
        let found = haystack.get(at..)?.iter().position(|ch| ch == wanted)?;
        positions.push(at + found);
        at += found + 1;
    }
    Some(positions)
}

/// How spread out a subsequence match is. A tight match is a better one.
fn highlights_span(highlights: &[usize]) -> i32 {
    match (highlights.first(), highlights.last()) {
        (Some(first), Some(last)) => (last - first) as i32,
        _ => 0,
    }
}

/// One row of the palette, ready to draw: the command, its shortcut, and where the
/// query matched.
#[derive(Debug, Clone)]
pub struct Row {
    pub command: Command,
    pub title: &'static str,
    pub group: &'static str,
    /// The chord, described for this platform, or `None` for an unbound command.
    pub shortcut: Option<String>,
    pub highlights: Vec<usize>,
}

/// The palette's rows, with the keymap's shortcuts attached.
pub fn rows(query: &str, keymap: &Keymap) -> Vec<Row> {
    matches_for(query)
        .into_iter()
        .map(|found| Row {
            command: found.command,
            title: found.command.title(),
            group: found.command.group(),
            shortcut: keymap
                .chord_for(found.command)
                .map(|chord| chord.describe(keymap.platform())),
            highlights: found.highlights,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{Overrides, Platform};

    fn ranked(query: &str) -> Vec<Command> {
        matches_for(query)
            .into_iter()
            .map(|found| found.command)
            .collect()
    }

    #[test]
    fn an_empty_query_offers_everything_so_the_palette_doubles_as_discovery() {
        let all = matches_for("");
        assert_eq!(all.len(), Command::ALL.len());
        assert_eq!(
            all.first().map(|found| found.command),
            Command::ALL.first().copied(),
            "the declared order is the grouped order and must survive"
        );
    }

    /// The case a palette gets wrong most visibly.
    #[test]
    fn a_title_prefix_beats_a_match_buried_in_the_middle() {
        let ranked = ranked("split");
        assert!(
            matches!(
                ranked.first(),
                Some(Command::SplitHorizontal | Command::SplitVertical)
            ),
            "got {ranked:?}"
        );
    }

    #[test]
    fn the_word_a_user_would_type_finds_the_command_they_mean() {
        for (query, expected) in [
            ("zoom", Command::ZoomPane),
            ("archive", Command::ArchiveSession),
            ("palette", Command::OpenPalette),
            ("settings", Command::OpenSettings),
            ("template", Command::SaveLayoutAsTemplate),
        ] {
            let ranked = ranked(query);
            assert_eq!(
                ranked.first(),
                Some(&expected),
                "searching {query:?} gave {ranked:?}"
            );
        }
    }

    /// A word in the middle of a title is still a word, and beats a scattered match.
    #[test]
    fn a_word_inside_a_title_ranks_above_a_scattered_one() {
        let found = matches_for("attention");
        let first = found.first().expect("something matches");
        assert!(
            matches!(
                first.command,
                Command::NextAttention | Command::ToggleAttentionPanel
            ),
            "got {:?}",
            first.command
        );
        assert!(first.score >= 600, "a word match must rank well: {first:?}");
    }

    #[test]
    fn initials_find_a_command_because_that_is_how_people_type() {
        let ranked = ranked("qn");
        assert!(
            ranked.contains(&Command::QuickNewSession),
            "`qn` must find the quick new session; got {ranked:?}"
        );
    }

    /// The ranking bug the palette snapshot caught: `pane` matched the "panel" inside
    /// `attention.togglePanel` and pushed it above every command that is actually about
    /// panes.
    #[test]
    fn a_query_naming_an_area_ranks_that_areas_commands_first() {
        let ranked = ranked("pane");
        let first = ranked.first().copied().expect("something matches");
        assert_eq!(
            first.group(),
            "Pane",
            "searching `pane` must offer the pane commands first; got {ranked:?}"
        );
        assert!(
            ranked.contains(&Command::ToggleAttentionPanel),
            "a partial match is still offered, just not first"
        );
    }

    /// The other half of the same rule: a whole word of an identifier is a strong signal
    /// and must still beat a title that only mentions the word.
    #[test]
    fn a_whole_word_of_the_identifier_still_beats_a_passing_mention_in_a_title() {
        let ranked = ranked("template");
        assert_eq!(
            ranked.first(),
            Some(&Command::SaveLayoutAsTemplate),
            "got {ranked:?}"
        );
        assert!(ranked.contains(&Command::NewSession));
    }

    #[test]
    fn a_query_matching_nothing_returns_nothing_rather_than_everything() {
        assert!(ranked("zzqqxx").is_empty());
    }

    #[test]
    fn the_group_is_searchable_so_a_whole_area_can_be_listed() {
        let ranked = ranked("pane");
        assert!(ranked.len() >= 6, "the pane commands: {ranked:?}");
        assert!(ranked.contains(&Command::ClosePane));
        assert!(ranked.contains(&Command::ZoomPane));
    }

    #[test]
    fn the_selection_wraps_so_the_last_row_is_one_keystroke_away() {
        let mut palette = Palette::new();
        palette.open();
        let count = palette.matches().len();
        assert!(count > 3);

        palette.move_selection(-1, count);
        assert_eq!(
            palette.selected,
            count - 1,
            "up from the top wraps to the end"
        );
        palette.move_selection(1, count);
        assert_eq!(palette.selected, 0);
        palette.move_selection(2, count);
        assert_eq!(palette.selected, 2);
    }

    #[test]
    fn a_palette_with_no_matches_has_nothing_selected_and_runs_nothing() {
        let mut palette = Palette::new();
        palette.open();
        palette.set_query("zzqqxx");
        assert!(palette.matches().is_empty());
        assert_eq!(palette.chosen(), None);
        palette.move_selection(1, 0);
        assert_eq!(palette.selected, 0);
    }

    /// Typing another character must select the new best match rather than keep a row
    /// index pointing at whatever happens to be there now.
    #[test]
    fn typing_resets_the_selection_to_the_best_match() {
        let mut palette = Palette::new();
        palette.open();
        palette.set_query("pane");
        palette.move_selection(3, palette.matches().len());
        assert_eq!(palette.selected, 3);
        palette.set_query("panes");
        assert_eq!(palette.selected, 0);
    }

    #[test]
    fn enter_runs_the_selected_row() {
        let mut palette = Palette::new();
        palette.open();
        palette.set_query("zoom");
        assert_eq!(palette.chosen(), Some(Command::ZoomPane));
    }

    #[test]
    fn closing_the_palette_forgets_what_was_typed() {
        let mut palette = Palette::new();
        palette.open();
        palette.set_query("zoom");
        palette.close();
        assert!(!palette.open);
        assert!(palette.query.is_empty());
        palette.open();
        assert!(palette.query.is_empty(), "reopening starts clean");
    }

    /// A command with no shortcut is still in the palette. Unbinding a chord must not
    /// make a feature unreachable.
    #[test]
    fn a_row_shows_its_shortcut_and_an_unbound_command_is_still_offered() {
        let keymap = Keymap::build(&Overrides::new().unbind(Command::ZoomPane), Platform::MAC);
        let rows = rows("", &keymap);
        assert_eq!(rows.len(), Command::ALL.len());

        let zoom = rows
            .iter()
            .find(|row| row.command == Command::ZoomPane)
            .expect("still offered");
        assert_eq!(zoom.shortcut, None);

        let palette_row = rows
            .iter()
            .find(|row| row.command == Command::OpenPalette)
            .expect("the palette itself");
        assert_eq!(palette_row.shortcut.as_deref(), Some("⌘K"));
        assert_eq!(palette_row.group, "View");
    }

    /// A highlight is a character position inside the visible title. A renderer slices
    /// the title with these, so one that pointed past the end would be a panic in a
    /// draw function — and a match found in the group or the identifier lies exactly
    /// there.
    #[test]
    fn a_highlight_is_always_a_position_inside_the_visible_title() {
        for query in [
            "split",
            "zoom",
            "template",
            "attention",
            "sess",
            "pane",
            "qn",
        ] {
            for found in matches_for(query) {
                let title_chars = found.command.title().chars().count();
                assert!(
                    found.highlights.iter().all(|at| *at < title_chars),
                    "{query:?} produced a highlight outside the title: {found:?}"
                );
            }
        }
    }

    #[test]
    fn a_match_in_the_title_says_where_it_landed_so_the_row_can_highlight_it() {
        let found = matches_for("split");
        let first = found.first().expect("something matches");
        assert!(!first.highlights.is_empty(), "got {first:?}");
        let title: Vec<char> = first.command.title().to_lowercase().chars().collect();
        let highlighted: String = first
            .highlights
            .iter()
            .filter_map(|at| title.get(*at))
            .collect();
        assert_eq!(highlighted, "split");
    }
}
