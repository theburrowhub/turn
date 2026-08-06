//! Running one `egui` frame outside a window, for tests that need real font metrics.
//!
//! A handful of things can only be measured inside a frame — the advance of a glyph, the height
//! of a row, whether installing a font twice loaded it twice — and the tests that check them run
//! a frame against a bare [`egui::Context`] rather than opening a window.
//!
//! It needs a helper because a frame's output is not something a caller may drop. From `epaint`
//! 0.36 a `TexturesDelta` panics on drop while it still holds unapplied deltas, and it is right
//! to: a delta describes a texture the renderer has been *told* about, so silently dropping one
//! leaves the atlas and the GPU disagreeing about what exists, which shows up later as a glyph
//! drawn from the wrong pixels. In the running window eframe applies them. A test with no
//! renderer has to say that it is deliberately throwing them away, and this is the one place
//! that says it.

/// Runs one frame and hands back its output, with the texture deltas already discarded.
///
/// Discarding them is honest here and would not be anywhere else: there is no renderer to apply
/// them to, and the atlas is dropped with the context at the end of the test.
pub fn measure(context: &egui::Context, build: impl FnMut(&mut egui::Ui)) -> egui::FullOutput {
    let mut output = context.run_ui(egui::RawInput::default(), build);
    output.textures_delta.clear();
    output
}

/// Runs one frame for its side effects, discarding everything it produced.
pub fn run(context: &egui::Context, build: impl FnMut(&mut egui::Ui)) {
    let _ = measure(context, build);
}
