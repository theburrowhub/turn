# Existing page dependency tree

`TurnApp`
└── `View::show`
    ├── global toolbar
    ├── hierarchy navigator
    │   ├── workspace rows
    │   ├── session rows
    │   └── semantic process/agent/subagent rows
    ├── session context bar
    ├── `WorkSurface`
    │   ├── exact terminal surface, when the selected node owns one
    │   └── structured semantic-node surface, otherwise
    ├── contextual inspector when explicitly opened
    ├── bottom status bar
    └── one active overlay
        ├── session creator
        └── layout editor

This task extends the WorkSurface header with at-a-glance agent runtime/capacity facts and extends the existing creation overlays with semantic provider/profile controls. It does not add a page or navigation mode.
