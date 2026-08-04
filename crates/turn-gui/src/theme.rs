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

use egui::{Color32, FontFamily, FontId, Stroke, TextStyle};

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
        style.visuals.dark_mode = true;
        style.visuals.panel_fill = self.background;
        style.visuals.window_fill = self.panel;
        style.visuals.extreme_bg_color = self.background;
        style.visuals.override_text_color = Some(self.text);
        style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, self.border);
        style.visuals.widgets.inactive.bg_fill = self.raised;
        style.visuals.widgets.hovered.bg_fill = self.selection;
        style.visuals.selection.bg_fill = self.selection;
        // Square corners: this is an instrument panel, not a card layout.
        style.visuals.window_corner_radius = egui::CornerRadius::ZERO;
        style.visuals.menu_corner_radius = egui::CornerRadius::ZERO;
        style.spacing.item_spacing = egui::vec2(6.0, 4.0);
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
}
