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

/// The glyphs Turn draws, by their codepoint in the Phosphor font.
///
/// Declared here rather than imported from `egui-phosphor`, which bundles the same font and
/// generates a constant for all nine thousand icons. That crate pins `egui = "0.35"`, and cargo
/// reads the pin as "not 0.36" — so a dependency whose whole job is a font file and a list of
/// numbers was deciding which version of egui Turn could build against. The font is vendored
/// beside this file (see `assets/fonts/NOTICE.md`, MIT) and these are the fourteen numbers.
///
/// The names are Turn's, not Phosphor's, where the two disagree: an icon is named for the act it
/// stands for in this window — `UNARCHIVE`, not `TRAY_ARROW_UP` — because that is what a reader
/// of the call site needs to know. The Phosphor name is in the comment so the glyph can be
/// looked up again at phosphoricons.com.
pub const ARCHIVE: &str = "\u{E00C}";
pub const BELL: &str = "\u{E0CE}";
/// `caret-down`
pub const NEXT: &str = "\u{E136}";
/// `caret-up`
pub const PREVIOUS: &str = "\u{E13C}";
pub const COMMAND: &str = "\u{E1C4}";
pub const FILE_PLUS: &str = "\u{E236}";
pub const FOLDER_PLUS: &str = "\u{E258}";
pub const GEAR: &str = "\u{E270}";
pub const KEYBOARD: &str = "\u{E2D8}";
pub const LAYOUT: &str = "\u{E6D6}";
pub const PLUS_SQUARE: &str = "\u{ED4A}";
pub const POWER: &str = "\u{E3DA}";
/// `tray-arrow-up`
pub const UNARCHIVE: &str = "\u{EE52}";
/// `x`
pub const CLOSE: &str = "\u{E4F6}";

/// The icon font itself, compiled in.
///
/// Compiled in rather than read at run time for the reason every other asset is: a window that
/// cannot find its icons draws boxes, and a missing file is a failure with no good moment to
/// discover it.
const FONT_BYTES: &[u8] = include_bytes!("../assets/fonts/Phosphor-Regular.ttf");

/// The name the icon font is registered under.
const FONT: &str = "phosphor-regular";

/// The font id an icon is drawn with, at `size` points.
///
/// A helper rather than `RichText::size`, so every call site asks for the icon face explicitly
/// and none of them can drift into whatever the surrounding text is set in.
pub fn font(size: f32) -> egui::FontId {
    egui::FontId::new(size, egui::FontFamily::Proportional)
}

/// Makes the icon glyphs available on `ctx`.
///
/// Cheap to call every frame: `add_font` looks the name up in the loaded definitions
/// and returns without work when it is already there.
pub fn install(ctx: &egui::Context) {
    ctx.add_font(egui::epaint::text::FontInsert::new(
        FONT,
        egui::FontData::from_static(FONT_BYTES),
        vec![egui::epaint::text::InsertFontFamily {
            family: egui::FontFamily::Proportional,
            // Lowest, deliberately. The icons occupy the private use area, so nothing
            // that is actually text can resolve to them, and a higher priority would
            // put an icon font in front of the face the body text is set in.
            //
            // A family of their own would be stronger — a second crate's icon font inserted at
            // the same priority wins these codepoints, which is exactly what happened when
            // `egui-elegance` was tried and turned every archive drawer into a plus sign. It is
            // not done here because `Context::add_font` takes effect at the *start of the next*
            // frame: a named family is unbound on the frame it is added, and asking for an
            // unbound family panics. Binding it would mean every surface registering its fonts
            // before its first frame. Worth doing the day a second icon font arrives, and not
            // before.
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
    let button = egui::Button::new(RichText::new(icon).font(font(15.0))).min_size(SIZE);
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
    let response = glyph_button(ui, at, icon, ROW_GLYPH, enabled, None);
    describe(&response, label);
    response.on_hover_text(tooltip)
}

/// A glyph drawn as a control that fills exactly the rectangle it is given.
///
/// The one place that knows how to do this, because getting it wrong looks like two different
/// bugs and both were reported. `egui`'s `Button` sizes itself from its content plus the style's
/// button padding, floored at the style's *interaction size* — 44 x 28 for Turn — and it takes
/// its alignment from the `Ui` it is added to. Added to a plain region at an 18 x 16 slot, it
/// therefore came out 32 x 28 with the glyph against the left edge: a rounded box overflowing a
/// pane header and clipped at the bottom, and a row of icons that looked out of line.
///
/// So the rectangle is not a suggestion. The layout is centred *and justified*, the padding is
/// zeroed — the box is already the size it should be, and padding would only shrink the glyph —
/// and the interaction floor is lowered to the slot.
///
/// `tint` is for a control whose colour carries meaning; `None` leaves it to the style, which is
/// what makes a disabled control look disabled.
pub fn glyph_button(
    ui: &mut Ui,
    at: egui::Rect,
    icon: &str,
    points: f32,
    enabled: bool,
    tint: Option<egui::Color32>,
) -> Response {
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(at)
            .layout(egui::Layout::centered_and_justified(
                egui::Direction::TopDown,
            )),
        |ui| {
            ui.spacing_mut().button_padding = Vec2::ZERO;
            ui.spacing_mut().interact_size = at.size();
            let mut text = RichText::new(icon).font(font(points));
            if let Some(tint) = tint {
                text = text.color(tint);
            }
            let button = egui::Button::new(text)
                .min_size(at.size())
                // Ink at rest, a box under the pointer. Three framed boxes on a row — or on a
                // pane header with three panes — compete with the words for a surface whose job
                // is to be scanned; the frame appears the moment the control is reached, so
                // nothing is hidden and nothing moves when it does.
                .frame_when_inactive(false);
            ui.add_enabled(enabled, button)
        },
    )
    .inner
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
            crate::frames::run(&ctx, |ui| install(ui.ctx()));
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
