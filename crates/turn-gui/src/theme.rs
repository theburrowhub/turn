//! Turn's visual language.
//!
//! Sober, dense and technical: the register of a tool people keep open all day,
//! not of something trying to be liked. No gradients, no rounded-everything, no
//! decorative colour.
//!
//! Two rules the palette exists to enforce:
//!
//! * **No state is signalled by colour alone.** Every state has a glyph and a
//!   word as well, so it survives a screenshot in greyscale, a colour-blind
//!   reader, and a screen reader.
//! * **Exactly one thing on screen is allowed to be loud.** `YOUR TURN` is the
//!   product's whole message; if three other things also shout, it stops working.

use egui::{Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle};

/// The cursor Turn draws when a pane exposes one.
///
/// Kept in the renderer rather than the settings catalogue: the catalogue validates the
/// stored word, while this type makes it impossible for painting code to invent a fourth
/// shape or silently fall back after the value has already been resolved.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CursorStyle {
    #[default]
    Block,
    Bar,
    Underline,
}

impl CursorStyle {
    fn from_setting(value: Option<&serde_json::Value>) -> Self {
        match value.and_then(serde_json::Value::as_str) {
            Some("bar") => Self::Bar,
            Some("underline") => Self::Underline,
            _ => Self::Block,
        }
    }
}

/// Appearance preferences resolved for the Session currently on screen.
///
/// The daemon owns Global → Workspace → Template → Session resolution and the Desk appends
/// this window's temporary layer. This reader consumes only those winning values: it is not
/// another precedence implementation. Malformed values fall back defensively, although a
/// conforming daemon has already validated them before they reach the window.
#[derive(Debug, Clone, PartialEq)]
pub struct AppearanceSettings {
    pub terminal_font_size: f32,
    pub ui_font_size: f32,
    pub zoom: f32,
    pub cursor: CursorStyle,
    pub cursor_blink: bool,
    pub ligatures: bool,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            terminal_font_size: 13.0,
            ui_font_size: 13.0,
            zoom: 1.0,
            cursor: CursorStyle::Block,
            cursor_blink: true,
            ligatures: false,
        }
    }
}

impl AppearanceSettings {
    pub fn from_view(settings: Option<&turn_proto::SettingsView>) -> Self {
        let mut appearance = Self::default();
        let Some(settings) = settings else {
            return appearance;
        };
        let value = |key: &str| settings.entry(key).map(|entry| &entry.resolution.value);
        if let Some(size) = value("appearance.font_size")
            .and_then(serde_json::Value::as_i64)
            .filter(|size| (6..=32).contains(size))
        {
            appearance.terminal_font_size = size as f32;
        }
        if let Some(size) = value("appearance.ui_font_size")
            .and_then(serde_json::Value::as_i64)
            .filter(|size| (8..=28).contains(size))
        {
            appearance.ui_font_size = size as f32;
        }
        if let Some(zoom) = value("appearance.zoom")
            .and_then(serde_json::Value::as_f64)
            .filter(|zoom| (0.5..=3.0).contains(zoom))
        {
            appearance.zoom = zoom as f32;
        }
        appearance.cursor = CursorStyle::from_setting(value("appearance.cursor"));
        appearance.cursor_blink = value("appearance.cursor_blink")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        appearance.ligatures = value("appearance.ligatures")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        if value("appearance.reduced_motion")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            appearance.cursor_blink = false;
        }
        appearance
    }
}

/// Colours and metrics, resolved once.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub background: Color32,
    pub panel: Color32,
    pub raised: Color32,
    pub border: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub text_faint: Color32,
    /// The one loud colour: something is blocked on you.
    pub attention: Color32,
    pub failure: Color32,
    pub running: Color32,
    pub done: Color32,
    /// Marks a value Turn inferred rather than was told.
    pub provisional: Color32,
    pub selection: Color32,
    pub cursor: Color32,
    pub mono: FontId,
    pub ui_font: FontId,
    pub cursor_style: CursorStyle,
    pub cursor_blink: bool,
    /// Joins a small, explicit set of programming operators visually while keeping their
    /// original cells, selection and copied text intact.
    pub ligatures: bool,
}

/// The glyph the terminal cell is measured from.
///
/// In a monospace face every glyph has the same advance, so any of them would answer the
/// question; `M` is the conventional choice and is in every face that could plausibly be
/// used for a terminal.
const CELL_REFERENCE_GLYPH: char = 'M';

/// Turns font metrics into the size of one terminal cell, in points.
///
/// Separate from the measurement so the arithmetic — the part that decides whether
/// columns line up — is testable without a window.
///
/// Two rules:
///
/// * **`None` rather than a guess.** A caller with no measurement must paint nothing: a
///   pane drawn at an invented cell size is the bug this whole module exists to remove,
///   and it is invisible until somebody compares Turn with a real terminal.
/// * **Whole physical pixels.** The advance of egui's bundled monospace at 13pt is
///   7.82666 points, so a grid built on the raw advance puts every other column boundary
///   in the middle of a pixel: box-drawing borders come out soft and doubled. Rounding
///   the cell to a whole pixel makes every column identical and every boundary a pixel
///   boundary, which is what makes a horizontal run one unbroken line. The remainder —
///   at most half a pixel per cell — is spent as slack inside the cell rather than
///   accumulated across the row.
pub fn cell_from_metrics(
    advance: f32,
    row_height: f32,
    pixels_per_point: f32,
) -> Option<egui::Vec2> {
    let usable = |value: f32| value.is_finite() && value > 0.0;
    if !usable(advance) || !usable(row_height) || !usable(pixels_per_point) {
        return None;
    }
    // At least one physical pixel: a cell of zero would divide by zero downstream, and a
    // cell of half a pixel cannot show a glyph.
    let snap = |points: f32| (points * pixels_per_point).round().max(1.0) / pixels_per_point;
    Some(egui::Vec2::new(snap(advance), snap(row_height)))
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            background: Color32::from_rgb(0x0d, 0x0f, 0x12),
            panel: Color32::from_rgb(0x12, 0x15, 0x19),
            raised: Color32::from_rgb(0x1a, 0x1e, 0x24),
            border: Color32::from_rgb(0x25, 0x2b, 0x33),
            text: Color32::from_rgb(0xd6, 0xdb, 0xe1),
            text_dim: Color32::from_rgb(0x8b, 0x94, 0xa0),
            text_faint: Color32::from_rgb(0x5a, 0x62, 0x6d),
            // Amber rather than red: red is for things that broke, and being
            // needed is not a fault.
            attention: Color32::from_rgb(0xe8, 0xa8, 0x3a),
            failure: Color32::from_rgb(0xe0, 0x5a, 0x5a),
            running: Color32::from_rgb(0x6a, 0x9e, 0xd8),
            done: Color32::from_rgb(0x6e, 0xb0, 0x7e),
            provisional: Color32::from_rgb(0x9a, 0x8c, 0xc4),
            selection: Color32::from_rgb(0x2a, 0x3a, 0x50),
            cursor: Color32::from_rgb(0xd6, 0xdb, 0xe1),
            mono: FontId::new(13.0, FontFamily::Monospace),
            ui_font: FontId::new(13.0, FontFamily::Proportional),
            cursor_style: CursorStyle::Block,
            cursor_blink: true,
            ligatures: false,
        }
    }

    /// Resolves the fixed palette with the appearance values currently in force.
    pub fn with_appearance(appearance: &AppearanceSettings) -> Self {
        Self {
            mono: FontId::new(appearance.terminal_font_size, FontFamily::Monospace),
            ui_font: FontId::new(appearance.ui_font_size, FontFamily::Proportional),
            cursor_style: appearance.cursor,
            cursor_blink: appearance.cursor_blink,
            ligatures: appearance.ligatures,
            ..Self::dark()
        }
    }

    /// The size of one terminal cell, in points, measured from the font in use.
    ///
    /// Derived, never declared. The cell was a literal `8.0 x 17.0` for as long as nobody
    /// compared Turn with a terminal: the real advance is 7.82666 and the real line height
    /// 15.125, so every column and every row accumulated error until box-drawing borders
    /// doubled and a program laid out for a width Turn never drew truncated its own file
    /// names. Reading it from the font also means it follows the font size, so changing
    /// that cannot silently break alignment again.
    ///
    /// Takes a `Ui` rather than a `Context` because font metrics exist only inside a pass,
    /// and holding a `Ui` is proof of being in one. `None` when the family has no
    /// measurable glyph — a caller must then paint nothing rather than guess.
    pub fn cell_size(&self, ui: &egui::Ui) -> Option<egui::Vec2> {
        let ctx = ui.ctx();
        let (advance, row_height) = ctx.fonts_mut(|fonts| {
            (
                fonts.glyph_width(&self.mono, CELL_REFERENCE_GLYPH),
                fonts.row_height(&self.mono),
            )
        });
        cell_from_metrics(advance, row_height, ctx.pixels_per_point())
    }

    /// Applies the theme to a context.
    pub fn install(&self, ctx: &egui::Context) {
        // The icon family is bound here, because every surface that draws anything installs
        // the theme and a named family panics rather than falling back: an icon asked for
        // before its font is bound is a mistake worth failing on, and this is the one call that
        // makes it impossible.
        crate::icons::install(ctx);

        let theme = egui::Theme::Dark;
        let mut style = (*ctx.style_of(theme)).clone();
        let radius = CornerRadius::same(6);
        let input = Color32::from_rgb(0x0a, 0x0d, 0x11);
        let inactive = Color32::from_rgb(0x17, 0x1b, 0x21);
        let hovered = Color32::from_rgb(0x20, 0x27, 0x30);
        let active = Color32::from_rgb(0x18, 0x22, 0x2e);
        let open = Color32::from_rgb(0x1c, 0x21, 0x29);
        let inactive_border = Color32::from_rgb(0x34, 0x3b, 0x46);
        let hovered_border = Color32::from_rgb(0x4a, 0x56, 0x65);
        let active_border = Color32::from_rgb(0x74, 0x86, 0x9c);
        let open_border = Color32::from_rgb(0x56, 0x64, 0x77);

        style.visuals.dark_mode = true;
        style.visuals.panel_fill = self.background;
        style.visuals.window_fill = self.panel;
        style.visuals.window_stroke = Stroke::new(1.0, self.border);
        style.visuals.extreme_bg_color = input;
        style.visuals.text_edit_bg_color = Some(input);
        style.visuals.faint_bg_color = self.raised;
        style.visuals.code_bg_color = input;
        // Let each interaction state own its foreground. A global override made a
        // focused field and a disabled button read exactly like an idle control.
        style.visuals.override_text_color = None;
        style.visuals.weak_text_color = Some(self.text_dim);

        let widgets = &mut style.visuals.widgets;
        widgets.noninteractive.bg_fill = self.panel;
        widgets.noninteractive.weak_bg_fill = self.panel;
        widgets.noninteractive.bg_stroke = Stroke::new(1.0, self.border);
        widgets.noninteractive.fg_stroke = Stroke::new(1.0, self.text);
        widgets.noninteractive.corner_radius = radius;

        widgets.inactive.bg_fill = inactive;
        widgets.inactive.weak_bg_fill = inactive;
        widgets.inactive.bg_stroke = Stroke::new(1.0, inactive_border);
        widgets.inactive.fg_stroke = Stroke::new(1.0, self.text);
        widgets.inactive.corner_radius = radius;

        widgets.hovered.bg_fill = hovered;
        widgets.hovered.weak_bg_fill = hovered;
        widgets.hovered.bg_stroke = Stroke::new(1.0, hovered_border);
        widgets.hovered.fg_stroke = Stroke::new(1.0, Color32::from_rgb(0xee, 0xf2, 0xf6));
        widgets.hovered.corner_radius = radius;

        widgets.active.bg_fill = active;
        widgets.active.weak_bg_fill = active;
        widgets.active.bg_stroke = Stroke::new(1.0, active_border);
        widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);
        widgets.active.corner_radius = radius;

        widgets.open.bg_fill = open;
        widgets.open.weak_bg_fill = open;
        widgets.open.bg_stroke = Stroke::new(1.0, open_border);
        widgets.open.fg_stroke = Stroke::new(1.0, Color32::from_rgb(0xe4, 0xe9, 0xef));
        widgets.open.corner_radius = radius;

        style.visuals.selection.bg_fill = self.selection;
        style.visuals.selection.stroke = Stroke::new(1.0, self.text);
        style.visuals.window_corner_radius = radius;
        style.visuals.menu_corner_radius = radius;

        // macOS-sized targets without turning a dense terminal workspace into a touch UI.
        style.spacing.interact_size = egui::vec2(44.0, 28.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.menu_margin = egui::Margin::same(8);
        style.spacing.icon_width = 16.0;
        style.spacing.icon_width_inner = 10.0;
        style.spacing.icon_spacing = 6.0;
        style.spacing.indent = 22.0;
        style.spacing.window_margin = egui::Margin::same(0);
        style
            .text_styles
            .insert(TextStyle::Body, self.ui_font.clone());
        style
            .text_styles
            .insert(TextStyle::Monospace, self.mono.clone());
        style
            .text_styles
            .insert(TextStyle::Button, self.ui_font.clone());
        ctx.set_style_of(theme, style);
        ctx.set_theme(theme);
    }

    /// The colour and glyph for a state.
    ///
    /// Returned together so a caller cannot draw one without the other — which is
    /// how "never rely on colour alone" stops being a good intention.
    ///
    /// The glyphs are chosen from what the bundled fonts actually draw, checked by
    /// rendering them. `U+2713 ✓` is *not* in them and comes out as a missing-glyph
    /// box — which would signal "done" as a colour and an empty square, so the
    /// heavier `U+2714 ✔` is used instead. A state whose glyph does not render is the
    /// same failure as a state with no glyph at all.
    pub fn state_marker(&self, state: turn_core::state::DisplayState) -> (Color32, &'static str) {
        use turn_core::state::DisplayState as S;
        match state {
            S::WaitingForUser | S::NeedsPermission | S::AskingQuestion => (self.attention, "!"),
            S::Failed => (self.failure, "×"),
            S::Running | S::Starting => (self.running, "▸"),
            S::CompletedTurn | S::CompletedTask => (self.done, "✔"),
            S::Stopped => (self.text_faint, "▪"),
            S::Idle => (self.text_dim, "·"),
            S::Unknown => (self.provisional, "?"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use turn_core::settings::{Catalogue, Resolution, Scope};
    use turn_core::state::DisplayState;

    fn appearance_view(values: &[(&str, Value)]) -> turn_proto::SettingsView {
        let catalogue = Catalogue::built_in();
        let entries = values
            .iter()
            .map(|(key, value)| {
                let definition = catalogue.get(key).expect("an appearance definition");
                turn_proto::SettingsEntry {
                    resolution: Resolution {
                        key: (*key).to_string(),
                        value: value.clone(),
                        origin: Some(Scope::Global),
                        shadowed: Vec::new(),
                        sensitivity: definition.sensitivity,
                    },
                    default_value: definition.default.clone(),
                    area: definition.area,
                    area_title: definition.area.title().to_string(),
                    title: definition.title.to_string(),
                    description: definition.description.to_string(),
                    accepts: definition.kind.describe(),
                    control: turn_proto::SettingsControl::from_kind(&definition.kind),
                    settable_at: definition.scopes.to_vec(),
                    hidden: false,
                    known: true,
                }
            })
            .collect();
        turn_proto::SettingsView {
            session_id: None,
            levels: vec![turn_proto::SettingsLevel::global()],
            entries,
        }
    }

    #[test]
    fn every_appearance_control_changes_the_values_the_renderer_reads() {
        let view = appearance_view(&[
            ("appearance.font_size", json!(21)),
            ("appearance.ui_font_size", json!(17)),
            ("appearance.zoom", json!(1.5)),
            ("appearance.cursor", json!("underline")),
            ("appearance.cursor_blink", json!(false)),
            ("appearance.ligatures", json!(true)),
        ]);
        let appearance = AppearanceSettings::from_view(Some(&view));
        assert_eq!(appearance.terminal_font_size, 21.0);
        assert_eq!(appearance.ui_font_size, 17.0);
        assert_eq!(appearance.zoom, 1.5);
        assert_eq!(appearance.cursor, CursorStyle::Underline);
        assert!(!appearance.cursor_blink);
        assert!(appearance.ligatures);

        let theme = Theme::with_appearance(&appearance);
        assert_eq!(theme.mono.size, 21.0);
        assert_eq!(theme.ui_font.size, 17.0);
        assert_eq!(theme.cursor_style, CursorStyle::Underline);
        assert!(!theme.cursor_blink);
        assert!(theme.ligatures);
    }

    #[test]
    fn reduced_motion_overrides_cursor_blink_and_bad_input_falls_back_safely() {
        let view = appearance_view(&[
            ("appearance.font_size", json!(400)),
            ("appearance.zoom", json!("huge")),
            ("appearance.cursor", json!("unknown")),
            ("appearance.cursor_blink", json!(true)),
            ("appearance.reduced_motion", json!(true)),
        ]);
        let appearance = AppearanceSettings::from_view(Some(&view));
        assert_eq!(appearance.terminal_font_size, 13.0);
        assert_eq!(appearance.zoom, 1.0);
        assert_eq!(appearance.cursor, CursorStyle::Block);
        assert!(!appearance.cursor_blink);
    }

    /// The rule is structural: every state must have a glyph, so nothing is ever
    /// distinguishable by colour alone.
    #[test]
    fn every_state_has_a_glyph_as_well_as_a_colour() {
        let theme = Theme::dark();
        let all = [
            DisplayState::Starting,
            DisplayState::Running,
            DisplayState::WaitingForUser,
            DisplayState::NeedsPermission,
            DisplayState::AskingQuestion,
            DisplayState::CompletedTurn,
            DisplayState::CompletedTask,
            DisplayState::Failed,
            DisplayState::Stopped,
            DisplayState::Idle,
            DisplayState::Unknown,
        ];
        for state in all {
            let (_, glyph) = theme.state_marker(state);
            assert!(!glyph.is_empty(), "{state:?} has no glyph");
            // And the state's own word is always available next to it.
            assert!(!state.label().is_empty());
        }
    }

    /// Only the states that genuinely block the user get the loud colour.
    #[test]
    fn the_attention_colour_is_reserved_for_states_that_block_the_user() {
        let theme = Theme::dark();
        for state in [
            DisplayState::WaitingForUser,
            DisplayState::NeedsPermission,
            DisplayState::AskingQuestion,
        ] {
            assert_eq!(theme.state_marker(state).0, theme.attention, "{state:?}");
            assert!(state.demands_user());
        }
        for state in [
            DisplayState::Running,
            DisplayState::CompletedTurn,
            DisplayState::Idle,
            DisplayState::Stopped,
        ] {
            assert_ne!(
                theme.state_marker(state).0,
                theme.attention,
                "{state:?} must not shout"
            );
        }
    }

    /// A failure is red; being needed is amber. Conflating them would make the
    /// product's main signal look like an error.
    #[test]
    fn being_needed_does_not_look_like_a_failure() {
        let theme = Theme::dark();
        assert_ne!(
            theme.state_marker(DisplayState::NeedsPermission).0,
            theme.state_marker(DisplayState::Failed).0
        );
    }

    /// The measurement, against the fonts the window actually ships with. A cell that
    /// disagrees with the font is the defect: `8.0 x 17.0` was neither the advance nor the
    /// line height of anything.
    #[test]
    fn the_cell_is_measured_from_the_font_rather_than_declared() {
        let context = egui::Context::default();
        let theme = Theme::dark();
        let mut measured = None;
        crate::frames::run(&context, |ui| {
            measured = theme.cell_size(ui);
        });
        let cell = measured.expect("the bundled monospace face can be measured");

        let (advance, row_height) = context.fonts_mut(|fonts| {
            (
                fonts.glyph_width(&theme.mono, 'M'),
                fonts.row_height(&theme.mono),
            )
        });
        assert!(
            (cell.x - advance).abs() <= 0.5,
            "the cell width {} is not the font's advance {advance}",
            cell.x
        );
        assert!(
            (cell.y - row_height).abs() <= 0.5,
            "the cell height {} is not the font's line height {row_height}",
            cell.y
        );
        assert_ne!(
            cell,
            egui::vec2(8.0, 17.0),
            "17.0 was the invented line height; the font's is nowhere near it"
        );
    }

    /// A user who changes the font size must not silently lose alignment, which is what a
    /// constant cell would do.
    #[test]
    fn the_cell_follows_the_font_size() {
        let context = egui::Context::default();
        let small = Theme::dark();
        let large = Theme {
            mono: FontId::new(26.0, FontFamily::Monospace),
            ..Theme::dark()
        };
        let mut cells = (None, None);
        crate::frames::run(&context, |ui| {
            cells = (small.cell_size(ui), large.cell_size(ui));
        });
        let (small, large) = (
            cells.0.expect("a measured cell"),
            cells.1.expect("a measured cell"),
        );
        assert!(
            large.x > small.x * 1.8 && large.y > small.y * 1.8,
            "doubling the font size must roughly double the cell: {small:?} -> {large:?}"
        );
    }

    /// The one case where painting has to be skipped entirely: there is no font to measure,
    /// so any cell size would be invented.
    #[test]
    fn a_font_that_cannot_be_measured_yields_no_cell_at_all() {
        assert_eq!(cell_from_metrics(0.0, 15.125, 1.0), None);
        assert_eq!(cell_from_metrics(7.82666, 0.0, 1.0), None);
        assert_eq!(cell_from_metrics(f32::NAN, 15.125, 1.0), None);
        assert_eq!(cell_from_metrics(7.82666, 15.125, 0.0), None);

        // And through the real path: a context with no faces at all measures nothing.
        let theme = Theme::dark();
        let mut measured = Some(egui::Vec2::ZERO);
        egui::__run_test_ui(|ui| measured = theme.cell_size(ui));
        assert_eq!(
            measured, None,
            "with no font faces the pane must paint nothing rather than pick a size"
        );
    }

    /// Every column boundary has to be a whole pixel, or half of them land mid-pixel and
    /// the borders they carry come out soft and doubled.
    #[test]
    fn the_cell_lands_on_whole_physical_pixels_at_every_scale() {
        for pixels_per_point in [1.0, 1.25, 1.5, 2.0, 3.0] {
            let cell = cell_from_metrics(7.82666, 15.125, pixels_per_point)
                .expect("a measurable font at any scale");
            for extent in [cell.x, cell.y] {
                let pixels = extent * pixels_per_point;
                assert!(
                    (pixels - pixels.round()).abs() < 1e-4,
                    "at {pixels_per_point}x the cell extent {extent} is {pixels} pixels"
                );
                assert!(pixels >= 1.0, "a cell must be at least one pixel");
            }
        }
    }

    /// A cell must never be rounded away to nothing, however small the font.
    #[test]
    fn an_absurdly_small_font_still_leaves_one_pixel_per_cell() {
        let cell = cell_from_metrics(0.01, 0.02, 1.0).expect("a measurable font");
        assert_eq!(cell, egui::vec2(1.0, 1.0));
    }

    #[test]
    fn installed_controls_have_native_sized_geometry_and_a_legible_input_surface() {
        let theme = Theme::dark();
        let context = egui::Context::default();
        theme.install(&context);
        let style = context.style_of(egui::Theme::Dark);

        assert_eq!(style.spacing.interact_size, egui::vec2(44.0, 28.0));
        assert_eq!(style.spacing.button_padding, egui::vec2(10.0, 5.0));
        assert_eq!(style.visuals.window_corner_radius, CornerRadius::same(6));
        assert_eq!(style.visuals.menu_corner_radius, CornerRadius::same(6));
        assert_eq!(
            style.visuals.text_edit_bg_color(),
            Color32::from_rgb(0x0a, 0x0d, 0x11)
        );
        assert_ne!(style.visuals.text_edit_bg_color(), theme.text);
    }

    #[test]
    fn every_interaction_state_has_its_own_fill_and_outline() {
        let theme = Theme::dark();
        let context = egui::Context::default();
        theme.install(&context);
        let style = context.style_of(egui::Theme::Dark);
        let widgets = &style.visuals.widgets;
        let states = [
            widgets.inactive,
            widgets.hovered,
            widgets.active,
            widgets.open,
        ];

        for state in states {
            assert_eq!(state.corner_radius, CornerRadius::same(6));
        }
        for left in 0..states.len() {
            for right in left + 1..states.len() {
                assert_ne!(
                    states[left].weak_bg_fill, states[right].weak_bg_fill,
                    "interaction states {left} and {right} share a button fill"
                );
                assert_ne!(
                    states[left].bg_stroke, states[right].bg_stroke,
                    "interaction states {left} and {right} share an outline"
                );
            }
        }
    }
}
