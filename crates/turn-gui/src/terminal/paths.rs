//! Paths dropped onto a pane, turned into words a shell will read the way the user meant.
//!
//! Dragging a file into a terminal is a request to *name* it, not to run anything. So what
//! this produces is text at the prompt and nothing more: the user reads it, and presses
//! Enter themselves if that is what they wanted. Nothing here appends a newline.
//!
//! ## Why every path is quoted, always
//!
//! A path arrives from the filesystem and is inserted into a shell's input, which means it
//! is about to be subject to word splitting, `$` expansion, globbing and `~` expansion. A
//! space is the obvious case and the least dangerous one — the user sees two words and
//! fixes it. `$(...)`, backticks and `*` are the ones that would go wrong silently, on
//! somebody else's filenames, in a directory they did not create.
//!
//! Single quotes suspend all of it, in every shell Turn claims to support, and the only
//! character they cannot contain is a single quote itself — closed, escaped, reopened, the
//! usual `'\''`. Quoted unconditionally rather than when it looks necessary: "looks
//! necessary" is where this class of bug lives, and two bytes is not a budget.
//!
//! ## Why a newline is refused rather than escaped
//!
//! A filename may contain one. Pasted into a prompt it is a carriage return, which is the
//! user pressing Enter — so `evil\nrm -rf ~` as a filename would submit the first line and
//! then run the second, and quoting does not help because the quote is still open when the
//! shell takes the line. The escapes that would help (`$'\n'`) are not portable to fish,
//! which is in this feature's acceptance matrix.
//!
//! So it is refused, and said out loud. A refusal the user can read and work around beats
//! an insertion that is correct in bash and executes something in fish.

use std::path::Path;

/// The text to insert at the prompt, and what was left out of it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Dropped {
    /// Space-separated quoted paths, ready to be pasted. `None` when nothing survived.
    pub text: Option<String>,
    /// Names that could not be turned into a safe word, in the order they were dropped.
    pub refused: Vec<String>,
}

impl Dropped {
    /// The sentence to show when something was left out, if anything was.
    ///
    /// Names what happened to *this* drop rather than explaining the rule in the abstract:
    /// the user is looking at a prompt that has fewer paths in it than they dropped, and the
    /// question they have is which ones and why.
    pub fn refusal(&self) -> Option<String> {
        let first = self.refused.first()?;
        Some(if self.refused.len() == 1 {
            format!(
                "{first} was not inserted: a newline in a filename cannot be typed at a prompt \
                 without submitting the line"
            )
        } else {
            format!(
                "{} paths were not inserted, including {first}: a newline in a filename cannot \
                 be typed at a prompt without submitting the line",
                self.refused.len()
            )
        })
    }
}

/// Turns dropped paths into prompt text.
pub fn dropped(paths: &[&Path]) -> Dropped {
    let mut words = Vec::new();
    let mut refused = Vec::new();
    for path in paths {
        let raw = path.to_string_lossy();
        match quote(&raw) {
            Some(word) => words.push(word),
            // Lossy is fine for a message nobody parses, and the alternative is refusing
            // to say which file was refused.
            None => refused.push(raw.replace(['\n', '\r'], "⏎")),
        }
    }
    Dropped {
        text: (!words.is_empty()).then(|| words.join(" ")),
        refused,
    }
}

/// Wraps a path so a shell reads it as exactly one literal word.
///
/// `None` for a path a prompt cannot hold: see the module documentation on newlines.
pub fn quote(path: &str) -> Option<String> {
    if path.contains('\n') || path.contains('\r') {
        return None;
    }
    let mut out = String::with_capacity(path.len() + 2);
    out.push('\'');
    for c in path.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn text_for(paths: &[&str]) -> Option<String> {
        let owned: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
        let borrowed: Vec<&Path> = owned.iter().map(PathBuf::as_path).collect();
        dropped(&borrowed).text
    }

    /// The plain case is still quoted. An unquoted path that happens to work today breaks
    /// the first time somebody drops it from a directory with a space in it.
    #[test]
    fn an_ordinary_path_is_quoted_anyway() {
        assert_eq!(
            quote("/repo/src/main.rs").as_deref(),
            Some("'/repo/src/main.rs'")
        );
    }

    /// The reported case, and the one in the acceptance matrix.
    #[test]
    fn a_path_with_spaces_arrives_as_one_word() {
        assert_eq!(
            text_for(&["/Users/x/My Documents/notes on turn.md"]).as_deref(),
            Some("'/Users/x/My Documents/notes on turn.md'")
        );
    }

    /// Every shell metacharacter, in one filename, inert.
    ///
    /// Not a contrived string: `$`, backticks, `*` and `;` are all legal in a filename on
    /// every platform Turn runs on, and a path from `node_modules` or a downloads folder is
    /// where the user meets them.
    #[test]
    fn nothing_in_a_filename_can_reach_the_shell_as_syntax() {
        let hostile = "/tmp/$(rm -rf ~)`whoami`; echo *|& <>&1 #~";
        let quoted = quote(hostile).expect("no newline in it");
        assert_eq!(quoted, format!("'{hostile}'"));
        // The proof that matters: outside the wrapping quotes there is no character a shell
        // reads as syntax, because there is nothing outside them at all.
        assert!(quoted.starts_with('\''));
        assert!(quoted.ends_with('\''));
        assert_eq!(
            quoted[1..quoted.len() - 1].matches('\'').count(),
            0,
            "an unescaped quote inside would end the word early: {quoted}"
        );
    }

    /// A single quote in the name closes and reopens, which is the one thing single-quoting
    /// cannot do by itself.
    #[test]
    fn a_quote_in_the_name_is_closed_escaped_and_reopened() {
        assert_eq!(
            quote("/tmp/it's here").as_deref(),
            Some("'/tmp/it'\\''s here'")
        );
    }

    /// Several files become several words, in the order they were dropped.
    #[test]
    fn several_files_become_several_words() {
        assert_eq!(
            text_for(&["/a/one file", "/b/two"]).as_deref(),
            Some("'/a/one file' '/b/two'")
        );
    }

    /// The dangerous one. A newline in a filename is the user pressing Enter, so it is left
    /// out and said out loud rather than inserted and hoped about.
    #[test]
    fn a_newline_in_a_filename_is_refused_rather_than_typed() {
        let owned = [
            PathBuf::from("/tmp/evil\nrm -rf ~"),
            PathBuf::from("/tmp/fine"),
        ];
        let borrowed: Vec<&Path> = owned.iter().map(PathBuf::as_path).collect();
        let outcome = dropped(&borrowed);

        assert_eq!(
            outcome.text.as_deref(),
            Some("'/tmp/fine'"),
            "the safe path still goes in: one hostile name does not cost the user the drop"
        );
        assert_eq!(outcome.refused.len(), 1);
        assert!(
            !outcome.refused[0].contains('\n'),
            "not even the message repeats the newline: {:?}",
            outcome.refused[0]
        );
        let refusal = outcome.refusal().expect("something was refused");
        assert!(refusal.contains("rm -rf"), "it names the file: {refusal}");
        assert!(refusal.contains("newline"), "and why: {refusal}");
    }

    /// A carriage return alone does the same thing at a prompt as a newline does.
    #[test]
    fn a_carriage_return_is_refused_for_the_same_reason() {
        assert_eq!(quote("/tmp/evil\rwhoami"), None);
    }

    /// Nothing dropped, nothing typed. Guards the case where every path was refused: the
    /// prompt must not receive an empty paste, which in bracketed mode is two escape
    /// sequences around nothing.
    #[test]
    fn a_drop_of_only_refused_paths_types_nothing_at_all() {
        let owned = [PathBuf::from("/tmp/a\nb")];
        let borrowed: Vec<&Path> = owned.iter().map(PathBuf::as_path).collect();
        let outcome = dropped(&borrowed);
        assert_eq!(outcome.text, None);
        assert!(outcome.refusal().is_some(), "and the user is told why");
    }

    #[test]
    fn dropping_nothing_is_not_an_error() {
        assert_eq!(dropped(&[]), Dropped::default());
    }
}
