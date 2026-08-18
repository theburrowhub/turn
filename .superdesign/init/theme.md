# Existing theme

The canonical palette is `Theme::dark()` in `crates/turn-gui/src/theme.rs`.

- Background `#0d0f12`; panel `#121519`; raised `#1a1e24`; border `#252b33`.
- Primary text `#d6dbe1`; dim `#8b94a0`; faint `#5a626d`.
- Attention `#e8a83a`; failure `#e05a5a`; running `#6a9ed8`; done `#6eb07e`; provisional `#9a8cc4`; selection `#2a3a50`.
- Typography is compact at roughly 13px. Terminal and operational values are monospace; controls may use the UI face.
- Corner radius is 6px, borders are one pixel, animation is deliberately disabled, and decoration is sparse.

New controls should read as native Turn: dense, quiet, high-contrast, semantically coloured, and legible at large process counts.
