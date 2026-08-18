# Existing components

- `View::hierarchy_navigator` in `crates/turn-gui/src/view.rs` renders the only navigation model: workspace → session → semantic process/agent descendants. Rows carry attention state, lifecycle, exact-node selection, and row actions.
- `WorkSurface` in `crates/turn-gui/src/work_surface.rs` renders the selected semantic node on the right. Terminal-capable nodes bind their exact pane; semantic-only nodes get structured activity/details and never substitute a parent terminal.
- `View::session_context_bar` is the compact breadcrumb and session action strip above the WorkSurface.
- `View::session_creator_overlay` selects a workspace and reusable layout template.
- `View::layout_editor_overlay` edits split geometry and a per-cell command. Today agent cells are configured as raw executable and argv fields.
- Inspector helpers (`inspector_section`, `inspector_value`, `inspector_optional`) provide dense labelled metadata rows.
- All components are immediate-mode Rust/egui. There is no HTML/CSS component library.

Do not introduce a node canvas, graph editor, alternate navigator, or second hierarchy. New agent controls belong in the existing tree rows, exact-node WorkSurface, session creator, and layout editor.
