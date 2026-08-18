# Turn design system

Turn is a dense desktop operations cockpit for supervising many agentic and terminal processes. The operator's attention, not ornamental hierarchy, is the scarce resource.

## Geometry

- The left tree is the only hierarchy and navigation control plane.
- The right side always represents the exact selected node. A child never opens its parent terminal as a substitute.
- Put important agent facts above the fold in the WorkSurface header; keep exhaustive metadata below or behind disclosure.
- Put creation acceleration inside the existing New Session and Layout Editor sheets.
- Keep the bottom status bar for transient operational feedback.

## Visual language

- Use `#0d0f12` background, `#121519` panels, `#1a1e24` raised controls, and `#252b33` borders.
- Use `#d6dbe1` primary text, `#8b94a0` secondary text, and `#5a626d` tertiary text.
- Use amber `#e8a83a` only for operator attention, red `#e05a5a` for failure/danger, blue `#6a9ed8` for active/running, green `#6eb07e` for done/healthy, and violet `#9a8cc4` for inferred/provisional facts.
- Prefer 13px compact typography, monospace for commands/models/measurements, 6px radii, 1px borders, and no decorative animation.

## Interaction principles

- Optimize the common path to one semantic choice and one confirmation.
- Default to Safe. Autonomous mode must be explicit, visibly describe its permission effect, and resolve exact flags per provider.
- Never make the operator remember CLI flags. Preserve Custom argv for experts under an advanced disclosure.
- Never claim capacity data that the provider did not expose. Show source, freshness, and “Unavailable” honestly.
- `Shift+Enter` inserts a newline in agent composers; plain `Enter` submits.
- Controls remain usable by keyboard, with clear focus and accessibility labels.
