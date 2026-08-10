//! Links, end to end: a real escape stream into the daemon's terminal buffer, out as a grid,
//! and found by the window's own scanner.
//!
//! Every other test of this feature exercises one side of the socket. This one exercises the
//! whole path, because the joins are where it can silently stop working: `vt100` does not
//! implement OSC 8, so the capture lives in `turn_pty`; the conversion to cells lives in
//! `turn_proto`; and the scanning, the allow-list and the confirmation live in `turn_gui`. A
//! hand-built grid would prove the scanner works and nothing about whether a hyperlink a
//! process printed ever reaches it.

use turn_gui::terminal::links::{open, LinkMap, LinkOrigin, LinkTarget, OpenError, PathResolver};
use turn_proto::cells::Grid;
use turn_pty::{ScreenSize, TerminalBuffer};

/// Feeds bytes to a terminal buffer and converts what came out **exactly as the daemon does**.
///
/// This call is the contract between the two crates: if it drifts, hyperlinks stop reaching
/// the client and every unit test on either side keeps passing.
fn screen_of(bytes: &[u8]) -> Grid {
    let mut buffer = TerminalBuffer::new(ScreenSize::new(8, 60));
    buffer.write(bytes);
    turn_proto::from_screen_with_links(
        buffer.screen(),
        buffer
            .screen_links()
            .iter()
            .map(|(row, from, to, uri)| (*row, *from, *to, uri.as_ref())),
    )
}

/// A resolver that finds nothing, because these tests are about URLs.
struct NoPaths;

impl PathResolver for NoPaths {
    fn resolve(&mut self, _candidate: &str) -> Option<std::path::PathBuf> {
        None
    }
}

/// The whole feature in one test: `gh pr view --web` prints this, and the user has to be able
/// to click it and to see where it goes.
#[test]
fn an_osc_8_hyperlink_printed_by_a_process_becomes_a_link_the_window_can_open() {
    let grid = screen_of(
        b"opened \x1b]8;;https://github.com/o/r/pull/42\x1b\\the PR\x1b]8;;\x1b\\ for review\r\n",
    );

    assert_eq!(
        grid.row_text(0),
        "opened the PR for review",
        "the escape sequences must not be drawn"
    );
    let map = LinkMap::find(&grid, &mut NoPaths);
    assert_eq!(map.len(), 1, "got {:?}", map.links());
    let link = &map.links()[0];
    assert_eq!(link.origin, LinkOrigin::Declared);
    assert_eq!(
        link.target,
        LinkTarget::Url("https://github.com/o/r/pull/42".into())
    );
    assert_eq!(link.text, "the PR");
    assert!(
        !link.request().needs_confirmation(),
        "a label that names no other host is not a phishing shape"
    );
    // The cells the user hovers are the ones the label occupies, and no others.
    let start = "opened ".chars().count() as u16;
    assert!(map.at(0, start).is_some());
    assert!(map.at(0, start + 5).is_some());
    assert_eq!(map.at(0, start + 6), None, "`for` is not part of the link");
    assert_eq!(map.at(0, 0), None);
}

/// The security case, all the way through: the process chose both the text and the
/// destination, and they disagree.
#[test]
fn a_hyperlink_whose_label_names_another_host_arrives_needing_confirmation() {
    let grid = screen_of(
        b"\x1b]8;;https://evil.example/steal\x1b\\https://github.com/o/r\x1b]8;;\x1b\\\r\n",
    );
    let map = LinkMap::find(&grid, &mut NoPaths);
    let request = map.links().first().expect("a link").request();

    assert_eq!(request.display, "https://evil.example/steal");
    assert!(
        request.needs_confirmation(),
        "the label reads as github.com and the link does not go there"
    );
    let warning = request.warning.expect("a warning").describe();
    assert!(warning.contains("github.com"), "got {warning}");
    assert!(warning.contains("evil.example"), "got {warning}");
}

/// A process must not be able to choose what the user's browser executes, and the refusal has
/// to survive the whole journey rather than living in one of the three crates.
#[test]
fn a_hyperlink_pointing_at_a_scheme_that_executes_is_refused_the_whole_way_through() {
    for hostile in [
        "javascript:alert(document.cookie)",
        "data:text/html;base64,PHNjcmlwdD4=",
        "vbscript:msgbox(1)",
    ] {
        let stream = format!("\x1b]8;;{hostile}\x1b\\click here\x1b]8;;\x1b\\\r\n");
        let grid = screen_of(stream.as_bytes());
        // It reached the grid, because the protocol carries what the program wrote.
        assert_eq!(
            grid.link_at(0, 2).map(|link| link.uri.as_str()),
            Some(hostile),
            "the URI must travel; the refusal is not a transport decision"
        );
        // And it is not offered to the user as a link at all.
        assert!(
            LinkMap::find(&grid, &mut NoPaths).is_empty(),
            "{hostile} was offered as a link"
        );
        // And it is still refused at the point of no return, by a caller that built the
        // target itself.
        assert!(matches!(
            open(&LinkTarget::Url(hostile.to_string())),
            Err(OpenError::RefusedScheme(_) | OpenError::Malformed(_))
        ));
        assert_eq!(grid.row_text(0), "click here");
    }
}

/// A URL a program merely printed, hard-wrapped by the terminal. One link, and the target is
/// the whole URL rather than the fragment on either row.
#[test]
fn a_plain_url_the_terminal_broke_at_the_margin_is_one_link_with_the_whole_target() {
    let grid = screen_of(
        b"see https://example.com/a/rather/long/path?query=value&more=1#fragment for details\r\n",
    );
    let map = LinkMap::find(&grid, &mut NoPaths);
    assert_eq!(map.len(), 1, "got {:?}", map.links());
    let link = &map.links()[0];
    assert_eq!(
        link.target,
        LinkTarget::Url(
            "https://example.com/a/rather/long/path?query=value&more=1#fragment".into()
        )
    );
    assert_eq!(link.origin, LinkOrigin::Detected);
    assert_eq!(
        link.spans.len(),
        2,
        "60 columns cannot hold it, so it crosses a row: {:?}",
        link.spans
    );
    assert!(grid.row_wrapped(0), "the emulator says it wrapped");
    // Hovering either half finds the same link.
    assert_eq!(map.at(0, 10).map(|l| &l.target), Some(&link.target));
    assert_eq!(map.at(1, 2).map(|l| &l.target), Some(&link.target));
}

/// A file a compiler named, resolved against a directory that really exists. This is the case
/// that makes a test failure actionable.
#[test]
fn a_compiler_error_naming_a_file_that_exists_becomes_a_link_to_it() {
    let grid = screen_of(b"error[E0308]: mismatched types\r\n  --> Cargo.toml:3:1\r\n");
    let mut paths = turn_gui::terminal::links::FsPaths::default();
    paths.set_cwd(Some(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))));
    paths.begin_scan(0);

    let map = LinkMap::find(&grid, &mut paths);
    let link = map
        .links()
        .iter()
        .find(|link| matches!(link.target, LinkTarget::File { .. }))
        .expect("the manifest of this crate is really there");
    match &link.target {
        LinkTarget::File { path, line, column } => {
            assert!(path.ends_with("Cargo.toml"), "got {path:?}");
            assert_eq!((*line, *column), (Some(3), Some(1)));
        }
        other => panic!("expected a file, got {other:?}"),
    }
    assert!(
        link.target.display().ends_with("Cargo.toml:3:1"),
        "the hover has to say which line will be opened: {}",
        link.target.display()
    );

    // A path in the same shape that is not there is not offered.
    let missing = screen_of(b"  --> src/does-not-exist.rs:9:2\r\n");
    let mut paths = turn_gui::terminal::links::FsPaths::default();
    paths.set_cwd(Some(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))));
    paths.begin_scan(0);
    assert!(
        LinkMap::find(&missing, &mut paths).is_empty(),
        "offering to open something that is not there is worse than offering nothing"
    );
}

/// The links have to survive the socket, because the client renders a decoded grid and never
/// the daemon's own.
#[test]
fn a_hyperlink_survives_the_journey_across_the_protocol() {
    let daemon =
        screen_of(b"\x1b]8;;ssh://git@example.com/o/r.git\x1b\\clone it\x1b]8;;\x1b\\\r\n");
    let wire = serde_json::to_string(&daemon).expect("a grid serialises");
    let client: Grid = serde_json::from_str(&wire).expect("and reads back");
    assert_eq!(client, daemon);

    let map = LinkMap::find(&client, &mut NoPaths);
    assert_eq!(
        map.links().first().map(|link| link.target.display()),
        Some("ssh://git@example.com/o/r.git".to_string())
    );
    assert!(
        wire.contains("ssh://git@example.com/o/r.git"),
        "the URI is on the wire once, as a span rather than per cell: {wire}"
    );
}
