# Existing routes

Turn is a native single-window Rust application rather than a URL-routed web app. Its user-visible states are selected through `ViewState` in `crates/turn-gui/src/view.rs`:

- Main hierarchy + exact WorkSurface.
- New workspace overlay.
- New session overlay.
- Layout template editor overlay.
- Settings, command palette, node edit, context handoff, recovery, and confirmation overlays.

Navigation identity is a typed `HierarchyTarget`/node id, not a route string. Designs must preserve exact-node identity and must not invent browser-style routes or a canvas route.
