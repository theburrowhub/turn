//! Links in a pane: finding them, showing what they really point at, and opening them.
//!
//! Agents emit targets constantly — the PR they just opened, a `localhost` port, the doc
//! they cited, a file with a line number in a compiler error — and without this the user
//! selects and copies by hand. `xterm.js` gave Turn this for free; drawing the terminal
//! ourselves means writing it.
//!
//! Three ways a link arrives, in the order they take precedence over each other:
//!
//! 1. **Declared** by the program with OSC 8 ([`turn_proto::cells::RowLink`]), which is the
//!    modern mechanism and the only one where the visible text and the destination can
//!    disagree.
//! 2. **A URL detected in the text**, for the overwhelming majority of output, which just
//!    prints a URL.
//! 3. **A file path that resolves**, which is what makes a compiler error actionable.
//!
//! ## Boundaries are where naive implementations are wrong
//!
//! Every rule here exists because the obvious version of it is wrong every day:
//!
//! * A trailing `.` or `,` is punctuation. `see https://example.com.` links to
//!   `https://example.com`.
//! * A URL inside parentheses must not swallow the closing one — but a URL may legitimately
//!   *contain* parentheses, so the rule is balance, not exclusion.
//! * **A row boundary is not a word boundary.** A terminal hard-wraps, so a URL that ran off
//!   the margin is one link across two rows. The grid says which rows wrapped
//!   ([`Grid::row_wrapped`]), so this is known rather than guessed.
//! * A path is offered **only when it resolves**. Offering to open something that is not
//!   there is worse than offering nothing, and a compiler error is full of tokens that look
//!   like paths and are not.
//!
//! ## Security: every link is attacker-controlled
//!
//! A pane's text is written by a process, and an agent controls its own stdout. So:
//!
//! * **The scheme is allow-listed** ([`ALLOWED_SCHEMES`]), and the decision is made at the
//!   point of opening — [`open`] re-checks it — rather than only where links are found. A
//!   filter that lives only at detection time is one refactor away from being bypassed.
//! * **The target is normalised to printable ASCII** ([`normalise_url`]) before it is shown
//!   or opened: the host is converted to its punycode form so a Cyrillic `а` cannot
//!   impersonate an `a`, and everything else that is not display-safe
//!   ([`turn_pty::is_display_safe`]) is percent-encoded, so a right-to-left override cannot
//!   make a URL render as a different one.
//! * **Hover always shows the whole target**, never a shortened or elided form, because an
//!   elided URL is how somebody gets fooled.
//! * **A declared link whose text names a different host asks for confirmation**
//!   ([`LinkWarning`]). That is the classic phishing shape and a terminal is a place people
//!   trust.
//! * **Opening passes the target as data**, an argument to the platform opener, never a
//!   string handed to a shell.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
#[cfg(not(test))]
use std::process::{Command, Stdio};

use turn_proto::cells::{Cell, Grid, RowLink};

/// The URI schemes Turn will open.
///
/// An allow-list rather than a deny-list, because the set of schemes that can execute
/// something is open-ended: `javascript:`, `data:`, `vbscript:`, whatever a browser or a
/// desktop handler adds next. Anything not named here is refused, and the refusal happens in
/// [`open`] as well as at detection.
pub const ALLOWED_SCHEMES: [&str; 5] = ["http", "https", "ssh", "file", "mailto"];

/// Most links Turn will offer on one screen.
///
/// A screen of nothing but URLs is something a program can produce deliberately, and the map
/// is rebuilt as the pointer moves. Five hundred is far past a real directory listing.
pub const MAX_LINKS_PER_SCREEN: usize = 512;

/// Most filesystem checks one scan of a screen may make.
///
/// A path is only offered when it resolves, and resolving means asking the filesystem. A
/// screen of ten thousand plausible-looking tokens must not become ten thousand `stat` calls
/// per frame, so the checks are both capped and remembered.
pub const MAX_PATH_CHECKS_PER_SCAN: usize = 96;

/// How long a resolved path is trusted before it is checked again, in milliseconds.
///
/// Long enough that moving the pointer along a compiler error costs one check per path, short
/// enough that a file created a moment ago becomes clickable without restarting anything.
pub const PATH_MEMO_MS: i64 = 2_000;

/// How long the pointer must rest on a link before its target is shown, in milliseconds.
///
/// A tooltip that appears the instant the pointer crosses a URL would cover the text somebody
/// is reading. Holding the platform modifier shows it immediately, because that is the
/// gesture that means "I am about to open this".
pub const HOVER_DELAY_MS: i64 = 350;

/// Where a link goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// A URL whose scheme is on [`ALLOWED_SCHEMES`], normalised to printable ASCII.
    Url(String),
    /// A path that resolved when the link was found, and the place a compiler named in it.
    File {
        path: PathBuf,
        line: Option<u32>,
        column: Option<u32>,
    },
}

impl LinkTarget {
    /// The whole target, as it is shown to the user.
    ///
    /// Never shortened and never elided: a hover that hid the middle of a URL would be the
    /// mechanism by which somebody is deceived, not a nicety.
    pub fn display(&self) -> String {
        match self {
            LinkTarget::Url(url) => url.clone(),
            LinkTarget::File { path, line, column } => {
                let mut out = path.display().to_string();
                if let Some(line) = line {
                    out.push_str(&format!(":{line}"));
                    if let Some(column) = column {
                        out.push_str(&format!(":{column}"));
                    }
                }
                out
            }
        }
    }

    /// The host a URL names, lower-cased and in its ASCII form, for comparing against what
    /// the visible text claims.
    pub fn host(&self) -> Option<String> {
        match self {
            LinkTarget::Url(url) => host_of(url),
            LinkTarget::File { .. } => None,
        }
    }
}

/// How a link was found, which decides how much Turn trusts the text over it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkOrigin {
    /// Turn recognised a URL or a path in the pane's text, so the text *is* the target.
    Detected,
    /// The program declared it with OSC 8, so the text says whatever the program wanted.
    Declared,
}

/// One row's worth of a link's extent, in grid columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkSpan {
    pub row: u16,
    /// First column, inclusive.
    pub from: u16,
    /// Last column, exclusive.
    pub to: u16,
}

impl LinkSpan {
    pub fn covers(&self, row: u16, col: u16) -> bool {
        self.row == row && col >= self.from && col < self.to
    }
}

/// A link on a pane's grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub target: LinkTarget,
    /// The cells it covers, one span per grid row: a hard-wrapped URL is one link over two.
    pub spans: Vec<LinkSpan>,
    /// The text the user can see, which for a declared link is not the target.
    pub text: String,
    pub origin: LinkOrigin,
}

impl Link {
    /// Whether a cell is inside this link.
    pub fn covers(&self, row: u16, col: u16) -> bool {
        self.spans.iter().any(|span| span.covers(row, col))
    }

    /// What the user is asked to confirm, if anything.
    ///
    /// Only a declared link can lie about where it goes, so only a declared link is ever
    /// checked: a detected URL *is* its own text.
    pub fn warning(&self) -> Option<LinkWarning> {
        if self.origin != LinkOrigin::Declared {
            return None;
        }
        let target = self.target.host()?;
        let shown = host_in_text(&self.text)?;
        if shown.eq_ignore_ascii_case(&target) {
            return None;
        }
        Some(LinkWarning::TextNamesAnotherHost {
            shown,
            target: target.clone(),
        })
    }

    /// The request a window performs when the user follows this link.
    pub fn request(&self) -> LinkRequest {
        LinkRequest {
            display: self.target.display(),
            warning: self.warning(),
            target: self.target.clone(),
            text: self.text.clone(),
        }
    }
}

/// Why following a link deserves a question first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkWarning {
    /// The visible text names one host and the link points at another. The classic phishing
    /// shape: text reading `https://github.com/...` over a link to somewhere else.
    TextNamesAnotherHost { shown: String, target: String },
}

impl LinkWarning {
    /// One sentence, for a confirmation the user can actually act on.
    pub fn describe(&self) -> String {
        match self {
            LinkWarning::TextNamesAnotherHost { shown, target } => format!(
                "This text says {shown} but the link goes to {target}. \
                 The program in this pane chose both."
            ),
        }
    }
}

/// What the window is asked to do when the user follows a link.
///
/// Returned as state rather than performed by the pane: opening a URL leaves Turn, and a
/// warning needs a confirmation the pane cannot show. The pane finds the link and says so;
/// the window decides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkRequest {
    pub target: LinkTarget,
    /// The whole target, ready to show.
    pub display: String,
    /// The text the link was shown as, for a confirmation that quotes both.
    pub text: String,
    pub warning: Option<LinkWarning>,
}

impl LinkRequest {
    /// Whether the user must be asked before this is opened.
    pub fn needs_confirmation(&self) -> bool {
        self.warning.is_some()
    }
}

/// Why a link could not be opened.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OpenError {
    #[error("the scheme {0:?} is not one Turn will open")]
    RefusedScheme(String),
    #[error("{0:?} is not a URL Turn can make sense of")]
    Malformed(String),
    #[error("{0} is no longer there")]
    Missing(PathBuf),
    #[error("the system opener failed: {0}")]
    Handler(String),
}

/// Opens a link through the platform's own handler.
///
/// Two rules, and both are enforced here rather than trusted to have happened earlier:
///
/// * **The scheme is checked again.** A [`LinkTarget`] can be constructed by any caller, and
///   the allow-list has to be the last thing between a process's output and the OS. Deciding
///   it at the point of opening is what makes it impossible to bypass by finding another way
///   to build a target.
/// * **The target is an argument, never a command line.** No shell is involved, so no part of
///   a URL can be read as a shell metacharacter however it is quoted.
pub fn open(target: &LinkTarget) -> Result<(), OpenError> {
    match target {
        LinkTarget::Url(url) => {
            let scheme = scheme_of(url)
                .ok_or_else(|| OpenError::Malformed(url.clone()))?
                .to_ascii_lowercase();
            if !is_allowed_scheme(&scheme) {
                return Err(OpenError::RefusedScheme(scheme));
            }
            // Normalised again, so what reaches the OS is the same printable ASCII that was
            // shown to the user rather than whatever the caller happened to hold.
            let safe = normalise_url(url).ok_or_else(|| OpenError::Malformed(url.clone()))?;
            hand_to_opener(&safe)
        }
        LinkTarget::File { path, .. } => {
            if !path.exists() {
                return Err(OpenError::Missing(path.clone()));
            }
            hand_to_opener(path)
        }
    }
}

/// The platform command that opens a URL or a file the way the user's desktop wants.
///
/// A `file:` URL carries no line number and neither does the OS handler, so Turn does not
/// invent an editor URL scheme to smuggle one in: the line and column stay on the
/// [`LinkTarget`] and in the hover, where they are true, rather than becoming a guess about
/// which editor somebody uses.
#[cfg(not(test))]
fn opener() -> &'static str {
    if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    }
}

/// Under test this records and does not run.
///
/// A suite that launched the developer's browser would open a page per run of `make test`,
/// which is exactly what happened once. The assertion worth making is "Turn asked the desktop
/// to open this", and the record supports it better than a spawned process nobody observes.
#[cfg(test)]
fn hand_to_opener(argument: impl AsRef<std::ffi::OsStr>) -> Result<(), OpenError> {
    let asked = argument.as_ref().to_string_lossy().into_owned();
    OPENED.with(|opened| opened.borrow_mut().push(asked));
    Ok(())
}

#[cfg(not(test))]
fn hand_to_opener(argument: impl AsRef<std::ffi::OsStr>) -> Result<(), OpenError> {
    Command::new(opener())
        .arg(argument)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| OpenError::Handler(error.to_string()))
}

#[cfg(test)]
thread_local! {
    /// What [`open`] was asked to hand to the desktop, in this thread, under test.
    ///
    /// Thread-local because the harness runs tests in parallel and a shared list would make
    /// one test's assertion depend on another's timing.
    pub(crate) static OPENED: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Whether a scheme is one Turn will open. Case-insensitive, because `HTTPS:` is `https:`.
pub fn is_allowed_scheme(scheme: &str) -> bool {
    ALLOWED_SCHEMES
        .iter()
        .any(|allowed| scheme.eq_ignore_ascii_case(allowed))
}

/// The scheme of a URL, without its colon.
///
/// Strict about the shape RFC 3986 defines — a letter followed by letters, digits, `+`, `-`
/// and `.` — so that a string like `not a scheme: text` does not appear to have one, and so
/// that a scheme padded with anything unusual fails the allow-list rather than sneaking past
/// a looser reading of it.
pub fn scheme_of(url: &str) -> Option<&str> {
    let colon = url.find(':')?;
    let scheme = &url[..colon];
    let mut chars = scheme.chars();
    if !chars.next()?.is_ascii_alphabetic() {
        return None;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
        return None;
    }
    Some(scheme)
}

/// ASCII characters a URL may carry unescaped: unreserved and reserved, per RFC 3986.
/// `%` is deliberately absent: it is admitted only where it introduces a well-formed escape,
/// so a stray one becomes `%25` rather than being left to mean something else.
const URL_SAFE_ASCII: &str =
    "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-._~:/?#[]@!$&'()*+,;=";

/// Turns a URL into printable ASCII that cannot misrepresent itself.
///
/// Three things happen, and each closes a way a URL can lie:
///
/// * The scheme is checked against [`ALLOWED_SCHEMES`] and lower-cased.
/// * The host is lower-cased and converted to its **punycode ASCII form**, so `аpple.com`
///   with a Cyrillic first letter is shown as `xn--pple-43d.com` and cannot be mistaken for
///   the real one.
/// * Everything else that is not [`turn_pty::is_display_safe`] or not in the ASCII set a URL
///   may carry is **percent-encoded**. A bidirectional override in a path cannot reorder what
///   the user reads, because it is no longer a formatting character by the time they see it.
///
/// `None` when there is nothing legible to show: no scheme, a refused scheme, or a URL with
/// no host where its scheme requires one.
pub fn normalise_url(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let scheme = scheme_of(raw)?.to_ascii_lowercase();
    if !is_allowed_scheme(&scheme) {
        return None;
    }
    let rest = &raw[scheme.len() + 1..];
    let mut out = String::with_capacity(raw.len() + 8);
    out.push_str(&scheme);
    out.push(':');

    if let Some(after_slashes) = rest.strip_prefix("//") {
        out.push_str("//");
        // The authority runs to the first delimiter; everything after it is a path.
        let end = after_slashes
            .find(['/', '?', '#'])
            .unwrap_or(after_slashes.len());
        let (authority, path) = after_slashes.split_at(end);
        // `file://` legitimately has an empty authority, meaning the local machine.
        if authority.is_empty() && scheme != "file" {
            return None;
        }
        out.push_str(&normalise_authority(authority)?);
        out.push_str(&percent_escape(path));
    } else {
        // `mailto:` and the like: no authority, and the address is the whole of it.
        if rest.is_empty() {
            return None;
        }
        out.push_str(&percent_escape(rest));
    }
    Some(out)
}

/// Normalises `user:password@host:port`, converting the host to ASCII.
fn normalise_authority(authority: &str) -> Option<String> {
    let (userinfo, hostport) = match authority.rsplit_once('@') {
        Some((user, host)) => (Some(user), host),
        None => (None, authority),
    };
    let mut out = String::new();
    if let Some(userinfo) = userinfo {
        out.push_str(&percent_escape(userinfo));
        out.push('@');
    }
    // An IPv6 literal is bracketed and is already ASCII; leaving it alone avoids treating
    // its colons as a port separator.
    if hostport.starts_with('[') {
        if !hostport.is_ascii() {
            return None;
        }
        out.push_str(&hostport.to_ascii_lowercase());
        return Some(out);
    }
    let (host, port) = match hostport.rsplit_once(':') {
        // Only a numeric tail is a port; `mailto`-ish colons in a host are not.
        Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
            (host, Some(port))
        }
        _ => (hostport, None),
    };
    out.push_str(&to_ascii_host(host)?);
    if let Some(port) = port {
        out.push(':');
        out.push_str(port);
    }
    Some(out)
}

/// The ASCII form of a host: lower-cased, with each non-ASCII label punycoded.
///
/// This is the anti-confusable measure. A label that is already ASCII — including one that is
/// already `xn--…` — is passed through, so the form the user sees is always the one a browser
/// will resolve.
pub fn to_ascii_host(host: &str) -> Option<String> {
    let lower = host.to_lowercase();
    let mut labels: Vec<String> = Vec::new();
    for label in lower.split('.') {
        labels.push(punycode_label(label)?);
    }
    Some(labels.join("."))
}

/// The host a URL names, in the form [`to_ascii_host`] produces.
pub fn host_of(url: &str) -> Option<String> {
    let scheme = scheme_of(url)?;
    let rest = &url[scheme.len() + 1..];
    let after = rest.strip_prefix("//")?;
    let end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let authority = &after[..end];
    let hostport = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    if let Some(closing) = hostport.find(']') {
        return Some(hostport[..=closing].to_ascii_lowercase());
    }
    let host = match hostport.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => host,
        _ => hostport,
    };
    if host.is_empty() {
        return None;
    }
    to_ascii_host(host)
}

/// Percent-encodes everything that could make a URL render as a different one.
///
/// The predicate is the union of two rules: the character must be in the ASCII set a URL may
/// carry, *and* it must be one [`turn_pty::is_display_safe`] admits. The second is the reason
/// this exists — Turn already knows which characters can misrepresent a string, and a URL is
/// a string a user is asked to trust.
fn percent_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        // An escape that is already well formed is left as it is, or a second pass would
        // turn `%20` into `%2520`.
        if c == '%' {
            let tail = &text[index + 1..];
            let hex: String = tail.chars().take(2).collect();
            if hex.len() == 2 && hex.chars().all(|h| h.is_ascii_hexdigit()) {
                out.push('%');
                out.push_str(&hex);
                chars.next();
                chars.next();
                continue;
            }
        }
        if c.is_ascii() && URL_SAFE_ASCII.contains(c) && turn_pty::is_display_safe(c) {
            out.push(c);
            continue;
        }
        let mut buffer = [0u8; 4];
        for byte in c.encode_utf8(&mut buffer).as_bytes() {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

// Punycode, RFC 3492, as used by IDNA. Only the encoding direction is needed: Turn shows and
// opens the ASCII form, and never has to turn one back into Unicode.
const PUNY_BASE: u32 = 36;
const PUNY_TMIN: u32 = 1;
const PUNY_TMAX: u32 = 26;
const PUNY_SKEW: u32 = 38;
const PUNY_DAMP: u32 = 700;
const PUNY_INITIAL_BIAS: u32 = 72;
const PUNY_INITIAL_N: u32 = 128;

fn punycode_digit(value: u32) -> char {
    // 0..25 are 'a'..'z' and 26..35 are '0'..'9'.
    match value {
        0..=25 => (b'a' + value as u8) as char,
        26..=35 => (b'0' + (value - 26) as u8) as char,
        _ => '?',
    }
}

fn punycode_adapt(delta: u32, points: u32, first: bool) -> u32 {
    let mut delta = if first { delta / PUNY_DAMP } else { delta / 2 };
    delta += delta / points.max(1);
    let mut k = 0u32;
    while delta > ((PUNY_BASE - PUNY_TMIN) * PUNY_TMAX) / 2 {
        delta /= PUNY_BASE - PUNY_TMIN;
        k += PUNY_BASE;
    }
    k + (((PUNY_BASE - PUNY_TMIN + 1) * delta) / (delta + PUNY_SKEW))
}

/// One domain label in its ASCII form.
///
/// An all-ASCII label — including one that is already `xn--…` — is returned unchanged.
/// `None` when the label cannot be encoded, which is the honest answer for a label made of
/// characters no domain can hold; a caller must then refuse the URL rather than show a host
/// it could not convert.
fn punycode_label(label: &str) -> Option<String> {
    if label.is_ascii() {
        return Some(label.to_string());
    }
    let points: Vec<u32> = label.chars().map(u32::from).collect();
    let mut output: String = label.chars().filter(char::is_ascii).collect();
    let basic = u32::try_from(output.chars().count()).ok()?;
    if basic > 0 {
        output.push('-');
    }
    let mut n = PUNY_INITIAL_N;
    let mut delta = 0u32;
    let mut bias = PUNY_INITIAL_BIAS;
    let mut handled = basic;
    let total = u32::try_from(points.len()).ok()?;
    while handled < total {
        let next = points.iter().copied().filter(|point| *point >= n).min()?;
        delta = delta.checked_add(next.checked_sub(n)?.checked_mul(handled + 1)?)?;
        n = next;
        for &point in &points {
            if point < n {
                delta = delta.checked_add(1)?;
            }
            if point != n {
                continue;
            }
            let mut q = delta;
            let mut k = PUNY_BASE;
            loop {
                let t = k.saturating_sub(bias).clamp(PUNY_TMIN, PUNY_TMAX);
                if q < t {
                    break;
                }
                output.push(punycode_digit(t + (q - t) % (PUNY_BASE - t)));
                q = (q - t) / (PUNY_BASE - t);
                k += PUNY_BASE;
            }
            output.push(punycode_digit(q));
            bias = punycode_adapt(delta, handled + 1, handled == basic);
            delta = 0;
            handled += 1;
        }
        delta += 1;
        n += 1;
    }
    Some(format!("xn--{output}"))
}

/// The host the *visible text* of a link appears to name, if it names one.
///
/// Used only to notice that a declared link disagrees with its own label. Deliberately
/// generous: it will find a host in `https://github.com/x`, in `github.com/x`, and in bare
/// `github.com`, because the point is to catch text that *looks* like a destination, and text
/// that merely looks like one is exactly what fools somebody.
pub fn host_in_text(text: &str) -> Option<String> {
    let text = text.trim();
    if let Some(host) = host_of(text) {
        return Some(host);
    }
    text.split_whitespace().find_map(|token| {
        let token = token.trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '-');
        let authority = token.split(['/', '?', '#']).next()?;
        if !authority.contains('.') || authority.starts_with('.') || authority.ends_with('.') {
            return None;
        }
        let tld = authority.rsplit('.').next()?;
        if tld.len() < 2 || !tld.chars().all(|c| c.is_alphabetic()) {
            return None;
        }
        to_ascii_host(authority)
    })
}

/// Turns a path a program printed into one that exists.
///
/// A trait so the rule — a path is offered only when it resolves — can be tested without a
/// filesystem, and so the pane's caching lives in one place.
pub trait PathResolver {
    /// The real path a candidate names, or `None` when it names nothing.
    fn resolve(&mut self, candidate: &str) -> Option<PathBuf>;
}

/// Resolves paths against a pane's working directory, remembering the answers.
///
/// Two bounds, because this is the only part of finding links that touches the outside world:
/// at most [`MAX_PATH_CHECKS_PER_SCAN`] filesystem calls per scan, and an answer is reused
/// for [`PATH_MEMO_MS`] so moving the pointer along a compiler error does not re-`stat`
/// everything on screen every frame.
#[derive(Debug, Clone, Default)]
pub struct FsPaths {
    cwd: Option<PathBuf>,
    home: Option<PathBuf>,
    /// Candidate to `(when it was checked, what it resolved to)`.
    memo: HashMap<String, (i64, Option<PathBuf>)>,
    now_ms: i64,
    checks: usize,
}

impl FsPaths {
    /// Points the resolver at a pane's working directory.
    ///
    /// Forgets what it knew when the directory changes: the same relative path means a
    /// different file, and answering from the old directory would offer to open the wrong one.
    pub fn set_cwd(&mut self, cwd: Option<PathBuf>) {
        if self.cwd != cwd {
            self.cwd = cwd;
            self.memo.clear();
        }
    }

    pub fn cwd(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    /// Starts a scan: resets the per-scan budget and sets the clock the memo ages against.
    pub fn begin_scan(&mut self, now_ms: i64) {
        self.now_ms = now_ms;
        self.checks = 0;
        if self.memo.len() > MAX_LINKS_PER_SCREEN * 2 {
            self.memo.clear();
        }
    }

    /// Where a candidate would be, before asking whether it is there.
    fn locate(&self, candidate: &str) -> Option<PathBuf> {
        if let Some(rest) = candidate.strip_prefix("~/") {
            return Some(self.home.clone().or_else(home_dir)?.join(rest));
        }
        if candidate == "~" {
            return self.home.clone().or_else(home_dir);
        }
        let path = Path::new(candidate);
        if path.is_absolute() {
            return Some(path.to_path_buf());
        }
        Some(self.cwd.as_ref()?.join(path))
    }
}

impl PathResolver for FsPaths {
    fn resolve(&mut self, candidate: &str) -> Option<PathBuf> {
        if let Some((checked, answer)) = self.memo.get(candidate) {
            if self.now_ms.saturating_sub(*checked) < PATH_MEMO_MS {
                return answer.clone();
            }
        }
        if self.checks >= MAX_PATH_CHECKS_PER_SCAN {
            // Out of budget: answer "not a path" rather than guessing yes. A link that fails
            // to appear is a smaller failure than one that offers to open nothing.
            return None;
        }
        self.checks += 1;
        let answer = self
            .locate(candidate)
            .filter(|path| path.exists())
            .map(|path| path.canonicalize().unwrap_or(path));
        self.memo
            .insert(candidate.to_string(), (self.now_ms, answer.clone()));
        answer
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Every link on a grid, and which cell belongs to which.
#[derive(Debug, Clone, Default)]
pub struct LinkMap {
    links: Vec<Link>,
    /// Row-major `rows * cols`, an index into [`Self::links`] or [`NO_LINK`].
    index: Vec<u16>,
    rows: u16,
    cols: u16,
}

/// The entry for a cell with no link.
const NO_LINK: u16 = u16::MAX;

impl LinkMap {
    /// Finds every link on a grid.
    ///
    /// Declared links are placed first and detected ones only fill cells nobody claimed, so a
    /// program that declared a link over a URL gets the link it declared rather than two
    /// answers for one cell.
    pub fn find(grid: &Grid, paths: &mut impl PathResolver) -> Self {
        let mut map = LinkMap {
            links: Vec::new(),
            index: vec![NO_LINK; grid.rows as usize * grid.cols as usize],
            rows: grid.rows,
            cols: grid.cols,
        };
        for link in declared_links(grid) {
            map.place(link);
        }
        let lines = logical_lines(grid);
        for line in &lines {
            for found in scan_urls(line) {
                map.place(found);
            }
        }
        for line in &lines {
            for found in scan_paths(line, paths) {
                map.place(found);
            }
        }
        map
    }

    /// An empty map, for a pane nobody is pointing at.
    pub fn none() -> Self {
        Self::default()
    }

    fn place(&mut self, link: Link) {
        if self.links.len() >= MAX_LINKS_PER_SCREEN {
            return;
        }
        // A link may not overlap one already placed: "which link is under this cell" has to
        // have one answer, and the first answer is the more trustworthy one.
        for span in &link.spans {
            for col in span.from..span.to {
                if self.cell_index(span.row, col).is_some_and(|i| i != NO_LINK) {
                    return;
                }
            }
        }
        let at = self.links.len() as u16;
        for span in &link.spans {
            for col in span.from..span.to {
                if let Some(slot) = self.slot(span.row, col) {
                    *slot = at;
                }
            }
        }
        self.links.push(link);
    }

    fn cell_index(&self, row: u16, col: u16) -> Option<u16> {
        if row >= self.rows || col >= self.cols {
            return None;
        }
        self.index
            .get(row as usize * self.cols as usize + col as usize)
            .copied()
    }

    fn slot(&mut self, row: u16, col: u16) -> Option<&mut u16> {
        if row >= self.rows || col >= self.cols {
            return None;
        }
        self.index
            .get_mut(row as usize * self.cols as usize + col as usize)
    }

    /// The link under a cell.
    pub fn at(&self, row: u16, col: u16) -> Option<&Link> {
        let index = self.cell_index(row, col)?;
        if index == NO_LINK {
            return None;
        }
        self.links.get(index as usize)
    }

    pub fn links(&self) -> &[Link] {
        &self.links
    }

    pub fn len(&self) -> usize {
        self.links.len()
    }

    pub fn is_empty(&self) -> bool {
        self.links.is_empty()
    }
}

/// A run of grid rows that is one line of text.
///
/// `chars` and `at` are the same length: the cell each character came from, so a range of the
/// text maps back to the cells it occupies. That is what makes a URL that hard-wrapped one
/// link rather than two halves.
#[derive(Debug, Clone, Default)]
pub struct LogicalLine {
    chars: Vec<char>,
    at: Vec<(u16, u16)>,
    /// How many columns the cell at each position occupies, so a span can include the
    /// trailing half of a double-width glyph.
    width: Vec<u16>,
}

impl LogicalLine {
    /// The line as one string, however many grid rows it came from.
    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    /// The grid spans covering a half-open range of this line's characters.
    fn spans(&self, from: usize, to: usize) -> Vec<LinkSpan> {
        let mut spans: Vec<LinkSpan> = Vec::new();
        for position in from..to.min(self.at.len()) {
            let (row, col) = self.at[position];
            let end = col.saturating_add(self.width[position].max(1));
            match spans.last_mut() {
                Some(last) if last.row == row => last.to = last.to.max(end),
                _ => spans.push(LinkSpan {
                    row,
                    from: col,
                    to: end,
                }),
            }
        }
        spans
    }
}

/// Groups a grid's rows into logical lines, joining the ones the terminal hard-wrapped.
///
/// The join is the whole reason this exists. A terminal breaks a long line at the margin, so
/// `https://example.com/a/very/long/` on one row and `path` on the next is one URL; treating
/// the row boundary as a word boundary would produce two useless fragments. The grid records
/// which rows wrapped, so this is a fact rather than a guess about full rows.
pub fn logical_lines(grid: &Grid) -> Vec<LogicalLine> {
    let mut lines: Vec<LogicalLine> = Vec::new();
    let mut current = LogicalLine::default();
    for row in 0..grid.rows {
        for col in 0..grid.cols {
            let cell = grid.cell(row, col);
            // The right-hand half of a double-width glyph holds no text of its own; counting
            // it would insert a space inside every emoji.
            if cell.is_some_and(Cell::is_trailer) {
                continue;
            }
            let width = cell.map_or(1, Cell::columns);
            // A tile of an inline image reads as a space, so a URL cannot be found running
            // through a picture and a marker cannot end up inside a link's text.
            match cell.filter(|cell| !cell.text.is_empty() && !cell.is_image()) {
                Some(cell) => {
                    for c in cell.text.chars() {
                        current.chars.push(c);
                        current.at.push((row, col));
                        current.width.push(width);
                    }
                }
                None => {
                    current.chars.push(' ');
                    current.at.push((row, col));
                    current.width.push(width);
                }
            }
        }
        if grid.row_wrapped(row) {
            continue;
        }
        trim_trailing_blanks(&mut current);
        lines.push(std::mem::take(&mut current));
    }
    if !current.chars.is_empty() {
        trim_trailing_blanks(&mut current);
        lines.push(current);
    }
    lines
}

/// Drops a line's trailing padding, which is not text anybody wrote.
fn trim_trailing_blanks(line: &mut LogicalLine) {
    while line.chars.last() == Some(&' ') {
        line.chars.pop();
        line.at.pop();
        line.width.pop();
    }
}

/// Whether a character may sit inside a URL as Turn reads one.
///
/// Stops at whitespace, at controls, and at the characters RFC 3986 names as delimiters when
/// a URI is embedded in running text. Quotes are delimiters here even though a URL may
/// legally contain them: shells and agents quote URLs far more often than URLs contain
/// quotes, and stopping at one costs a rare query string while including it would put a quote
/// on the end of every quoted URL.
fn is_url_char(c: char) -> bool {
    !c.is_whitespace()
        && !c.is_control()
        && !matches!(
            c,
            '<' | '>' | '"' | '\'' | '`' | '{' | '}' | '|' | '\\' | '^'
        )
}

/// Whether a character could be part of a token, for deciding that a scheme really starts.
fn is_token_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '+' | '-' | '.' | '_' | '%' | ':' | '/' | '@' | '~')
}

/// Finds every URL in a logical line.
pub fn scan_urls(line: &LogicalLine) -> Vec<Link> {
    let mut found = Vec::new();
    let chars = &line.chars;
    let mut at = 0usize;
    while at < chars.len() {
        // A candidate starts at the beginning of a token. Alphanumeric rather than alphabetic
        // because `127.0.0.1:8080` is one of the things worth finding and it starts with a
        // digit.
        if !chars[at].is_ascii_alphanumeric() || (at > 0 && is_token_char(chars[at - 1])) {
            at += 1;
            continue;
        }
        match url_at(line, at).or_else(|| loopback_at(line, at)) {
            Some((link, end)) => {
                found.push(link);
                at = end;
            }
            None => at += 1,
        }
    }
    found
}

/// A URL starting at `at`, and where it ends.
fn url_at(line: &LogicalLine, at: usize) -> Option<(Link, usize)> {
    let chars = &line.chars;
    let mut cursor = at;
    while cursor < chars.len()
        && (chars[cursor].is_ascii_alphanumeric() || matches!(chars[cursor], '+' | '-' | '.'))
    {
        cursor += 1;
    }
    let scheme: String = chars[at..cursor].iter().collect();
    if !is_allowed_scheme(&scheme) || chars.get(cursor) != Some(&':') {
        return None;
    }
    let mut body = cursor + 1;
    let lower = scheme.to_ascii_lowercase();
    if lower != "mailto" {
        // Every other allowed scheme is hierarchical, so `//` has to be there. Without this,
        // a compiler error reading `file:42` would look like a URL.
        if chars.get(body) != Some(&'/') || chars.get(body + 1) != Some(&'/') {
            return None;
        }
        body += 2;
    }
    let mut end = body;
    while end < chars.len() && is_url_char(chars[end]) {
        end += 1;
    }
    let end = trim_url_end(chars, at, end);
    if end <= body {
        return None;
    }
    let raw: String = chars[at..end].iter().collect();
    let target = LinkTarget::Url(normalise_url(&raw)?);
    Some((
        Link {
            target,
            spans: line.spans(at, end),
            text: raw,
            origin: LinkOrigin::Detected,
        },
        end,
    ))
}

/// A bare loopback authority — `localhost:3000`, `127.0.0.1:8080` — promoted to `http://`.
///
/// Restricted to the loopback names on purpose. Agents print these constantly and they are
/// worth a click, but promoting *any* `host:port` to a URL would turn a compiler message like
/// `error:42` into a link to a machine called `error`.
fn loopback_at(line: &LogicalLine, at: usize) -> Option<(Link, usize)> {
    const LOOPBACK: [&str; 3] = ["localhost", "127.0.0.1", "0.0.0.0"];
    let chars = &line.chars;
    let tail: String = chars[at..].iter().collect();
    let host = LOOPBACK
        .iter()
        .find(|host| tail.starts_with(**host))
        .copied()?;
    let mut end = at + host.chars().count();
    if chars.get(end) != Some(&':') {
        return None;
    }
    end += 1;
    let digits = end;
    while end < chars.len() && chars[end].is_ascii_digit() {
        end += 1;
    }
    if end == digits {
        return None;
    }
    // A path may follow the port, but nothing else: a stray word after it belongs to the
    // sentence, not to the address.
    while end < chars.len() && chars[end] == '/' {
        while end < chars.len() && is_url_char(chars[end]) {
            end += 1;
        }
    }
    let end = trim_url_end(chars, at, end);
    let raw: String = chars[at..end].iter().collect();
    let target = LinkTarget::Url(normalise_url(&format!("http://{raw}"))?);
    Some((
        Link {
            target,
            spans: line.spans(at, end),
            text: raw,
            origin: LinkOrigin::Detected,
        },
        end,
    ))
}

/// Moves a URL's end back over anything that belongs to the sentence rather than the URL.
///
/// Two rules. Sentence punctuation at the end is never part of a URL, so `example.com.` is
/// `example.com`. A closing bracket is part of it only if the URL opened one — that is what
/// makes `(see https://example.com/x)` link to `…/x` while
/// `https://en.wikipedia.org/wiki/Rust_(programming_language)` keeps its own parenthesis.
fn trim_url_end(chars: &[char], start: usize, mut end: usize) -> usize {
    while end > start {
        let last = chars[end - 1];
        let closer = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' => {
                end -= 1;
                continue;
            }
            ')' => '(',
            ']' => '[',
            '}' => '{',
            _ => break,
        };
        let opens = chars[start..end].iter().filter(|c| **c == closer).count();
        let closes = chars[start..end].iter().filter(|c| **c == last).count();
        if closes <= opens {
            break;
        }
        end -= 1;
    }
    end
}

/// Finds every file path in a logical line that resolves.
pub fn scan_paths(line: &LogicalLine, paths: &mut impl PathResolver) -> Vec<Link> {
    let mut found = Vec::new();
    let chars = &line.chars;
    let mut at = 0usize;
    while at < chars.len() {
        if chars[at].is_whitespace() {
            at += 1;
            continue;
        }
        let mut end = at;
        while end < chars.len() && !chars[end].is_whitespace() {
            end += 1;
        }
        if let Some(link) = path_in_token(line, at, end, paths) {
            found.push(link);
        }
        at = end;
    }
    found
}

/// A path link inside one whitespace-delimited token, if the token holds one.
fn path_in_token(
    line: &LogicalLine,
    start: usize,
    end: usize,
    paths: &mut impl PathResolver,
) -> Option<Link> {
    let chars = &line.chars;
    // Brackets and quotes around a path belong to whoever printed it. Trailing sentence
    // punctuation goes too, but not before the line and column have been read off the end.
    let mut from = start;
    let mut to = end;
    while from < to && matches!(chars[from], '(' | '[' | '{' | '<' | '"' | '\'' | '`') {
        from += 1;
    }
    while to > from
        && matches!(
            chars[to - 1],
            ')' | ']' | '}' | '>' | '"' | '\'' | '`' | ','
        )
    {
        to -= 1;
    }
    // A trailing colon is how `grep` and `make` end a location.
    while to > from && chars[to - 1] == ':' {
        to -= 1;
    }
    let token: String = chars[from..to].iter().collect();
    let (path, line_no, column) = split_location(&token);
    if !looks_like_path(path) {
        return None;
    }
    let resolved = paths.resolve(path)?;
    // Only the path's own characters are part of the link; the line and column are shown in
    // the hover but clicking the number is clicking the path.
    let path_chars = path.chars().count();
    Some(Link {
        target: LinkTarget::File {
            path: resolved,
            line: line_no,
            column,
        },
        spans: line.spans(from, from + path_chars),
        text: token,
        origin: LinkOrigin::Detected,
    })
}

/// Splits `path:line:column` into its parts, as a compiler or `grep` writes it.
///
/// Only a numeric tail counts, so `https:` and a Windows drive letter are not mistaken for a
/// line number, and a path that genuinely contains a colon keeps it.
pub fn split_location(token: &str) -> (&str, Option<u32>, Option<u32>) {
    let numeric = |part: &str| -> Option<u32> {
        if part.is_empty() || part.len() > 9 || !part.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        part.parse().ok()
    };
    let Some((head, last)) = token.rsplit_once(':') else {
        return (token, None, None);
    };
    let Some(last_number) = numeric(last) else {
        return (token, None, None);
    };
    match head.rsplit_once(':') {
        Some((path, middle)) => match numeric(middle) {
            Some(line) => (path, Some(line), Some(last_number)),
            None => (head, Some(last_number), None),
        },
        None => (head, Some(last_number), None),
    }
}

/// Whether a token is worth asking the filesystem about.
///
/// The gate before a `stat`, not a decision about whether the path exists. Deliberately
/// narrow: a screen of prose contains hundreds of words, and asking about every one of them
/// would make hovering a pane cost a directory scan.
pub fn looks_like_path(token: &str) -> bool {
    if token.is_empty() || token.len() > 512 {
        return false;
    }
    if token.contains(':') || token.chars().any(|c| c.is_control()) {
        // A colon is either a scheme or a location that has already been split off. Either
        // way what is left is not a path Turn will resolve.
        return false;
    }
    if token.starts_with('/') || token.starts_with("./") || token.starts_with("../") {
        return true;
    }
    if token == "~" || token.starts_with("~/") {
        return true;
    }
    if token.contains('/') {
        return true;
    }
    // A bare filename, but only with a plausible extension: `main.rs`, not `e.g`.
    match token.rsplit_once('.') {
        Some((stem, extension)) => {
            !stem.is_empty()
                && (1..=10).contains(&extension.len())
                && extension.chars().all(|c| c.is_ascii_alphanumeric())
        }
        None => false,
    }
}

/// The links a program declared with OSC 8, joined across the rows they wrapped over.
///
/// A wrapped declared link arrives as one span per row; a hover has to treat it as one link
/// or the tooltip would appear and disappear as the pointer crossed the margin.
pub fn declared_links(grid: &Grid) -> Vec<Link> {
    let mut links: Vec<Link> = Vec::new();
    for row in 0..grid.rows {
        for declared in grid.row_links(row) {
            match links
                .last_mut()
                .filter(|last| continues(last, row, declared, grid))
            {
                Some(last) => {
                    last.spans.push(LinkSpan {
                        row,
                        from: declared.from,
                        to: declared.to,
                    });
                    last.text.push_str(&span_text(grid, row, declared));
                }
                None => {
                    let Some(url) = normalise_url(&declared.uri) else {
                        // A scheme Turn will not open is not offered as a link at all. The
                        // refusal that matters is in `open`, but there is no reason to show
                        // somebody a link that would be refused.
                        continue;
                    };
                    links.push(Link {
                        target: LinkTarget::Url(url),
                        spans: vec![LinkSpan {
                            row,
                            from: declared.from,
                            to: declared.to,
                        }],
                        text: span_text(grid, row, declared),
                        origin: LinkOrigin::Declared,
                    });
                }
            }
        }
    }
    // A hard wrap is not whitespace. Trim once after all of a link's spans have been
    // joined; trimming every physical row would silently remove a real separating space at
    // the margin and change the label the user is being asked to trust.
    for link in &mut links {
        link.text = link.text.trim().to_string();
    }
    links
}

/// Whether a declared span continues the link before it: same URI, the row above wrapped, and
/// the two spans meet at the margin.
fn continues(last: &Link, row: u16, declared: &RowLink, grid: &Grid) -> bool {
    let Some(previous) = last.spans.last() else {
        return false;
    };
    previous.row + 1 == row
        && grid.row_wrapped(previous.row)
        && previous.to == grid.cols
        && declared.from == 0
        && last.target == LinkTarget::Url(normalise_url(&declared.uri).unwrap_or_default())
}

/// The text under a declared span, as the user sees it.
fn span_text(grid: &Grid, row: u16, span: &RowLink) -> String {
    let mut out = String::new();
    for col in span.from..span.to {
        match grid.cell(row, col) {
            Some(cell) if cell.is_trailer() => {}
            // A marker is not text the user can see, so it is not text a link may show.
            Some(cell) if cell.is_image() => out.push(' '),
            Some(cell) if !cell.text.is_empty() => out.push_str(&cell.text),
            _ => out.push(' '),
        }
    }
    out
}

/// Whether the target of a hovered link should be shown yet.
///
/// Immediately while the platform modifier is held, because that gesture means "I am about to
/// open this" and the whole point is that the user sees where it goes first. Otherwise after
/// [`HOVER_DELAY_MS`], so a pointer crossing a line of output does not cover the text
/// somebody is reading.
pub fn target_visible(hovered_since_ms: i64, now_ms: i64, modifier_held: bool) -> bool {
    modifier_held || now_ms.saturating_sub(hovered_since_ms) >= HOVER_DELAY_MS
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// A resolver that knows a fixed set of paths, so the rules can be tested without
    /// touching a filesystem.
    #[derive(Debug, Default)]
    struct Known {
        exist: HashSet<String>,
        asked: Vec<String>,
    }

    impl Known {
        fn with(paths: &[&str]) -> Self {
            Self {
                exist: paths.iter().map(|p| p.to_string()).collect(),
                asked: Vec::new(),
            }
        }
    }

    impl PathResolver for Known {
        fn resolve(&mut self, candidate: &str) -> Option<PathBuf> {
            self.asked.push(candidate.to_string());
            self.exist
                .contains(candidate)
                .then(|| PathBuf::from("/repo").join(candidate))
        }
    }

    /// A resolver that says no to everything, for the many tests about URLs.
    #[derive(Debug, Default)]
    struct NoPaths;

    impl PathResolver for NoPaths {
        fn resolve(&mut self, _candidate: &str) -> Option<PathBuf> {
            None
        }
    }

    fn grid(lines: &[&str]) -> Grid {
        let cols = lines.iter().map(|l| l.chars().count()).max().unwrap_or(1);
        Grid::from_lines(lines, cols.max(1) as u16)
    }

    fn urls(lines: &[&str]) -> Vec<String> {
        let grid = grid(lines);
        LinkMap::find(&grid, &mut NoPaths)
            .links()
            .iter()
            .map(|link| link.target.display())
            .collect()
    }

    fn only_url(line: &str) -> String {
        let found = urls(&[line]);
        assert_eq!(found.len(), 1, "expected one link in {line:?}: {found:?}");
        found.into_iter().next().unwrap_or_default()
    }

    #[test]
    fn the_schemes_turn_opens_are_the_ones_it_finds() {
        for good in ["http", "https", "HTTPS", "ssh", "file", "mailto"] {
            assert!(is_allowed_scheme(good), "{good} must be allowed");
        }
        for bad in [
            "javascript",
            "JavaScript",
            "data",
            "vbscript",
            "about",
            "blob",
            "jar",
            "chrome",
            "smb",
            "ftp",
            "tel",
            "",
        ] {
            assert!(!is_allowed_scheme(bad), "{bad} must not be allowed");
        }
    }

    /// The refusal that matters. A target can be built by any caller, so the allow-list has
    /// to be the last thing between a process's output and the OS.
    #[test]
    fn opening_a_scheme_that_can_execute_something_is_refused_at_the_point_of_opening() {
        for hostile in [
            "javascript:alert(document.cookie)",
            "JAVASCRIPT:alert(1)",
            "data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==",
            "vbscript:msgbox(1)",
            "about:config",
            "blob:https://example.com/x",
            "jar:http://example.com/a!/b",
            "chrome://settings",
            "smb://host/share",
            "ftp://host/file",
        ] {
            let error = open(&LinkTarget::Url(hostile.to_string()))
                .expect_err("{hostile} must not be opened");
            assert!(
                matches!(error, OpenError::RefusedScheme(_) | OpenError::Malformed(_)),
                "{hostile} was refused for the wrong reason: {error}"
            );
            assert!(
                normalise_url(hostile).is_none(),
                "{hostile} must not normalise into something openable"
            );
        }

        // And something with no scheme at all is malformed rather than assumed to be http.
        assert!(matches!(
            open(&LinkTarget::Url("example.com/x".into())),
            Err(OpenError::Malformed(_))
        ));
        assert!(matches!(
            open(&LinkTarget::Url(String::new())),
            Err(OpenError::Malformed(_))
        ));
    }

    /// A path that has been deleted since the link was found must not be handed to the OS.
    #[test]
    fn opening_a_file_that_is_no_longer_there_is_refused() {
        let target = LinkTarget::File {
            path: PathBuf::from("/definitely/not/here/turn-test-missing"),
            line: Some(4),
            column: None,
        };
        assert!(matches!(open(&target), Err(OpenError::Missing(_))));
    }

    /// The boundary rules, which is where naive implementations are wrong every day.
    #[test]
    fn a_url_stops_where_the_sentence_resumes() {
        assert_eq!(
            only_url("see https://example.com/pr/42."),
            "https://example.com/pr/42"
        );
        assert_eq!(
            only_url("one https://example.com/a, two"),
            "https://example.com/a"
        );
        assert_eq!(
            only_url("(see https://example.com/x)"),
            "https://example.com/x"
        );
        assert_eq!(
            only_url("read https://en.wikipedia.org/wiki/Rust_(programming_language)"),
            "https://en.wikipedia.org/wiki/Rust_(programming_language)",
            "a URL may contain a balanced parenthesis of its own"
        );
        assert_eq!(
            only_url("[https://example.com/a]"),
            "https://example.com/a",
            "a bracket that the URL did not open is not part of it"
        );
        assert_eq!(
            only_url("run 'https://example.com/a?b=c'"),
            "https://example.com/a?b=c"
        );
        assert_eq!(
            only_url("at https://example.com/a!"),
            "https://example.com/a"
        );
        assert_eq!(
            only_url("ends https://example.com/a?"),
            "https://example.com/a"
        );
        // A colon before a port is not sentence punctuation.
        assert_eq!(
            only_url("http://example.com:8080/x"),
            "http://example.com:8080/x"
        );
    }

    /// A terminal hard-wraps, so a row boundary is not a word boundary. This is one link.
    #[test]
    fn a_url_that_wrapped_at_the_margin_is_one_link_over_two_rows() {
        let mut grid = Grid::blank(3, 20);
        let text = "see https://example.com/a/very/long/path?x=1";
        for (index, ch) in text.chars().enumerate() {
            let row = (index / 20) as u16;
            let col = (index % 20) as u16;
            if let Some(cell) = grid.cell_mut(row, col) {
                cell.text = ch.to_string();
            }
        }
        assert!(grid.set_row_wrapped(0, true));
        assert!(grid.set_row_wrapped(1, true));

        let map = LinkMap::find(&grid, &mut NoPaths);
        assert_eq!(map.len(), 1, "got {:?}", map.links());
        let link = &map.links()[0];
        assert_eq!(
            link.target.display(),
            "https://example.com/a/very/long/path?x=1"
        );
        assert_eq!(link.spans.len(), 3, "one span per row it crosses");
        assert!(link.covers(0, 5), "the start of the URL is inside it");
        assert!(link.covers(1, 0), "and so is the row it wrapped onto");
        assert!(!link.covers(0, 0), "but not the word before it");
        assert!(map.at(2, 2).is_some());

        // Without the wrap flag the same rows are separate lines, and the fragments are not
        // offered as links at all.
        let mut unwrapped = grid.clone();
        for row in 0..3 {
            assert!(unwrapped.set_row_wrapped(row, false));
        }
        let split = LinkMap::find(&unwrapped, &mut NoPaths);
        assert_eq!(
            split.links()[0].target.display(),
            "https://example",
            "a row that did not wrap ends its line, and the fragment on it is a different URL"
        );
        assert!(
            split.links().iter().all(|link| link.spans.len() == 1),
            "and no link crosses a row boundary: {:?}",
            split.links()
        );
    }

    #[test]
    fn the_other_schemes_agents_print_are_found_too() {
        assert_eq!(
            only_url("clone ssh://git@github.com/o/r.git"),
            "ssh://git@github.com/o/r.git"
        );
        assert_eq!(
            only_url("open file:///repo/notes.md"),
            "file:///repo/notes.md"
        );
        assert_eq!(
            only_url("mail mailto:someone@example.com"),
            "mailto:someone@example.com"
        );
        assert!(
            urls(&["a javascript:alert(1) b"]).is_empty(),
            "a refused scheme is never offered"
        );
        assert!(
            urls(&["error at file:42 in the parser"]).is_empty(),
            "a scheme with no authority is not a URL"
        );
        assert!(
            urls(&["nothttps://example.com"]).is_empty(),
            "a scheme in the middle of a word is not a scheme"
        );
    }

    /// The one bare authority worth promoting, because agents print it constantly.
    #[test]
    fn a_loopback_address_with_a_port_becomes_a_link_and_nothing_else_does() {
        assert_eq!(
            only_url("serving on localhost:3000"),
            "http://localhost:3000"
        );
        assert_eq!(
            only_url("bound to 127.0.0.1:8080/health"),
            "http://127.0.0.1:8080/health"
        );
        assert_eq!(only_url("listening on 0.0.0.0:5173"), "http://0.0.0.0:5173");
        assert!(
            urls(&["error:42 in the parser"]).is_empty(),
            "promoting any host:port would make a compiler message a link"
        );
        assert!(
            urls(&["see localhost for details"]).is_empty(),
            "a name with no port is not an address"
        );
        assert!(urls(&["localhost:"]).is_empty());
    }

    /// A compiler error is the case that makes this worth having, and a path is only offered
    /// when it resolves.
    #[test]
    fn a_path_with_a_line_and_column_is_a_link_only_when_it_resolves() {
        let mut known = Known::with(&["src/main.rs"]);
        let grid = grid(&["error[E0308]: at src/main.rs:42:8 and src/gone.rs:9"]);
        let map = LinkMap::find(&grid, &mut known);
        assert_eq!(map.len(), 1, "got {:?}", map.links());
        assert_eq!(
            map.links()[0].target,
            LinkTarget::File {
                path: PathBuf::from("/repo/src/main.rs"),
                line: Some(42),
                column: Some(8),
            }
        );
        assert_eq!(
            map.links()[0].target.display(),
            "/repo/src/main.rs:42:8",
            "the hover says exactly which line will be opened"
        );
        assert!(
            known.asked.contains(&"src/gone.rs".to_string()),
            "the second path was considered and refused: {:?}",
            known.asked
        );

        // The link covers the path, not the line number: clicking a digit opens the file.
        let link = &map.links()[0];
        let start = "error[E0308]: at ".chars().count() as u16;
        assert!(link.covers(0, start));
        assert!(link.covers(0, start + 10));
        assert!(!link.covers(0, start + 12), "the `:42:8` is not the link");
    }

    #[test]
    fn the_shapes_a_compiler_writes_a_location_in_are_all_understood() {
        assert_eq!(
            split_location("src/main.rs:42:8"),
            ("src/main.rs", Some(42), Some(8))
        );
        assert_eq!(
            split_location("src/main.rs:42"),
            ("src/main.rs", Some(42), None)
        );
        assert_eq!(split_location("src/main.rs"), ("src/main.rs", None, None));
        assert_eq!(
            split_location("/abs/path/x.rs:7:1"),
            ("/abs/path/x.rs", Some(7), Some(1))
        );
        assert_eq!(
            split_location("weird:name.rs"),
            ("weird:name.rs", None, None),
            "only a numeric tail is a line number"
        );
        assert_eq!(
            split_location("x.rs:99999999999"),
            ("x.rs:99999999999", None, None),
            "a number no file has is not a line number"
        );
    }

    #[test]
    fn only_tokens_worth_a_filesystem_call_are_offered_to_the_resolver() {
        for path in [
            "/etc/hosts",
            "./run.sh",
            "../sibling/file",
            "~/notes.md",
            "~",
            "src/main.rs",
            "main.rs",
            "Makefile.toml",
        ] {
            assert!(looks_like_path(path), "{path} should be considered");
        }
        for not in [
            "",
            "the",
            "error[E0308]",
            // A colon is either a scheme or a location that has already been split off.
            "https://example.com",
            "src/main.rs:42",
            "a\u{1b}b",
            &"x".repeat(600),
        ] {
            assert!(!looks_like_path(not), "{not:?} should not be considered");
        }
        // `e.g` is asked about, and that is the accepted cost of `x.c` being a real filename:
        // a bare word with a plausible extension gets one `stat`, remembered for the rest of
        // the screen. The answer is no, and no link appears.
        assert!(looks_like_path("e.g"));

        // A screen of prose must not become a screen of filesystem calls.
        let mut known = Known::default();
        let prose = "the quick brown fox jumps over the lazy dog again and again";
        let grid = grid(&[prose, prose, prose]);
        assert!(LinkMap::find(&grid, &mut known).is_empty());
        assert!(
            known.asked.is_empty(),
            "no word of prose is worth a stat: {:?}",
            known.asked
        );
    }

    /// The whole point of OSC 8: the text says one thing and the link goes somewhere else.
    #[test]
    fn a_declared_link_takes_its_target_from_the_program_and_not_from_the_text() {
        let mut grid = grid(&["open the PR now"]);
        assert!(grid.set_row_meta(
            0,
            turn_proto::cells::RowMeta {
                wrapped: false,
                links: vec![RowLink::new(5, 12, "https://example.com/pull/42")],
            }
        ));
        let map = LinkMap::find(&grid, &mut NoPaths);
        assert_eq!(map.len(), 1);
        let link = &map.links()[0];
        assert_eq!(link.origin, LinkOrigin::Declared);
        assert_eq!(link.target.display(), "https://example.com/pull/42");
        assert_eq!(link.text, "the PR");
        assert!(link.warning().is_none(), "the text names no other host");
        assert_eq!(
            map.at(0, 6).map(|l| l.target.display()).as_deref(),
            Some("https://example.com/pull/42")
        );
        assert!(map.at(0, 0).is_none());
    }

    /// The classic phishing shape, and the reason a declared link is checked against its own
    /// label at all.
    #[test]
    fn a_declared_link_whose_text_names_another_host_asks_for_confirmation() {
        let cases = [
            ("https://github.com/turn/turn", "https://evil.example/steal"),
            ("github.com", "https://evil.example/steal"),
            ("  https://GITHUB.com/a  ", "https://evil.example"),
            ("Visit github.com now", "https://evil.example/steal"),
        ];
        for (text, target) in cases {
            let mut grid = Grid::blank(1, 80);
            for (col, ch) in text.chars().enumerate() {
                if let Some(cell) = grid.cell_mut(0, col as u16) {
                    cell.text = ch.to_string();
                }
            }
            assert!(grid.set_row_meta(
                0,
                turn_proto::cells::RowMeta {
                    wrapped: false,
                    links: vec![RowLink::new(0, text.chars().count() as u16, target)],
                }
            ));
            let map = LinkMap::find(&grid, &mut NoPaths);
            let link = map.links().first().expect("a declared link");
            let request = link.request();
            assert!(
                request.needs_confirmation(),
                "{text:?} over {target:?} must be questioned"
            );
            let description = request.warning.as_ref().expect("a warning").describe();
            assert!(
                description.contains("evil.example"),
                "the confirmation must name the real destination: {description}"
            );
            assert_eq!(request.display, LinkTarget::Url(target.into()).display());
        }

        // The same host in both is not a warning, and neither is a link whose text is a label.
        for (text, target) in [
            ("https://github.com/a", "https://github.com/b"),
            ("the PR", "https://github.com/a"),
            ("build #42", "https://ci.example/42"),
        ] {
            let mut grid = Grid::blank(1, 60);
            for (col, ch) in text.chars().enumerate() {
                if let Some(cell) = grid.cell_mut(0, col as u16) {
                    cell.text = ch.to_string();
                }
            }
            assert!(grid.set_row_meta(
                0,
                turn_proto::cells::RowMeta {
                    wrapped: false,
                    links: vec![RowLink::new(0, text.chars().count() as u16, target)],
                }
            ));
            let map = LinkMap::find(&grid, &mut NoPaths);
            assert!(
                !map.links()[0].request().needs_confirmation(),
                "{text:?} over {target:?} must not be questioned"
            );
        }
    }

    /// A detected URL is its own text, so it can never disagree with itself.
    #[test]
    fn a_detected_url_is_never_questioned_because_the_text_is_the_target() {
        let grid = grid(&["https://example.com/a"]);
        let map = LinkMap::find(&grid, &mut NoPaths);
        assert_eq!(map.links()[0].origin, LinkOrigin::Detected);
        assert!(!map.links()[0].request().needs_confirmation());
    }

    /// A declared link over text that is itself a URL must win, or the user would be told
    /// about the wrong destination.
    #[test]
    fn a_declared_link_takes_precedence_over_a_url_detected_under_it() {
        let text = "https://example.com/safe";
        let mut grid = grid(&[text]);
        assert!(grid.set_row_meta(
            0,
            turn_proto::cells::RowMeta {
                wrapped: false,
                links: vec![RowLink::new(
                    0,
                    text.chars().count() as u16,
                    "https://evil.example"
                )],
            }
        ));
        let map = LinkMap::find(&grid, &mut NoPaths);
        assert_eq!(map.len(), 1, "got {:?}", map.links());
        assert_eq!(map.links()[0].target.display(), "https://evil.example");
        assert!(map.links()[0].request().needs_confirmation());
    }

    /// A declared link that a program pointed at a scheme Turn will not open is not shown as
    /// a link at all: there is no reason to offer something that would be refused.
    #[test]
    fn a_declared_link_with_a_refused_scheme_is_not_offered() {
        let mut grid = grid(&["click here"]);
        assert!(grid.set_row_meta(
            0,
            turn_proto::cells::RowMeta {
                wrapped: false,
                links: vec![RowLink::new(0, 5, "javascript:alert(1)")],
            }
        ));
        assert!(LinkMap::find(&grid, &mut NoPaths).is_empty());
    }

    /// A declared link that wrapped is one link, or the tooltip would flicker as the pointer
    /// crossed the margin.
    #[test]
    fn a_declared_link_that_wrapped_is_joined_back_into_one() {
        let mut grid = Grid::blank(2, 10);
        for (index, ch) in "the whole label".chars().enumerate() {
            let (row, col) = ((index / 10) as u16, (index % 10) as u16);
            if let Some(cell) = grid.cell_mut(row, col) {
                cell.text = ch.to_string();
            }
        }
        assert!(grid.set_row_meta(
            0,
            turn_proto::cells::RowMeta {
                wrapped: true,
                links: vec![RowLink::new(0, 10, "https://example.com/x")],
            }
        ));
        assert!(grid.set_row_meta(
            1,
            turn_proto::cells::RowMeta {
                wrapped: false,
                links: vec![RowLink::new(0, 5, "https://example.com/x")],
            }
        ));
        let map = LinkMap::find(&grid, &mut NoPaths);
        assert_eq!(map.len(), 1, "got {:?}", map.links());
        assert_eq!(map.links()[0].spans.len(), 2);
        assert_eq!(map.links()[0].text, "the whole label");
    }

    /// A Unicode confusable must not be able to misrepresent the host. This is the reason the
    /// URL Turn shows is always the ASCII one.
    #[test]
    fn a_confusable_host_is_shown_in_its_ascii_form_so_it_cannot_impersonate_another() {
        // A Cyrillic `а` in place of the Latin one.
        let attack = "https://\u{430}pple.com/id";
        let shown = normalise_url(attack).expect("the URL normalises");
        assert!(shown.is_ascii(), "got {shown}");
        assert!(shown.starts_with("https://xn--"), "got {shown}");
        assert_ne!(shown, "https://apple.com/id");
        assert_eq!(host_of(attack).as_deref(), Some("xn--pple-43d.com"));

        // And the real one is untouched.
        assert_eq!(
            normalise_url("https://apple.com/id").as_deref(),
            Some("https://apple.com/id")
        );
        // A host already in punycode is left as it is: it is already the ASCII form.
        assert_eq!(
            normalise_url("https://xn--bcher-kva.example/a").as_deref(),
            Some("https://xn--bcher-kva.example/a")
        );
    }

    /// The punycode encoder, against the vectors in RFC 3492 and the domains people quote.
    #[test]
    fn hosts_are_converted_to_ascii_the_way_a_browser_would() {
        // Cross-checked against a reference implementation, so a defect in the encoder shows
        // up here rather than as a host somebody trusts and should not.
        for (label, ascii) in [
            ("bücher", "xn--bcher-kva"),
            ("münchen", "xn--mnchen-3ya"),
            ("français", "xn--franais-xxa"),
            ("müller", "xn--mller-kva"),
            ("例子", "xn--fsqu00a"),
            ("测试", "xn--0zwm56d"),
            ("日本語", "xn--wgv71a119e"),
            ("한국", "xn--3e0b707e"),
            ("ยจฆฟคฏข", "xn--22cdfh1b8fsa"),
            ("\u{430}pple", "xn--pple-43d"),
        ] {
            assert_eq!(punycode_label(label).as_deref(), Some(ascii), "{label}");
        }
        assert_eq!(punycode_label("ascii").as_deref(), Some("ascii"));
        assert_eq!(
            punycode_label("xn--bcher-kva").as_deref(),
            Some("xn--bcher-kva"),
            "a label already in its ASCII form is not encoded a second time"
        );
        assert_eq!(
            to_ascii_host("Bücher.Example.COM").as_deref(),
            Some("xn--bcher-kva.example.com"),
            "the host is lower-cased as well, because case is not identity in a domain"
        );
    }

    /// A right-to-left override in a URL must not be able to reorder what the user reads.
    #[test]
    fn a_direction_override_in_a_url_is_escaped_rather_than_rendered() {
        let attack = "https://example.com/\u{202e}gpj.exe";
        let shown = normalise_url(attack).expect("it normalises");
        assert!(shown.is_ascii(), "got {shown}");
        assert!(shown.contains("%E2%80%AE"), "got {shown}");
        assert!(
            shown.chars().all(turn_pty::is_display_safe),
            "a URL shown to a user must not carry formatting characters: {shown}"
        );

        for hostile in [
            "https://example.com/\u{200b}hidden",
            "https://example.com/a\u{2028}b",
            "https://example.com/\u{feff}x",
            "https://example.com/\u{e0041}",
        ] {
            let shown = normalise_url(hostile).expect("it normalises");
            assert!(shown.is_ascii(), "got {shown}");
            assert!(shown.chars().all(turn_pty::is_display_safe), "got {shown}");
        }
    }

    #[test]
    fn an_escape_that_is_already_well_formed_is_not_escaped_twice() {
        assert_eq!(
            normalise_url("https://example.com/a%20b?q=%2F").as_deref(),
            Some("https://example.com/a%20b?q=%2F")
        );
        // A stray percent is escaped, because it is not an escape.
        assert_eq!(
            normalise_url("https://example.com/100%").as_deref(),
            Some("https://example.com/100%25")
        );
    }

    #[test]
    fn a_url_with_no_host_where_its_scheme_needs_one_is_refused() {
        assert!(normalise_url("https://").is_none());
        assert!(normalise_url("https:///path").is_none());
        assert!(normalise_url("mailto:").is_none());
        assert!(normalise_url("http").is_none());
        // `file://` with an empty authority means the local machine, which is legitimate.
        assert_eq!(
            normalise_url("file:///repo/x.md").as_deref(),
            Some("file:///repo/x.md")
        );
    }

    #[test]
    fn a_userinfo_and_a_port_survive_normalisation() {
        assert_eq!(
            normalise_url("ssh://git@GitHub.com:22/o/r.git").as_deref(),
            Some("ssh://git@github.com:22/o/r.git")
        );
        assert_eq!(
            normalise_url("http://[::1]:8080/x").as_deref(),
            Some("http://[::1]:8080/x")
        );
        assert_eq!(host_of("http://[::1]:8080/x").as_deref(), Some("[::1]"));
    }

    /// A screen a program filled with links must not make the map unbounded.
    #[test]
    fn the_number_of_links_on_one_screen_is_capped() {
        let line = "https://a.example/x ".repeat(6);
        let lines: Vec<&str> = (0..200).map(|_| line.as_str()).collect();
        let grid = Grid::from_lines(&lines, line.chars().count() as u16);
        let map = LinkMap::find(&grid, &mut NoPaths);
        assert!(
            map.len() <= MAX_LINKS_PER_SCREEN,
            "the map grew to {}",
            map.len()
        );
    }

    /// Resolving is the only part of this that touches the world, so it is both remembered
    /// and capped.
    #[test]
    fn path_resolution_is_remembered_and_bounded() {
        let mut paths = FsPaths::default();
        paths.set_cwd(Some(PathBuf::from(env!("CARGO_MANIFEST_DIR"))));
        paths.begin_scan(1_000);
        // A path that really is there, in this crate.
        assert!(paths.resolve("Cargo.toml").is_some());
        assert!(paths.resolve("src/terminal/links.rs").is_some());
        assert!(paths.resolve("src/does-not-exist.rs").is_none());

        // Asked again inside the memo window, the answers come back without a check.
        let before = paths.checks;
        assert!(paths.resolve("Cargo.toml").is_some());
        assert_eq!(paths.checks, before, "a remembered answer costs nothing");

        // Past the window it is checked again.
        paths.begin_scan(1_000 + PATH_MEMO_MS);
        assert!(paths.resolve("Cargo.toml").is_some());
        assert_eq!(paths.checks, 1);

        // And a scan cannot make more than its budget of calls.
        paths.begin_scan(2_000_000);
        for index in 0..(MAX_PATH_CHECKS_PER_SCAN * 2) {
            let _ = paths.resolve(&format!("candidate-{index}"));
        }
        assert_eq!(paths.checks, MAX_PATH_CHECKS_PER_SCAN);

        // Changing the working directory forgets everything: the same relative path is a
        // different file.
        paths.set_cwd(Some(PathBuf::from("/")));
        paths.begin_scan(3_000_000);
        assert!(paths.resolve("Cargo.toml").is_none());
    }

    #[test]
    fn an_absolute_path_resolves_without_a_working_directory() {
        let mut paths = FsPaths::default();
        paths.begin_scan(0);
        assert!(
            paths.resolve("src/terminal/links.rs").is_none(),
            "a relative path with no cwd resolves to nothing rather than to the process's own"
        );
        let absolute = format!("{}/Cargo.toml", env!("CARGO_MANIFEST_DIR"));
        assert!(paths.resolve(&absolute).is_some());
    }

    #[test]
    fn the_target_is_shown_at_once_with_the_modifier_and_after_a_pause_without_it() {
        assert!(target_visible(1_000, 1_000, true), "the modifier means now");
        assert!(!target_visible(1_000, 1_000, false));
        assert!(!target_visible(1_000, 1_000 + HOVER_DELAY_MS - 1, false));
        assert!(target_visible(1_000, 1_000 + HOVER_DELAY_MS, false));
        // A clock that went backwards must not make the target appear.
        assert!(!target_visible(5_000, 1_000, false));
    }

    /// A wide glyph is two columns, and a link over one must cover both or the highlight ends
    /// inside a character.
    #[test]
    fn a_link_over_a_double_width_glyph_covers_both_of_its_columns() {
        let mut grid = Grid::blank(1, 30);
        for (col, ch) in "https://a.example/".chars().enumerate() {
            if let Some(cell) = grid.cell_mut(0, col as u16) {
                cell.text = ch.to_string();
            }
        }
        assert!(grid.set_wide(0, 18, "漢"));
        let map = LinkMap::find(&grid, &mut NoPaths);
        let link = map.links().first().expect("a link");
        assert!(link.covers(0, 18));
        assert!(
            link.covers(0, 19),
            "the trailing half of the glyph is part of the link"
        );
    }

    /// The line the pane actually reads: rows joined where they wrapped, padding gone.
    #[test]
    fn logical_lines_join_wrapped_rows_and_drop_the_padding() {
        let mut grid = Grid::blank(3, 6);
        for (index, ch) in "abcdefghi".chars().enumerate() {
            let (row, col) = ((index / 6) as u16, (index % 6) as u16);
            if let Some(cell) = grid.cell_mut(row, col) {
                cell.text = ch.to_string();
            }
        }
        assert!(grid.set_row_wrapped(0, true));
        let lines = logical_lines(&grid);
        let texts: Vec<String> = lines.iter().map(LogicalLine::text).collect();
        assert_eq!(
            texts,
            vec!["abcdefghi".to_string(), String::new()],
            "rows 0 and 1 are one line and row 2 is another"
        );
        assert_eq!(lines[0].at[6], (1, 0), "the seventh character is on row 1");
    }
}
