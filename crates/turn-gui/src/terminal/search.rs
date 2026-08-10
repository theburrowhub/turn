//! Searching a pane, from the window's side.
//!
//! The matching itself happens in the daemon — it is the only thing that holds the whole
//! scrollback, and `turn_proto::search` is the one reading of what a pattern means. What
//! lives here is everything around it: the query the user is building, which match they are
//! on, what to highlight, and where to put them back when they leave.
//!
//! ## Why it is a state machine and not a widget
//!
//! [`PaneSearch`] is a plain type with no drawing in it, because the interesting parts are
//! all decisions: whether a keystroke should cost a round trip, what "next" means when the
//! last match is selected, what to do when the buffer scrolls under a result set, and where
//! the viewport was before the search began. Those are testable without a window, and they
//! are tested without one. [`show_bar`] is the drawing, and it is thin.
//!
//! ## One request at a time
//!
//! A search box gets a keystroke every eighty milliseconds and the daemon reads five
//! thousand rows to answer one. So a query is *coalesced*: while one is outstanding, a newer
//! one is remembered and sent when the answer arrives. That bounds the cost at one search
//! per round trip however fast the user types, with no timer and no guesswork about how long
//! a search takes.
//!
//! ## Leaving puts the user back
//!
//! Search moves the viewport, which means it moves the user. Closing it returns the pane to
//! wherever they were when they opened it — [`SearchIntent::Restore`] — because a search that
//! abandons you three thousand rows from where you were reading is a search you stop using.

use egui::{Align2, Color32, FontId, Key, Rect, Stroke, Ui, Vec2};
use turn_proto::cells::Grid;
use turn_proto::search::{SearchMode, SearchOutcome, SearchQuery, MAX_QUERY_CHARS};

use crate::theme::Theme;

/// How tall the search bar is, and therefore how much of the pane it floats over.
pub const BAR_HEIGHT: f32 = 30.0;

/// How wide it is. Wide enough for a path or an error code, narrow enough to leave most of
/// a pane visible; a pane narrower than this gets the pane's width instead.
pub const BAR_WIDTH: f32 = 440.0;

/// The shortest gap between two searches of the same pane forced by new output.
///
/// New output can invalidate a result set — a row that scrolled out of the daemon's ring
/// moves every line index — so the query is run again when the buffer moves. During a build
/// that would otherwise be a search per frame, so it is rate-limited: an out-of-date count
/// for a third of a second is not a defect, and a search per frame across thirty panes is.
pub const REFRESH_INTERVAL_MS: i64 = 400;

/// What the search needs the window to do, which it cannot do itself.
///
/// The pane has no socket and does not own the feed, so every one of these is reported and
/// performed by the window. Same rule as the rest of this module: the pane decides nothing
/// that reaches beyond its own drawing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchIntent {
    /// Ask the daemon to run this query over the pane's scrollback —
    /// [`Request::SearchPane`](turn_proto::Request::SearchPane).
    Query(SearchQuery),
    /// Move the viewport so this line is visible — `feed::PaneFeed::reveal_line`.
    Reveal { line: usize },
    /// Put the viewport back where it was before the search opened —
    /// `feed::PaneFeed::scroll_to`.
    Restore { offset: usize },
}

/// One match to paint, in the coordinates of the grid on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Highlight {
    pub row: u16,
    pub col: u16,
    pub cols: u16,
    /// Whether this is the match the user is on. Exactly one highlight is current, and it
    /// is painted differently — a search where every hit looks the same is one where "next"
    /// does not visibly do anything.
    pub current: bool,
}

impl Highlight {
    /// Whether a column of a row falls inside this highlight.
    pub fn contains(&self, row: u16, col: u16) -> bool {
        row == self.row && col >= self.col && col < self.col.saturating_add(self.cols)
    }
}

/// Which of a match's two appearances a cell is painted as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// One of the other matches.
    Other,
    /// The match the user is on.
    Current,
}

/// Whichever mark applies to a cell, current winning over the rest.
pub fn mark_at(highlights: &[Highlight], row: u16, col: u16) -> Option<Mark> {
    let mut mark = None;
    for highlight in highlights {
        if highlight.contains(row, col) {
            if highlight.current {
                return Some(Mark::Current);
            }
            mark = Some(Mark::Other);
        }
    }
    mark
}

/// The colour behind a match that is not the current one.
///
/// A dimmed relative of the *current* match rather than of the selection, and that is the
/// point: every hit reads as one family with the loud one, and none of them can be mistaken
/// for text the user selected. Derived from the theme rather than added to it, so a theme
/// that changes its one loud colour changes both together.
pub fn match_background(theme: &Theme) -> Color32 {
    theme.attention.gamma_multiply(0.42)
}

/// The colours of the current match: the one loud colour in the theme, with the background
/// knocked out of the glyphs so they stay legible on it.
pub fn current_match_colours(theme: &Theme) -> (Color32, Color32) {
    (theme.background, theme.attention)
}

/// A pane's search: what is being looked for, what was found, and where the user was.
#[derive(Debug, Default, Clone)]
pub struct PaneSearch {
    open: bool,
    text: String,
    mode: SearchMode,
    case_sensitive: bool,
    /// The answer to the last query that was run, if any.
    outcome: Option<SearchOutcome>,
    /// Which match the user is on, an index into the outcome's matches.
    current: Option<usize>,
    /// The query the daemon has been asked and has not yet answered.
    in_flight: Option<SearchQuery>,
    /// Whether the query changed while one was outstanding, so it has to be run again the
    /// moment the answer arrives.
    queued: bool,
    /// Why the last query could not be run, in the daemon's words, so a bad pattern says
    /// what is wrong with it rather than silently finding nothing.
    refusal: Option<String>,
    /// Where the viewport was when the search opened, so leaving returns the user there.
    resume_offset: Option<usize>,
    /// How much history existed the last time this search looked, so a change can be
    /// noticed without the pane having to tell it about output.
    observed_len: Option<usize>,
    /// When the last query was sent, for the rate limit on refreshes.
    last_query_ms: i64,
    /// Set for one frame when the field should take the keyboard, so opening the search puts
    /// the cursor in it without the user clicking.
    grab_focus: bool,
    intents: Vec<SearchIntent>,
}

impl PaneSearch {
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Opens the search over a pane whose viewport is at `offset`.
    ///
    /// Keeps whatever was last searched for, which is what every editor does: reopening the
    /// box to search for the same thing again is the common case — and it runs that query
    /// again, because the output has moved on since.
    ///
    /// `now_ms` is when: every query stamps the clock, and the stamp is what the refresh rate
    /// limit is measured from. A query sent with a stale one would be followed by another a
    /// frame later.
    pub fn open(&mut self, offset: usize, now_ms: i64) {
        if !self.open {
            self.resume_offset = Some(offset);
        }
        self.open = true;
        self.grab_focus = true;
        if !self.text.is_empty() {
            self.request_query(now_ms);
        }
    }

    /// Opens the search for some text — the pane's own "search for this" menu item, or the
    /// window's.
    pub fn open_with(&mut self, text: impl Into<String>, offset: usize, now_ms: i64) {
        let text = clamp_query(&text.into());
        self.open(offset, now_ms);
        if text != self.text {
            self.set_text(text, now_ms);
        }
    }

    /// Closes the search and asks for the viewport the user came from.
    ///
    /// The results are dropped: a closed search that kept its highlights would leave a pane
    /// with orange text on it and nothing to explain why.
    pub fn close(&mut self) {
        if !self.open {
            return;
        }
        self.open = false;
        self.outcome = None;
        self.current = None;
        self.refusal = None;
        self.in_flight = None;
        self.queued = false;
        self.observed_len = None;
        if let Some(offset) = self.resume_offset.take() {
            self.intents.push(SearchIntent::Restore { offset });
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Replaces the text and runs the new query.
    pub fn set_text(&mut self, text: String, now_ms: i64) {
        let text = clamp_query(&text);
        if text == self.text {
            return;
        }
        self.text = text;
        self.current = None;
        self.refusal = None;
        if self.text.is_empty() {
            // Nothing to look for: the highlights go rather than lingering from the last
            // thing that was typed.
            self.outcome = None;
            self.queued = false;
            return;
        }
        self.request_query(now_ms);
    }

    pub fn case_sensitive(&self) -> bool {
        self.case_sensitive
    }

    pub fn set_case_sensitive(&mut self, sensitive: bool, now_ms: i64) {
        if sensitive == self.case_sensitive {
            return;
        }
        self.case_sensitive = sensitive;
        self.rerun(now_ms);
    }

    pub fn mode(&self) -> SearchMode {
        self.mode
    }

    pub fn is_regex(&self) -> bool {
        self.mode.is_regex()
    }

    pub fn set_regex(&mut self, regex: bool, now_ms: i64) {
        let wanted = if regex {
            SearchMode::Regex
        } else {
            SearchMode::Literal
        };
        if wanted == self.mode {
            return;
        }
        self.mode = wanted;
        self.rerun(now_ms);
    }

    /// The query as it now stands.
    pub fn query(&self) -> SearchQuery {
        SearchQuery {
            text: self.text.clone(),
            mode: self.mode,
            case_sensitive: self.case_sensitive,
        }
    }

    /// What the daemon last found.
    pub fn outcome(&self) -> Option<&SearchOutcome> {
        self.outcome.as_ref()
    }

    /// Which match the user is on.
    pub fn current(&self) -> Option<usize> {
        self.current
    }

    pub fn matches(&self) -> usize {
        self.outcome.as_ref().map_or(0, SearchOutcome::count)
    }

    /// Moves to the next match, wrapping, and asks for it to be shown.
    ///
    /// Wrapping rather than stopping at the end: a search that goes quiet on the last match
    /// leaves the user pressing a key that does nothing.
    pub fn next_match(&mut self) -> bool {
        self.step(1)
    }

    pub fn previous_match(&mut self) -> bool {
        self.step(-1)
    }

    fn step(&mut self, delta: isize) -> bool {
        let count = self.matches();
        if count == 0 {
            return false;
        }
        let index = match self.current {
            Some(current) => {
                let count = count as isize;
                (((current as isize + delta) % count) + count) as usize % count as usize
            }
            // No position yet: forwards starts at the oldest match and backwards at the
            // newest, so the first press goes somewhere sensible either way.
            None if delta >= 0 => 0,
            None => count - 1,
        };
        self.current = Some(index);
        if let Some(found) = self.outcome.as_ref().and_then(|o| o.matches.get(index)) {
            self.intents.push(SearchIntent::Reveal { line: found.line });
        }
        true
    }

    /// Selects the match nearest to where the user is already looking.
    ///
    /// Used when a fresh result set arrives: keeping the position means "next" continues from
    /// where the last one left off rather than jumping back to the top of the buffer.
    fn keep_position(&mut self, previous_line: Option<usize>) {
        let Some(outcome) = self.outcome.as_ref() else {
            self.current = None;
            return;
        };
        self.current = match previous_line {
            Some(line) => outcome
                .first_at_or_after(line)
                .or_else(|| outcome.count().checked_sub(1)),
            None => None,
        };
    }

    /// Files the daemon's answer.
    ///
    /// Answers to a query that is no longer the current one are dropped: a stale count is
    /// worse than none, because the user cannot tell it is stale.
    pub fn receive(&mut self, query: &SearchQuery, outcome: SearchOutcome) {
        if self.in_flight.as_ref() != Some(query) {
            return;
        }
        self.in_flight = None;
        let previous_line = self
            .current
            .and_then(|index| self.outcome.as_ref()?.matches.get(index))
            .map(|found| found.line);
        self.observed_len = Some(outcome.scrollback_len);
        self.refusal = None;
        self.outcome = Some(outcome);
        self.keep_position(previous_line);
        if self.queued {
            self.queued = false;
            // The queued query goes out now, and now is when it was sent: the stamp is what
            // the refresh rate limit is measured from.
            self.request_query(self.last_query_ms);
        }
    }

    /// Files a refusal: a pattern that will not compile, or a query the daemon would not run.
    pub fn refuse(&mut self, query: &SearchQuery, reason: impl Into<String>) {
        if self.in_flight.as_ref() != Some(query) {
            return;
        }
        self.in_flight = None;
        self.outcome = None;
        self.current = None;
        self.refusal = Some(reason.into());
        if self.queued {
            self.queued = false;
            self.request_query(self.last_query_ms);
        }
    }

    /// Looks at the pane being drawn and re-runs the query if the buffer has moved.
    ///
    /// Line indices are stable while output only arrives — a row keeps its number as the
    /// screen scrolls — but they shift when the daemon's ring drops its oldest rows, and new
    /// output can contain new matches. So the query is run again when the record's depth
    /// changes, rate-limited by [`REFRESH_INTERVAL_MS`] so that a build does not turn into a
    /// search per frame.
    pub fn observe(&mut self, grid: &Grid, now_ms: i64) {
        if !self.open || self.text.is_empty() {
            return;
        }
        let len = grid.scrollback_len;
        match self.observed_len {
            Some(seen) if seen == len => {}
            Some(_) => {
                if now_ms.saturating_sub(self.last_query_ms) >= REFRESH_INTERVAL_MS {
                    self.observed_len = Some(len);
                    self.request_query(now_ms);
                }
            }
            None => self.observed_len = Some(len),
        }
    }

    /// The words the bar shows: which match of how many, or why there are none.
    pub fn status(&self) -> String {
        if let Some(reason) = &self.refusal {
            return reason.clone();
        }
        if self.text.is_empty() {
            return String::new();
        }
        match &self.outcome {
            Some(outcome) => outcome.position_label(self.current),
            None => "searching…".to_string(),
        }
    }

    /// Whether the search has an answer that found nothing, which the bar says out loud.
    pub fn found_nothing(&self) -> bool {
        self.refusal.is_some()
            || self
                .outcome
                .as_ref()
                .is_some_and(|outcome| outcome.is_empty() && !self.text.is_empty())
    }

    /// The matches that fall on the grid being painted, in its own coordinates.
    ///
    /// Only the visible ones: a thousand matches across five thousand rows are at most a
    /// screen's worth on screen, and the renderer never sees the rest.
    pub fn highlights(&self, grid: &Grid) -> Vec<Highlight> {
        let Some(outcome) = self.outcome.as_ref() else {
            return Vec::new();
        };
        if !self.open {
            return Vec::new();
        }
        // The top of the viewport, in the daemon's line numbering. The grid carries both
        // halves: how much history sits above it, and how far back it is being shown.
        let Some(top) = grid.scrollback_len.checked_sub(grid.scrollback_offset) else {
            return Vec::new();
        };
        let mut highlights = Vec::new();
        for (index, found) in outcome.matches.iter().enumerate() {
            let Some(row) = found.line.checked_sub(top) else {
                continue;
            };
            if row >= usize::from(grid.rows) {
                continue;
            }
            highlights.push(Highlight {
                row: row as u16,
                col: found.col,
                cols: found.cols,
                current: self.current == Some(index),
            });
        }
        highlights
    }

    /// Whether the field should take the keyboard this frame, consuming the request.
    pub fn take_focus_request(&mut self) -> bool {
        std::mem::take(&mut self.grab_focus)
    }

    /// Everything the window has to act on, taken and cleared.
    pub fn take_intents(&mut self) -> Vec<SearchIntent> {
        std::mem::take(&mut self.intents)
    }

    /// Runs the current query, or queues it behind the one outstanding.
    fn request_query(&mut self, now_ms: i64) {
        if self.text.is_empty() {
            return;
        }
        if self.in_flight.is_some() {
            // One search at a time. The newest query wins when the answer arrives, so
            // typing quickly costs one round trip rather than one search per keystroke.
            self.queued = true;
            return;
        }
        let query = self.query();
        self.in_flight = Some(query.clone());
        self.last_query_ms = now_ms;
        self.intents.push(SearchIntent::Query(query));
    }

    /// Runs the query again after a toggle changed what it means.
    fn rerun(&mut self, now_ms: i64) {
        self.current = None;
        self.refusal = None;
        self.outcome = None;
        if !self.text.is_empty() {
            self.request_query(now_ms);
        }
    }
}

/// Trims a query to the protocol's limit before it is ever sent.
///
/// Done here rather than left to the daemon's refusal so that pasting a whole file into the
/// search field is a short search rather than an error.
fn clamp_query(text: &str) -> String {
    // A newline in a search field comes from a paste and can never match: rows are
    // searched one at a time.
    let single_line: String = text.chars().filter(|c| *c != '\n' && *c != '\r').collect();
    if single_line.chars().count() <= MAX_QUERY_CHARS {
        return single_line;
    }
    single_line.chars().take(MAX_QUERY_CHARS).collect()
}

/// Where the search bar goes: the top-right of the pane, floating over the output.
///
/// Over the content rather than in a strip of its own, because a pane's height is decided by
/// the window's layout and taking eighteen points out of it would reflow the program every
/// time somebody searched. The right-hand side keeps it away from the left margin, which is
/// where the text a user is reading begins.
pub fn bar_rect(pane: Rect) -> Rect {
    let width = BAR_WIDTH.min(pane.width());
    let height = BAR_HEIGHT.min(pane.height());
    Rect::from_min_size(
        egui::pos2(pane.max.x - width, pane.min.y),
        Vec2::new(width, height),
    )
}

/// Draws the search bar and collects what the user did to it.
///
/// Everything it does goes through [`PaneSearch`], so the behaviour is the behaviour the
/// tests exercise and this function only has to place things.
pub fn show_bar(ui: &mut Ui, theme: &Theme, pane: Rect, search: &mut PaneSearch, now_ms: i64) {
    if !search.is_open() {
        return;
    }
    // The bar is drawn with the chrome's icon font, and installing it is idempotent — so the
    // pane can say it needs the glyphs rather than depending on the window having said so.
    crate::icons::install(ui.ctx());
    let rect = bar_rect(pane);
    let painter = ui.painter().with_clip_rect(rect);
    painter.rect_filled(rect, 0.0, theme.raised);
    painter.rect_stroke(
        rect,
        0.0,
        Stroke::new(1.0, theme.border),
        egui::StrokeKind::Inside,
    );

    let mut cursor = rect.shrink2(Vec2::new(6.0, 4.0));
    // Laid out from the right: the buttons have fixed widths and the field takes what is
    // left, so a narrow pane loses field width rather than losing its controls.
    let close = take_right(&mut cursor, 24.0);
    let next = take_right(&mut cursor, 24.0);
    let previous = take_right(&mut cursor, 24.0);
    let regex = take_right(&mut cursor, 34.0);
    let case = take_right(&mut cursor, 34.0);
    let status = take_right(&mut cursor, 88.0);

    let mut text = search.text().to_string();
    let field = ui.put(
        cursor,
        egui::TextEdit::singleline(&mut text)
            .hint_text("Find")
            .font(egui::TextStyle::Monospace)
            .desired_width(cursor.width()),
    );
    label_node(ui, field.id, "Find in pane");
    if search.take_focus_request() {
        field.request_focus();
    }
    if text != search.text() {
        search.set_text(text, now_ms);
    }

    let colour = if search.found_nothing() {
        theme.attention
    } else {
        theme.text_dim
    };
    ui.painter().text(
        status.left_center(),
        Align2::LEFT_CENTER,
        search.status(),
        FontId::new(11.0, egui::FontFamily::Monospace),
        colour,
    );

    let mut case_on = search.case_sensitive();
    let case_response = ui.put(case, toggle(case_on, "Aa"));
    label_node(ui, case_response.id, "Match case");
    if case_response.clicked() {
        case_on = !case_on;
        search.set_case_sensitive(case_on, now_ms);
    }
    let mut regex_on = search.is_regex();
    let regex_response = ui.put(regex, toggle(regex_on, ".*"));
    label_node(ui, regex_response.id, "Regular expression");
    if regex_response.clicked() {
        regex_on = !regex_on;
        search.set_regex(regex_on, now_ms);
    }

    // Icons rather than characters: the chrome's font has glyphs for these and the
    // terminal's monospace face does not, so a literal arrow would come out as the
    // missing-glyph box. Each one carries its phrase into the accessibility tree, because a
    // picture on its own conveys nothing to a screen reader.
    let has_matches = search.matches() > 0;
    let back = ui.put(previous, icon(crate::icons::PREVIOUS));
    label_node(ui, back.id, "Previous match");
    if back.clicked() && has_matches {
        search.previous_match();
    }
    let forward = ui.put(next, icon(crate::icons::NEXT));
    label_node(ui, forward.id, "Next match");
    if forward.clicked() && has_matches {
        search.next_match();
    }
    let dismiss = ui.put(close, icon(crate::icons::CLOSE));
    label_node(ui, dismiss.id, "Close search");
    if dismiss.clicked() {
        search.close();
        return;
    }

    // The keys a find field is expected to answer, taken only while it has the keyboard so
    // that Escape still reaches the program when the field does not.
    if field.has_focus() {
        let (enter, shift, escape) = ui.input(|i| {
            (
                i.key_pressed(Key::Enter),
                i.modifiers.shift,
                i.key_pressed(Key::Escape),
            )
        });
        if enter {
            if shift {
                search.previous_match();
            } else {
                search.next_match();
            }
        }
        if escape {
            search.close();
        }
    }
}

/// One of the bar's two-character toggles.
///
/// Set a size down from the body text, because the bar is narrow and a button whose label
/// does not fit is a button that shows half of it — which for "Aa" is the half that says
/// nothing.
fn toggle(on: bool, label: &str) -> egui::Button<'_> {
    egui::Button::selectable(on, egui::RichText::new(label).size(11.0))
}

/// One of the bar's icon buttons.
fn icon(glyph: &str) -> egui::Button<'_> {
    egui::Button::new(egui::RichText::new(glyph).font(crate::icons::font(13.0)))
}

/// Splits a fixed width off the right of a rectangle.
fn take_right(cursor: &mut Rect, width: f32) -> Rect {
    let width = width.min(cursor.width());
    let taken = Rect::from_min_max(egui::pos2(cursor.max.x - width, cursor.min.y), cursor.max);
    cursor.max.x -= width + 2.0;
    taken
}

/// Gives a control a name a screen reader can read.
///
/// The glyphs on these buttons are two characters wide because the bar is narrow, and "Aa"
/// read out on its own means nothing. A GPU-drawn pane has no DOM to fall back on, so the
/// name has to be set explicitly.
fn label_node(ui: &Ui, id: egui::Id, label: &str) {
    ui.ctx().accesskit_node_builder(id, |node| {
        node.set_label(label);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use turn_proto::search::PaneMatch;

    const T0: i64 = 1_700_000_000_000;

    fn outcome(lines: &[usize], scrollback_len: usize) -> SearchOutcome {
        SearchOutcome {
            matches: lines
                .iter()
                .map(|line| PaneMatch::new(*line, 4, 5))
                .collect(),
            truncated: false,
            scanned_lines: scrollback_len + 40,
            total_lines: scrollback_len + 40,
            screen_rows: 40,
            scrollback_len,
        }
    }

    /// A pane's grid as it is painted while scrolled back: the offset it is showing and the
    /// history above it.
    fn viewport(rows: u16, offset: usize, len: usize) -> Grid {
        let mut grid = Grid::blank(rows, 80);
        grid.scrollback_offset = offset;
        grid.scrollback_len = len;
        grid
    }

    #[test]
    fn opening_the_search_remembers_where_the_user_was_and_leaving_puts_them_back() {
        let mut search = PaneSearch::default();
        assert!(!search.is_open());

        search.open(1_240, T0);
        assert!(search.is_open());
        assert!(
            search.take_intents().is_empty(),
            "an empty search asks the daemon for nothing"
        );
        assert!(search.take_focus_request(), "the field takes the keyboard");
        assert!(!search.take_focus_request(), "once");

        search.close();
        assert!(!search.is_open());
        assert_eq!(
            search.take_intents(),
            vec![SearchIntent::Restore { offset: 1_240 }],
            "a search that abandons you where it finished is one you stop using"
        );
    }

    #[test]
    fn typing_asks_the_daemon_and_the_answer_becomes_the_count() {
        let mut search = PaneSearch::default();
        search.open(0, T0);
        search.set_text("error".into(), T0);

        let query = SearchQuery::literal("error");
        assert_eq!(
            search.take_intents(),
            vec![SearchIntent::Query(query.clone())]
        );
        assert_eq!(search.status(), "searching…");

        search.receive(&query, outcome(&[10, 900, 4_020], 4_000));
        assert_eq!(search.matches(), 3);
        assert_eq!(
            search.status(),
            "3 matches",
            "before the user has stepped to one of them"
        );

        assert!(search.next_match());
        assert_eq!(search.status(), "1 of 3");
        assert_eq!(
            search.take_intents(),
            vec![SearchIntent::Reveal { line: 10 }]
        );
    }

    /// The sentence the whole feature is judged by.
    #[test]
    fn the_bar_says_which_match_of_how_many() {
        let mut search = PaneSearch::default();
        search.open(0, T0);
        search.set_text("error".into(), T0);
        let query = SearchQuery::literal("error");
        let lines: Vec<usize> = (0..200).map(|index| index * 3).collect();
        search.receive(&query, outcome(&lines, 1_000));
        let _ = search.take_intents();

        for expected in ["1 of 200", "2 of 200", "3 of 200"] {
            assert!(search.next_match());
            assert_eq!(search.status(), expected);
        }
        assert_eq!(search.current(), Some(2));
    }

    #[test]
    fn next_and_previous_wrap_rather_than_going_quiet_at_the_ends() {
        let mut search = PaneSearch::default();
        search.open(0, T0);
        search.set_text("x".into(), T0);
        let query = SearchQuery::literal("x");
        search.receive(&query, outcome(&[5, 50, 500], 900));
        let _ = search.take_intents();

        assert!(search.next_match());
        assert!(search.next_match());
        assert!(search.next_match());
        assert_eq!(search.current(), Some(2));
        assert!(search.next_match());
        assert_eq!(search.current(), Some(0), "forwards wraps to the first");
        assert!(search.previous_match());
        assert_eq!(search.current(), Some(2), "and backwards to the last");

        // Every step asks for its line to be shown.
        assert_eq!(
            search.take_intents(),
            vec![
                SearchIntent::Reveal { line: 5 },
                SearchIntent::Reveal { line: 50 },
                SearchIntent::Reveal { line: 500 },
                SearchIntent::Reveal { line: 5 },
                SearchIntent::Reveal { line: 500 },
            ]
        );
    }

    #[test]
    fn stepping_backwards_first_starts_at_the_newest_match() {
        let mut search = PaneSearch::default();
        search.open(0, T0);
        search.set_text("x".into(), T0);
        search.receive(&SearchQuery::literal("x"), outcome(&[5, 50, 500], 900));
        let _ = search.take_intents();
        assert!(search.previous_match());
        assert_eq!(
            search.current(),
            Some(2),
            "backwards from nowhere means the most recent output"
        );
    }

    #[test]
    fn a_search_with_no_matches_says_so_rather_than_looking_broken() {
        let mut search = PaneSearch::default();
        search.open(0, T0);
        search.set_text("nothing at all".into(), T0);
        let _ = search.take_intents();
        search.receive(&SearchQuery::literal("nothing at all"), outcome(&[], 4_000));
        assert_eq!(search.status(), "no matches");
        assert!(search.found_nothing());
        assert!(!search.next_match(), "and there is nowhere to go");
        assert!(
            search.take_intents().is_empty(),
            "with nothing found there is nowhere to reveal"
        );
    }

    #[test]
    fn a_truncated_result_set_says_the_count_is_a_floor() {
        let mut search = PaneSearch::default();
        search.open(0, T0);
        search.set_text("a".into(), T0);
        let mut found = outcome(&(0..1_000).collect::<Vec<_>>(), 4_000);
        found.truncated = true;
        search.receive(&SearchQuery::literal("a"), found);
        assert!(search.next_match());
        assert_eq!(search.status(), "1 of 1000+");
    }

    /// A pattern the user is half way through typing is not an error to shout about, but a
    /// pattern that cannot compile has to say why.
    #[test]
    fn a_refused_pattern_shows_the_reason_instead_of_finding_nothing_silently() {
        let mut search = PaneSearch::default();
        search.open(0, T0);
        search.set_regex(true, T0);
        search.set_text("(unclosed".into(), T0);
        let query = SearchQuery::regex("(unclosed");
        assert_eq!(
            search.take_intents(),
            vec![SearchIntent::Query(query.clone())]
        );

        search.refuse(&query, "unclosed group");
        assert_eq!(search.status(), "unclosed group");
        assert!(search.found_nothing());
        assert_eq!(search.matches(), 0);

        // And fixing it clears the complaint.
        search.set_text("(closed)".into(), T0);
        assert!(search.status().contains("searching"));
    }

    /// The cost bound on the client's side: however fast somebody types, one search is
    /// outstanding at a time and only the newest one is run.
    #[test]
    fn typing_quickly_costs_one_search_per_round_trip_rather_than_one_per_keystroke() {
        let mut search = PaneSearch::default();
        search.open(0, T0);
        for text in ["e", "er", "err", "erro", "error"] {
            search.set_text(text.into(), T0);
        }
        let asked = search.take_intents();
        assert_eq!(
            asked,
            vec![SearchIntent::Query(SearchQuery::literal("e"))],
            "the first went; the rest coalesced"
        );

        // The answer to the first arrives, and the newest query goes out.
        search.receive(&SearchQuery::literal("e"), outcome(&[1], 10));
        assert_eq!(
            search.take_intents(),
            vec![SearchIntent::Query(SearchQuery::literal("error"))]
        );
        assert!(
            search.matches() > 0,
            "the stale answer is still shown until a better one arrives"
        );
    }

    #[test]
    fn an_answer_to_a_query_nobody_is_waiting_for_is_dropped() {
        let mut search = PaneSearch::default();
        search.open(0, T0);
        search.set_text("error".into(), T0);
        let _ = search.take_intents();

        search.receive(
            &SearchQuery::literal("something else"),
            outcome(&[1, 2], 10),
        );
        assert_eq!(
            search.matches(),
            0,
            "an answer to another question must not become this one's count"
        );
        search.receive(&SearchQuery::literal("error"), outcome(&[1, 2], 10));
        assert_eq!(search.matches(), 2);
    }

    #[test]
    fn the_toggles_change_what_is_asked_for() {
        let mut search = PaneSearch::default();
        search.open(0, T0);
        search.set_text("Error".into(), T0);
        let _ = search.take_intents();
        // The answer to the first query, so the toggle is not queued behind it: one search
        // is outstanding at a time.
        search.receive(&SearchQuery::literal("Error"), outcome(&[3], 9));

        search.set_case_sensitive(true, T0);
        assert_eq!(
            search.take_intents(),
            vec![SearchIntent::Query(
                SearchQuery::literal("Error").with_case_sensitive(true)
            )],
            "case sensitivity is a toggle, and off is the default"
        );

        search.receive(
            &SearchQuery::literal("Error").with_case_sensitive(true),
            outcome(&[3], 9),
        );
        search.set_regex(true, T0);
        assert_eq!(
            search.take_intents(),
            vec![SearchIntent::Query(
                SearchQuery::regex("Error").with_case_sensitive(true)
            )]
        );
        assert_eq!(
            search.matches(),
            0,
            "and the old answer is dropped, because it answered a different question"
        );
    }

    #[test]
    fn emptying_the_field_takes_the_highlights_away() {
        let mut search = PaneSearch::default();
        search.open(0, T0);
        search.set_text("error".into(), T0);
        search.receive(&SearchQuery::literal("error"), outcome(&[1_000], 4_000));
        let _ = search.take_intents();
        assert_eq!(search.highlights(&viewport(40, 3_020, 4_000)).len(), 1);

        search.set_text(String::new(), T0);
        assert_eq!(search.matches(), 0);
        assert!(search.highlights(&viewport(40, 3_020, 4_000)).is_empty());
        assert_eq!(search.status(), "");
        assert!(search.take_intents().is_empty());
    }

    /// Only the matches on screen are handed to the renderer, and exactly one of them is
    /// the current one.
    #[test]
    fn the_matches_on_screen_are_the_ones_painted_and_the_current_one_is_distinguished() {
        let mut search = PaneSearch::default();
        search.open(0, T0);
        search.set_text("error".into(), T0);
        // History of 4,000 rows; matches near the start, in the middle and on the live
        // screen.
        search.receive(
            &SearchQuery::literal("error"),
            outcome(&[10, 3_500, 3_510, 4_010], 4_000),
        );
        let _ = search.take_intents();

        // Scrolled so that lines 3,500 to 3,540 are on screen.
        let grid = viewport(40, 500, 4_000);
        let highlights = search.highlights(&grid);
        assert_eq!(highlights.len(), 2, "{highlights:?}");
        assert_eq!(highlights[0].row, 0);
        assert_eq!((highlights[0].col, highlights[0].cols), (4, 5));
        assert_eq!(highlights[1].row, 10);
        assert!(
            highlights.iter().all(|h| !h.current),
            "nothing is current until the user steps to it"
        );

        // Stepping to the second on-screen match marks exactly that one.
        assert!(search.next_match());
        assert!(search.next_match());
        let highlights = search.highlights(&grid);
        assert_eq!(
            highlights.iter().filter(|h| h.current).count(),
            1,
            "{highlights:?}"
        );
        assert!(highlights[0].current, "the second match is on the top row");

        // And on the live screen, the last match is the one visible.
        let live = viewport(40, 0, 4_000);
        let visible = search.highlights(&live);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].row, 10);
    }

    #[test]
    fn a_closed_search_paints_nothing_even_if_it_still_had_results() {
        let mut search = PaneSearch::default();
        search.open(0, T0);
        search.set_text("error".into(), T0);
        search.receive(&SearchQuery::literal("error"), outcome(&[4_010], 4_000));
        assert_eq!(search.highlights(&viewport(40, 0, 4_000)).len(), 1);
        search.close();
        assert!(search.highlights(&viewport(40, 0, 4_000)).is_empty());
    }

    #[test]
    fn the_current_match_wins_the_cell_it_shares_with_another() {
        let highlights = vec![
            Highlight {
                row: 2,
                col: 4,
                cols: 5,
                current: false,
            },
            Highlight {
                row: 2,
                col: 6,
                cols: 2,
                current: true,
            },
        ];
        assert_eq!(mark_at(&highlights, 2, 4), Some(Mark::Other));
        assert_eq!(mark_at(&highlights, 2, 6), Some(Mark::Current));
        assert_eq!(mark_at(&highlights, 2, 8), Some(Mark::Other));
        assert_eq!(mark_at(&highlights, 2, 9), None);
        assert_eq!(mark_at(&highlights, 3, 4), None);
    }

    /// New output can bring new matches and, once the daemon's ring starts dropping rows,
    /// moves every line index. So the query is run again — but not once a frame.
    #[test]
    fn new_output_re_runs_the_query_at_most_a_few_times_a_second() {
        let mut search = PaneSearch::default();
        search.open(0, T0);
        search.set_text("error".into(), T0);
        let query = SearchQuery::literal("error");
        let _ = search.take_intents();
        search.receive(&query, outcome(&[100], 4_000));
        let _ = search.take_intents();

        // A frame at the same depth asks for nothing.
        search.observe(&viewport(40, 0, 4_000), T0);
        assert!(search.take_intents().is_empty());

        // The buffer moves, but only a moment has passed.
        search.observe(&viewport(40, 0, 4_020), T0 + 10);
        assert!(
            search.take_intents().is_empty(),
            "a search per frame during a build is what the rate limit is for"
        );

        // Once the interval has passed, it looks again.
        search.observe(&viewport(40, 0, 4_050), T0 + REFRESH_INTERVAL_MS);
        assert_eq!(
            search.take_intents(),
            vec![SearchIntent::Query(query.clone())]
        );

        // And the position survives the new answer: the match the user was on is still the
        // one they are on.
        assert!(search.next_match());
        let _ = search.take_intents();
        search.receive(&query, outcome(&[100, 4_100], 4_050));
        assert_eq!(search.current(), Some(0));
        assert_eq!(
            search
                .outcome()
                .and_then(|o| o.matches.first())
                .map(|m| m.line),
            Some(100)
        );
    }

    /// A search field is not a program. A pasted file is trimmed rather than refused, and a
    /// pasted newline cannot match anything because rows are searched one at a time.
    #[test]
    fn a_pasted_essay_is_trimmed_to_something_a_search_can_be() {
        let mut search = PaneSearch::default();
        search.open(0, T0);
        search.set_text("x".repeat(MAX_QUERY_CHARS + 500), T0);
        assert_eq!(search.text().chars().count(), MAX_QUERY_CHARS);
        assert_eq!(search.query().text.chars().count(), MAX_QUERY_CHARS);

        search.set_text("two\nlines".into(), T0);
        assert_eq!(search.text(), "twolines");
    }

    #[test]
    fn opening_with_some_text_searches_for_it_straight_away() {
        let mut search = PaneSearch::default();
        search.open_with("error[E0599]", 40, T0);
        assert_eq!(search.text(), "error[E0599]");
        assert_eq!(
            search.take_intents(),
            vec![SearchIntent::Query(SearchQuery::literal("error[E0599]"))],
            "a literal search, so the brackets are brackets"
        );
        search.close();
        assert_eq!(
            search.take_intents(),
            vec![SearchIntent::Restore { offset: 40 }]
        );
    }

    /// Re-opening the search keeps what was last looked for, which is what every editor
    /// does, and runs it again against whatever has happened since.
    #[test]
    fn reopening_the_search_looks_for_the_same_thing_again() {
        let mut search = PaneSearch::default();
        search.open(0, T0);
        search.set_text("error".into(), T0);
        search.receive(&SearchQuery::literal("error"), outcome(&[7], 10));
        let _ = search.take_intents();
        search.close();
        let _ = search.take_intents();

        search.open(0, T0);
        assert_eq!(search.text(), "error");
        assert_eq!(
            search.take_intents(),
            vec![SearchIntent::Query(SearchQuery::literal("error"))]
        );
    }

    #[test]
    fn the_bar_sits_in_the_top_right_and_never_leaves_the_pane() {
        let pane = Rect::from_min_size(egui::pos2(100.0, 50.0), Vec2::new(900.0, 400.0));
        let bar = bar_rect(pane);
        assert_eq!(bar.max.x, pane.max.x);
        assert_eq!(bar.min.y, pane.min.y);
        assert_eq!(bar.width(), BAR_WIDTH);

        // A pane narrower than the bar gets a bar the width of the pane rather than one
        // hanging off the side of it.
        let narrow = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(200.0, 100.0));
        assert_eq!(bar_rect(narrow).width(), 200.0);
        assert!(narrow.contains_rect(bar_rect(narrow)));
    }
}
