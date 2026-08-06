//! The icons the window's chrome is drawn with.
//!
//! Two rules, and they are the reason this is a module rather than a handful of
//! string literals sprinkled through the view.
//!
//! **An icon is never the only thing that says what a control does.** Every icon here
//! is paired with a `label` — a full phrase — and the two are handed to
//! [`icon_button`] together, which writes the phrase into the accessibility tree and
//! into the tooltip. A picture on its own conveys the action by appearance, which
//! fails a screen reader and fails anybody who has not learnt the picture.
//!
//! **The font is installed once, from the view itself.** `egui::Context::add_font` is
//! idempotent — it compares the font's name against what is already loaded and does
//! nothing when it is present — so [`install`] can be called on every frame and no
//! caller has to remember to do it at startup. The glyphs live in the Unicode private
//! use area, so the icon font is added as the *lowest* priority fallback of the
//! proportional family: it can only ever be reached by a codepoint no real text uses,
//! and no ordinary character changes shape because it is loaded.

use egui::{Response, RichText, Ui, Vec2};

pub use egui_phosphor::regular::{
    ARCHIVE, BELL, CARET_DOWN as NEXT, CARET_UP as PREVIOUS, COMMAND, FILE_PLUS, FOLDER_PLUS, GEAR,
    KEYBOARD, LAYOUT, PLUS_SQUARE, POWER, TRAY_ARROW_UP as UNARCHIVE, X as CLOSE,
};

/// The name the icon font is registered under.
const FONT: &str = "phosphor-regular";

/// Makes the icon glyphs available on `ctx`.
///
/// Cheap to call every frame: `add_font` looks the name up in the loaded definitions
/// and returns without work when it is already there.
pub fn install(ctx: &egui::Context) {
    ctx.add_font(egui::epaint::text::FontInsert::new(
        FONT,
        egui::FontData::from_static(egui_phosphor::Variant::Regular.font_bytes()),
        vec![egui::epaint::text::InsertFontFamily {
            family: egui::FontFamily::Proportional,
            // Lowest, deliberately. The icons occupy the private use area, so nothing
            // that is actually text can resolve to them, and a higher priority would
            // put an icon font in front of the face the body text is set in.
            priority: egui::epaint::text::FontPriority::Lowest,
        }],
    ));
}

/// A square icon button that says in words what it does.
///
/// `label` is the phrase a person hears and reads: it becomes the accessible name and
/// the tooltip. `shortcut` is appended to the tooltip when the action has one, so the
/// chrome teaches the keyboard instead of hiding it.
pub fn icon_button(
    ui: &mut Ui,
    icon: &str,
    label: &str,
    shortcut: Option<&str>,
    enabled: bool,
) -> Response {
    let button = egui::Button::new(RichText::new(icon).size(15.0)).min_size(SIZE);
    let response = ui.add_enabled(enabled, button);
    let tooltip = match shortcut {
        Some(shortcut) if !shortcut.is_empty() => format!("{label} · {shortcut}"),
        _ => label.to_string(),
    };
    describe(&response, label);
    response.on_hover_text(tooltip)
}

/// The size every icon button in the chrome is drawn at.
///
/// Fixed rather than derived from the glyph so a row of them is a row of equal
/// squares, and so the number that fit in a bar can be worked out before drawing —
/// see `toolbar_capacity` in the view.
pub const SIZE: Vec2 = Vec2::new(26.0, 22.0);

/// The horizontal room one icon button needs, gap included.
pub const PITCH: f32 = SIZE.x + 4.0;

/// The size a control that sits *inside* a row of the tree is drawn at.
///
/// Smaller than [`SIZE`], and it has to be: a Workspace row is 34 points tall and
/// carries three of these beside a name and a status tag, so a chrome-sized button
/// would either outgrow the row or leave no room for the words.
pub const ROW_SIZE: Vec2 = Vec2::new(20.0, 18.0);

/// The horizontal room one row control needs, gap included.
///
/// The row reserves `count * ROW_PITCH` before it paints anything, which is what keeps
/// a name from running underneath a button — see `row_action_width` in the view.
pub const ROW_PITCH: f32 = ROW_SIZE.x + 4.0;

/// The size the glyph on a row control is drawn at.
///
/// 12.5 rather than the 11 it began as. Phosphor's strokes are one unit wide at any size, so
/// at 11 points in a 20-point box the archive drawer and the page-with-a-plus were the same
/// grey smudge in a screenshot — legible only if you already knew which was which.
const ROW_GLYPH: f32 = 12.5;

/// A control drawn on a row of the tree: an icon, named in words, teaching its chord.
///
/// `at` is the exact rectangle the row reserved — see `row_action_slot` in the view — and the
/// control fills it. **It has to be given rather than allocated**, and that is the whole
/// shape of this function. `egui`'s `Button` takes its alignment from the `Ui` it is added to
/// and offers no knob of its own, so a button added to a region inherited that region's
/// left-and-top alignment: the boxes landed in their columns, and the glyphs sat inside them
/// at whatever offset each glyph's own width produced. A row came out visibly ragged, twice,
/// and both times it read as "the buttons are misaligned" because that is what it looked
/// like.
///
/// `label` is the accessible name and the first line of the tooltip, and it names the exact
/// target — "Close session Fix climbing bugs", not "Close". `detail` says what the action
/// will and will not do, because on these rows the difference between archiving and closing
/// is the difference between tidying up and stopping work. `shortcut` is the chord for the
/// keyboard in front of the user, so the row teaches it instead of hiding it in a sheet.
pub fn row_button(
    ui: &mut Ui,
    at: egui::Rect,
    icon: &str,
    label: &str,
    detail: &str,
    shortcut: Option<&str>,
    enabled: bool,
) -> Response {
    let mut tooltip = format!("{label} — {detail}");
    if let Some(shortcut) = shortcut.filter(|shortcut| !shortcut.is_empty()) {
        tooltip.push_str(" · ");
        tooltip.push_str(shortcut);
    }
    let response = ui
        .scope_builder(
            egui::UiBuilder::new().max_rect(at).layout(
                // Centred on both axes and justified to the rectangle. This is the line that
                // puts the glyph in the middle of its box and makes every box the same size
                // whatever glyph it carries — the two halves of the reported defect.
                egui::Layout::centered_and_justified(egui::Direction::TopDown),
            ),
            |ui| {
                // Zero padding: the box is [`ROW_SIZE`] and the glyph is centred in it, so
                // padding would only shrink the glyph. `interact_size` because a button is
                // floored at it, and the style's is wider *and* taller than a row can hold.
                ui.spacing_mut().button_padding = Vec2::ZERO;
                ui.spacing_mut().interact_size = ROW_SIZE;
                let button = egui::Button::new(RichText::new(icon).size(ROW_GLYPH))
                    .min_size(ROW_SIZE)
                    // Ink at rest, a box under the pointer. Three framed boxes on every
                    // Workspace row competed with the name and the state for a row whose job
                    // is to be scanned; the frame still appears the moment the control is
                    // hovered or focused, so nothing is hidden and nothing moves when it does.
                    .frame_when_inactive(false);
                ui.add_enabled(enabled, button)
            },
        )
        .inner;
    describe(&response, label);
    response.on_hover_text(tooltip)
}

/// Names a control whose visible content is a glyph.
///
/// Written after the widget so it wins: `Button` fills the node in from its own text,
/// which for an icon button is a private-use codepoint — a screen reader would read
/// nothing, or read the codepoint out. The label is applied on every frame, including
/// the frame the button is clicked on.
pub fn describe(response: &Response, label: &str) {
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, response.enabled(), label)
    });
    response.ctx.accesskit_node_builder(response.id, |node| {
        node.set_role(egui::accesskit::Role::Button);
        node.set_label(label.to_string());
        node.add_action(egui::accesskit::Action::Click);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every icon is a single private-use codepoint. A multi-character "icon" would be
    /// a string that happened to render, and an ASCII one would mean the font is not
    /// being used at all.
    #[test]
    fn every_named_icon_is_one_glyph_from_the_icon_font() {
        for (name, icon) in [
            ("close", CLOSE),
            ("new pane", PLUS_SQUARE),
            ("layout", LAYOUT),
            ("new session", FILE_PLUS),
            ("new workspace", FOLDER_PLUS),
            ("palette", COMMAND),
            ("attention", BELL),
            ("shortcuts", KEYBOARD),
            ("settings", GEAR),
            ("archive", ARCHIVE),
            ("unarchive", UNARCHIVE),
            ("stop the processes", POWER),
        ] {
            let mut chars = icon.chars();
            let glyph = chars.next().unwrap_or_default();
            assert_eq!(chars.next(), None, "{name} is more than one glyph");
            assert!(
                ('\u{E000}'..='\u{F8FF}').contains(&glyph),
                "{name} is not a private-use codepoint and would collide with text"
            );
        }
    }

    /// Taking a row out of the tree and stopping its processes are different acts, and
    /// the two controls that do them sit next to each other. Sharing a glyph — or sharing
    /// the glyph that already means "close this view, nothing stops" on a pane header —
    /// would be an invitation to destroy work while tidying up.
    #[test]
    fn archiving_and_stopping_never_look_like_each_other_or_like_closing_a_view() {
        let lifecycle = [
            ("archive", ARCHIVE),
            ("unarchive", UNARCHIVE),
            ("stop", POWER),
        ];
        for (name, icon) in lifecycle {
            assert_ne!(
                icon, CLOSE,
                "{name} must not borrow the glyph that closes a view without stopping it"
            );
        }
        for (first_name, first) in lifecycle {
            for (second_name, second) in lifecycle {
                if first_name != second_name {
                    assert_ne!(first, second, "{first_name} and {second_name} look alike");
                }
            }
        }
    }

    /// Installing twice must be free, because the view does it on every frame.
    #[test]
    fn installing_the_icon_font_twice_leaves_one_copy_of_it() {
        let ctx = egui::Context::default();
        for _ in 0..2 {
            let _ = ctx.run_ui(Default::default(), |ui| install(ui.ctx()));
        }
        let names = ctx.fonts(|fonts| {
            fonts
                .definitions()
                .font_data
                .keys()
                .filter(|name| name.as_str() == FONT)
                .count()
        });
        assert_eq!(names, 1);
    }
}
