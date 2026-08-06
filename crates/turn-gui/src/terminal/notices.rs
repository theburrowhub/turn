//! What Turn has to say about a pane, said in Turn's own furniture.
//!
//! There is exactly one thing in here, and it exists because of a bug worth remembering.
//! When a pane refused to draw a picture, the sentence explaining why was **written into
//! the emulated screen** at the program's cursor, with a newline after it. It looked
//! reasonable in a test and was wrong in front of a user: an agent's startup line came out
//! cut in half by a sentence about images, and the row below it shifted by one. A program's
//! screen is the program's. It laid that screen out, it positions its cursor absolutely, and
//! it repaints — so text Turn inserted is never overwritten and can never be corrected.
//!
//! The reason for showing the refusal at all has not changed: a picture that silently did
//! not appear is a bug report nobody can write. Only the surface changed. The notice is now
//! drawn over the bottom of the pane, in the same register as the find bar, by Turn.
//!
//! Two properties it needs and the grid could not give it:
//!
//! * **Dismissable.** It covers a row of output while it is up, so the user must be able to
//!   put it away. Nothing Turn says about a pane is more important than the pane.
//! * **It comes back.** Dismissing what a pane has said so far must not silence the next
//!   thing it says, or the second refused picture would be the silent one.

use egui::{Align2, FontId, Rect, Stroke, Ui, Vec2};
use turn_proto::images::ImageNotice;

use crate::theme::Theme;

/// Height of the strip, in points. One line of the chrome's small face plus its padding.
const STRIP_HEIGHT: f32 = 22.0;

/// Width of the dismiss button.
const DISMISS_WIDTH: f32 = 22.0;

/// Whether the strip has been put away, and for what.
///
/// Keyed on how much the pane had refused when the user dismissed it, rather than on a plain
/// boolean: a boolean would silence every later refusal too, and the second picture that did
/// not appear is exactly as worth reporting as the first.
///
/// A refusal of a *ninth* distinct kind does not reopen a dismissed strip, because the pane
/// tracks eight and the ninth changes no count. That is a deliberate consequence of the cap
/// rather than an oversight; a pane that has produced eight distinct refusals has already
/// said everything this strip can say.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NoticeStrip {
    dismissed_at: Option<u64>,
}

impl NoticeStrip {
    /// Whether the strip should be drawn for these notices.
    pub fn is_open(&self, notices: &[ImageNotice]) -> bool {
        !notices.is_empty() && self.dismissed_at != Some(weight(notices))
    }

    /// Puts the strip away for everything the pane has refused so far.
    pub fn dismiss(&mut self, notices: &[ImageNotice]) {
        self.dismissed_at = Some(weight(notices));
    }

    /// Forgets the dismissal, so the next refusal is shown again.
    ///
    /// Called when a pane's notices are cleared rather than added to — a pane that has
    /// nothing to say has nothing dismissed either.
    pub fn reset(&mut self) {
        self.dismissed_at = None;
    }
}

/// How much a pane has refused, as the strip counts it.
///
/// The sum of the counts, not the number of kinds: a second refusal of a kind already shown
/// is still a second picture that did not appear.
fn weight(notices: &[ImageNotice]) -> u64 {
    notices
        .iter()
        .map(|notice| u64::from(notice.count))
        .fold(0u64, |total, count| total.saturating_add(count))
}

/// The line the strip shows.
///
/// Joined rather than stacked: the strip is one row tall so that it covers as little of the
/// program's screen as possible, and a pane that refused two different things is rare enough
/// that a separator is a better answer than a second row. A count is shown only when it is
/// more than one, because "x1" on every notice is noise.
pub fn summary(notices: &[ImageNotice]) -> String {
    notices
        .iter()
        .map(|notice| {
            if notice.count > 1 {
                format!("{} x{}", notice.text, notice.count)
            } else {
                notice.text.clone()
            }
        })
        .collect::<Vec<_>>()
        .join("  ")
}

/// Where the strip sits: across the bottom of the pane.
///
/// The bottom because the find bar is at the top right, and two pieces of Turn's furniture
/// overlapping each other would be worse than either of them covering a row.
pub fn strip_rect(pane: Rect) -> Rect {
    let height = STRIP_HEIGHT.min(pane.height());
    Rect::from_min_max(
        egui::pos2(pane.min.x, pane.max.y - height),
        egui::pos2(pane.max.x, pane.max.y),
    )
}

/// Draws the strip and collects whether the user put it away.
///
/// Returns `true` when it was dismissed this frame, so the caller can record it against the
/// notices it was showing rather than against whatever arrives next.
pub fn show(
    ui: &mut Ui,
    theme: &Theme,
    pane: Rect,
    notices: &[ImageNotice],
    strip: &NoticeStrip,
) -> bool {
    if !strip.is_open(notices) {
        return false;
    }
    let rect = strip_rect(pane);
    let painter = ui.painter().with_clip_rect(rect);
    painter.rect_filled(rect, 0.0, theme.raised);
    // A line along the top edge only, so the strip reads as sitting over the screen rather
    // than as a box floating in it.
    painter.line_segment(
        [rect.left_top(), rect.right_top()],
        Stroke::new(1.0, theme.border),
    );

    let mut cursor = rect.shrink2(Vec2::new(6.0, 3.0));
    let dismiss = Rect::from_min_max(
        egui::pos2(
            cursor.max.x - DISMISS_WIDTH.min(cursor.width()),
            cursor.min.y,
        ),
        cursor.max,
    );
    cursor.max.x = dismiss.min.x - 4.0;

    // `text_dim`, not `attention`: exactly one thing on screen is allowed to be loud, and a
    // picture Turn would not draw is not it. The sentence is already bracketed and prefixed,
    // which is what tells the user whose voice it is.
    painter.text(
        cursor.left_center(),
        Align2::LEFT_CENTER,
        summary(notices),
        FontId::new(11.0, egui::FontFamily::Monospace),
        theme.text_dim,
    );

    // The glyph comes from the chrome's icon font, and installing it is idempotent — so the
    // strip can say it needs it rather than depending on the window having said so.
    crate::icons::install(ui.ctx());
    let response = ui.put(
        dismiss,
        egui::Button::new(egui::RichText::new(crate::icons::CLOSE).size(12.0)).frame(false),
    );
    // The button's name carries the sentence, not just "dismiss". A screen reader user has
    // no other way to reach text that was painted rather than laid out, and the reason a
    // picture did not appear is the whole point of the strip.
    crate::icons::describe(&response, &format!("Dismiss: {}", summary(notices)));
    response.clicked()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notice(text: &str, count: u32) -> ImageNotice {
        ImageNotice {
            text: text.into(),
            count,
        }
    }

    #[test]
    fn a_pane_that_refused_nothing_shows_no_strip() {
        let strip = NoticeStrip::default();
        assert!(!strip.is_open(&[]));
    }

    #[test]
    fn a_refusal_opens_the_strip() {
        let strip = NoticeStrip::default();
        assert!(strip.is_open(&[notice("[turn: image not shown — nope]", 1)]));
    }

    /// The property the whole type exists for: putting it away must not silence the next one.
    #[test]
    fn dismissing_hides_what_was_shown_and_not_what_comes_after() {
        let first = vec![notice("[turn: image not shown — nope]", 1)];
        let mut strip = NoticeStrip::default();
        strip.dismiss(&first);
        assert!(!strip.is_open(&first), "the dismissed strip must stay away");

        // The same kind again: a second picture that did not appear.
        let again = vec![notice("[turn: image not shown — nope]", 2)];
        assert!(
            strip.is_open(&again),
            "a second refusal must be shown even after the first was dismissed"
        );

        // And a different kind, with the first still counted once.
        let other = vec![
            notice("[turn: image not shown — nope]", 1),
            notice("[turn: image not shown — too big]", 1),
        ];
        assert!(strip.is_open(&other), "a new kind of refusal must be shown");
    }

    #[test]
    fn a_pane_whose_notices_were_cleared_forgets_the_dismissal() {
        let notices = vec![notice("[turn: image not shown — nope]", 1)];
        let mut strip = NoticeStrip::default();
        strip.dismiss(&notices);
        strip.reset();
        assert!(strip.is_open(&notices));
    }

    #[test]
    fn the_summary_counts_repeats_and_leaves_a_single_one_alone() {
        assert_eq!(
            summary(&[notice("[turn: image not shown — nope]", 1)]),
            "[turn: image not shown — nope]"
        );
        assert_eq!(
            summary(&[notice("[turn: image not shown — nope]", 4)]),
            "[turn: image not shown — nope] x4"
        );
        assert_eq!(
            summary(&[notice("one", 1), notice("two", 2)]),
            "one  two x2"
        );
    }

    /// The strip covers the bottom of the pane and nothing outside it, however small the
    /// pane is: a strip taller than its pane would paint over the pane beneath it.
    #[test]
    fn the_strip_stays_inside_its_pane() {
        let pane = Rect::from_min_size(egui::pos2(10.0, 20.0), Vec2::new(400.0, 300.0));
        let rect = strip_rect(pane);
        assert!(pane.contains_rect(rect));
        assert_eq!(rect.max.y, pane.max.y);
        assert_eq!(rect.width(), pane.width());

        let tiny = Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(40.0, 8.0));
        assert!(tiny.contains_rect(strip_rect(tiny)));
    }

    /// A count of zero would make the weight of two different notice sets equal and could
    /// silence a refusal. The protocol refuses it; this is the client agreeing.
    #[test]
    fn the_weight_of_a_refusal_never_goes_backwards() {
        let one = vec![notice("a", 1)];
        let two = vec![notice("a", 1), notice("b", 1)];
        assert!(weight(&two) > weight(&one));
    }
}
