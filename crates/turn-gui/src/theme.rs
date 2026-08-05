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

/// Colours and metrics, resolved once.
#[derive(Debug, Clone)]
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
    pub cell_size: egui::Vec2,
    pub mono: FontId,
    pub ui_font: FontId,
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
            cell_size: egui::vec2(8.0, 17.0),
            mono: FontId::new(13.0, FontFamily::Monospace),
            ui_font: FontId::new(13.0, FontFamily::Proportional),
        }
    }

    /// Applies the theme to a context.
    pub fn install(&self, ctx: &egui::Context) {
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
    use turn_core::state::DisplayState;

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
