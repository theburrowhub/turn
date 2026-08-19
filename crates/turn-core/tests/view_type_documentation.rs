//! Documentation contract for every persisted Pane kind.
//!
//! The production enum owns wire spelling, labels and menu eligibility. These tests
//! make the two human-facing catalogues consume that authority instead of silently
//! maintaining a second vocabulary.

use std::collections::HashSet;

use turn_core::model::{NodeKind, PaneKind};

const VIEW_TYPES: &str = include_str!("../../../docs/VIEW_TYPES.md");
const PROTOCOL: &str = include_str!("../../../docs/PROTOCOL.md");
const README: &str = include_str!("../../../README.md");

fn operational_override(kind: PaneKind) -> bool {
    // Deliberately exhaustive: adding a variant requires a documentation decision
    // even if somebody forgets to add it to PaneKind::ALL.
    match kind {
        PaneKind::Terminal
        | PaneKind::Agent
        | PaneKind::Shell
        | PaneKind::Tui
        | PaneKind::Logs
        | PaneKind::TestOutput
        | PaneKind::Server
        | PaneKind::TmuxTerminal => true,
        PaneKind::EventLog
        | PaneKind::AgentTree
        | PaneKind::ProcessDetails
        | PaneKind::Preview
        | PaneKind::Placeholder => false,
    }
}

#[test]
fn every_kind_has_one_exact_section_and_matching_catalogue_rows() {
    let mut wires = HashSet::new();
    for kind in PaneKind::ALL {
        let encoded = serde_json::to_string(&kind).expect("PaneKind serialises");
        let wire = encoded.trim_matches('"');
        let status = if operational_override(kind) {
            "yes"
        } else {
            "no"
        };

        assert_eq!(
            kind.is_display_override(),
            operational_override(kind),
            "{} has conflicting renderer eligibility",
            kind.slug()
        );
        assert!(
            wires.insert(wire.to_owned()),
            "duplicate wire value: {wire}"
        );

        let section = format!("<a id=\"{}\"></a>\n## {}\n", kind.slug(), kind.label());
        assert_eq!(
            VIEW_TYPES.matches(&section).count(),
            1,
            "{} needs one adjacent anchor and exact heading",
            kind.slug()
        );
        let section_start = VIEW_TYPES.find(&section).unwrap() + section.len();
        let section_body = &VIEW_TYPES[section_start..];
        let section_end = section_body
            .find("\n<a id=\"")
            .unwrap_or(section_body.len());
        let section_body = &section_body[..section_end];
        for required in [
            "- **Status:**",
            "- **Automatic detection:**",
            "- **Data and renderer:**",
            "- **Input:**",
            "- **Launch and restore:**",
            "- **Fallback:**",
            "- **Truth boundary:**",
        ] {
            assert!(
                section_body.contains(required),
                "{} must document {required}",
                kind.slug()
            );
        }

        let catalogue_row = format!(
            "| [`{0}`](#{0}) | `{1}` | {2} | {3} |",
            kind.slug(),
            wire,
            kind.label(),
            status
        );
        assert_eq!(
            VIEW_TYPES.matches(&catalogue_row).count(),
            1,
            "{} has a missing or stale view-catalogue row",
            kind.slug()
        );

        let protocol_row = format!(
            "| `{}` | `{}` | {} | {} |",
            wire,
            kind.slug(),
            kind.label(),
            status
        );
        assert_eq!(
            PROTOCOL.matches(&protocol_row).count(),
            1,
            "{} has a missing or stale protocol row",
            kind.slug()
        );
    }
    assert_eq!(wires.len(), PaneKind::ALL.len());
}

#[test]
fn documented_automatic_detection_matches_the_exhaustive_node_mapping() {
    for (node, expected) in [
        (NodeKind::Agent, PaneKind::Agent),
        (NodeKind::Subagent, PaneKind::Agent),
        (NodeKind::Shell, PaneKind::Shell),
        (NodeKind::Terminal, PaneKind::Terminal),
        (NodeKind::Tui, PaneKind::Tui),
        (NodeKind::Server, PaneKind::Server),
        (NodeKind::Watcher, PaneKind::Terminal),
        (NodeKind::TestRunner, PaneKind::TestOutput),
        (NodeKind::Build, PaneKind::TestOutput),
        (NodeKind::Background, PaneKind::Terminal),
        (NodeKind::TmuxSession, PaneKind::TmuxTerminal),
        (NodeKind::TmuxPane, PaneKind::TmuxTerminal),
        (NodeKind::ExternalApp, PaneKind::ProcessDetails),
        (NodeKind::Unknown, PaneKind::Terminal),
    ] {
        assert_eq!(PaneKind::detected_for_node(node, true), expected);
        assert_eq!(
            PaneKind::detected_for_node(node, false),
            PaneKind::ProcessDetails,
            "a Node without an exact terminal must never borrow another Node's PTY"
        );
    }
    for claim in [
        "Claude Code (`claude`)",
        "Codex CLI (`codex`)",
        "Gemini CLI (`gemini`)",
        "OpenCode (`opencode`)",
        "Ctrl+Z returns",
        "Shift+Enter** sends line feed (`0x0a`)",
        "tiled or floating Process-details Pane",
    ] {
        assert!(
            VIEW_TYPES.contains(claim),
            "the canonical catalogue must document {claim}"
        );
    }
}

#[test]
fn the_catalogue_is_discoverable_and_names_the_compatibility_fields() {
    assert_eq!(
        README
            .matches("[Pane and view types](docs/VIEW_TYPES.md)")
            .count(),
        1,
        "README must link the canonical catalogue exactly once"
    );
    for field in [
        "`Pane.kind`",
        "`Pane.launch_kind`",
        "`Pane.kind_is_user_set`",
        "`Pane.detected_kind`",
    ] {
        assert!(
            VIEW_TYPES.contains(field),
            "view catalogue must explain {field}"
        );
    }
    for field in [
        "`kind: PaneKind`",
        "`launch_kind?: PaneKind`",
        "`kind_is_user_set: bool`",
        "`detected_kind?: PaneKind`",
        "`terminal_runtime_host`",
    ] {
        assert!(PROTOCOL.contains(field), "protocol must define {field}");
    }
}
