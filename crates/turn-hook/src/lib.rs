//! The `turn-hook` helper: forwards one agent callback to Turn and gets out of
//! the way.
//!
//! Agents that cannot POST over HTTP themselves run a command instead. Codex's
//! hook handlers and its `notify` mechanism are both command-based, so this is the
//! only way Turn hears from Codex at all.
//!
//! It exists because of one requirement that overrides every other: **a broken
//! helper must never break the user's agent session.** Whatever happens — no URL,
//! no daemon listening, an unreadable payload, a refused connection — the process
//! exits 0 and prints nothing. An agent that treats a non-zero hook exit as a
//! failure must never see one because of Turn.
//!
//! Two payload conventions are supported, because Codex uses both — verified by
//! recording what a real handler received from codex-cli 0.146.0:
//!
//! * **stdin**, for Codex's hook handlers and Claude Code's `command` hooks. A
//!   Codex hook handler's argv is *empty*: the handler `args` array its config
//!   accepts is parsed and then silently ignored, so stdin is the only channel.
//! * **argv**, for Codex's `notify` — the program is invoked with the event JSON
//!   appended as one final argument, after whatever the config listed.
//!
//! The destination comes from `--url`, or from `TURN_HOOK_URL`. In practice it is
//! always the environment variable: a Codex hook handler cannot be given
//! arguments at all, and for `notify` the URL is deliberately kept out of argv
//! because it carries the node's token and argv is world-readable on Linux. Codex
//! passes its own environment to both, checked by reading `TURN_HOOK_URL` back out
//! of a live hook handler and a live notify invocation.
//!
//! One consequence of how Codex invokes a `command` handler: it goes through a
//! shell, so the helper's path is quoted by the adapter before it ever gets here.
//! Nothing in this crate can compensate for that, which is why the quoting lives
//! next to the config that produces it.
//!
//! Only `http://` is supported: the target is always a loopback port on this
//! machine, so there is nothing for TLS to protect and no certificate story to get
//! wrong.
//!
//! **And only loopback.** This binary runs inside the agent's process tree, which
//! means its environment is whatever the agent inherited — including anything a
//! repository's `direnv`, `.envrc` or task runner put there. A `TURN_HOOK_URL`
//! pointing somewhere else would turn Turn's own helper into a tidy exfiltration
//! channel for hook payloads, which carry prompts and assistant messages. A
//! destination that is not a loopback address is refused, and since Turn only ever
//! configures `127.0.0.1`, refusing costs nothing. The refusal is applied twice:
//! once to the host as it was written, which is why a name other than `localhost`
//! is never resolved at all, and once to the address the resolver answered with,
//! because `localhost` is a name and `/etc/hosts` is not Turn's to trust.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Default socket timeout. Short: the destination is a local port and Turn's
/// handler answers immediately, so anything slower than this means the daemon is
/// gone and waiting would only delay the agent.
pub const DEFAULT_TIMEOUT_MS: u64 = 2_000;

/// Largest payload forwarded. Matches the hook server's own body limit, so an
/// oversized payload is dropped here rather than being sent to be refused.
pub const MAX_PAYLOAD_BYTES: usize = 256 * 1024;

/// What the helper was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// Where to post. `None` means nothing can be done, which is not an error.
    pub url: Option<String>,
    /// A payload supplied on the command line, as Codex's `notify` does.
    pub inline_payload: Option<String>,
    pub timeout: Duration,
    /// Whether to explain failures on stderr. Off unless `TURN_HOOK_DEBUG` is set,
    /// because stderr from a hook lands in the middle of the user's agent output.
    pub debug: bool,
}

/// Invocation details for Claude Code's status-line fan-out mode.
pub struct StatusLineOptions {
    /// Authenticated loopback destination. `None` still runs the user's command.
    pub url: Option<String>,
    /// Private script containing the user's effective status-line command.
    /// `None` selects Turn's compact fallback renderer.
    pub original_script: Option<PathBuf>,
    /// This binary, used to detach the best-effort network forward from the
    /// command whose stdout/stderr/exit Claude Code observes.
    pub forwarder_exe: PathBuf,
    pub timeout: Duration,
    pub debug: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            url: None,
            inline_payload: None,
            timeout: Duration::from_millis(DEFAULT_TIMEOUT_MS),
            debug: false,
        }
    }
}

impl Options {
    /// Parses arguments, with the environment as fallback.
    ///
    /// `args` must not include the program name. Unrecognised flags are ignored
    /// rather than rejected: a future Codex release adding an argument of its own
    /// must not turn every callback into a silent no-op.
    pub fn parse<I, S>(args: I, env_url: Option<String>, env_debug: bool) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut options = Options {
            url: env_url.filter(|u| !u.trim().is_empty()),
            debug: env_debug,
            ..Options::default()
        };

        let args: Vec<String> = args.into_iter().map(Into::into).collect();
        let mut index = 0;
        while index < args.len() {
            let arg = args[index].as_str();
            match arg {
                "--url" | "-u" => {
                    if let Some(value) = args.get(index + 1) {
                        options.url = Some(value.clone());
                        index += 1;
                    }
                }
                "--timeout-ms" => {
                    if let Some(value) = args.get(index + 1) {
                        if let Ok(ms) = value.parse::<u64>() {
                            options.timeout = Duration::from_millis(ms.clamp(1, 30_000));
                        }
                        index += 1;
                    }
                }
                "--debug" => options.debug = true,
                _ => {
                    if let Some(value) = arg.strip_prefix("--url=") {
                        options.url = Some(value.to_string());
                    } else if arg.starts_with('-') {
                        // An unknown flag. Skipped, never fatal.
                    } else if options.inline_payload.is_none() {
                        // The first positional argument is the payload. This is
                        // how Codex's `notify` hands over its event JSON.
                        options.inline_payload = Some(arg.to_string());
                    }
                }
            }
            index += 1;
        }

        options
    }

    /// Reads the options as the process actually sees them.
    pub fn from_process() -> Self {
        Self::parse(
            std::env::args().skip(1),
            std::env::var("TURN_HOOK_URL").ok(),
            std::env::var_os("TURN_HOOK_DEBUG").is_some(),
        )
    }
}

/// Why a forward did not happen. Never surfaced to the agent as an exit code.
#[derive(Debug, PartialEq, Eq)]
pub enum Failure {
    /// Neither `--url` nor `TURN_HOOK_URL` was given.
    NoUrl,
    /// The URL was not a usable `http://host:port/path`.
    BadUrl(String),
    /// Nothing arrived on stdin and no payload was passed as an argument.
    NoPayload,
    /// The payload was larger than the hook server would accept anyway.
    PayloadTooLarge(usize),
    /// The host resolved to an address that is not this machine. Carries the
    /// address that was offered, which is the only useful thing to say about it.
    NotLoopback(String),
    /// The socket could not be used.
    Transport(String),
    /// The private status-line delegate could not be launched. The original
    /// command itself is deliberately absent from this diagnostic.
    StatusLineCommand,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::NoUrl => write!(f, "no --url and no TURN_HOOK_URL"),
            Failure::BadUrl(url) => write!(
                f,
                "unusable url (expected http:// on a loopback address): {}",
                // Masked: this message can be printed to the agent's stderr, which
                // lands in the user's terminal and in the agent's own transcript.
                // The last path segment is the session's token.
                mask_token(url)
            ),
            Failure::NoPayload => write!(f, "no payload on stdin or in the arguments"),
            Failure::PayloadTooLarge(size) => write!(f, "payload of {size} bytes is too large"),
            Failure::NotLoopback(address) => write!(
                f,
                "the destination resolved to {address}, which is not this machine"
            ),
            Failure::Transport(error) => write!(f, "{error}"),
            Failure::StatusLineCommand => write!(f, "could not run the preserved status line"),
        }
    }
}

/// Replaces the last path segment of a URL, which is where the token lives.
///
/// Deliberately crude: this only exists so a debug line is readable, and anything
/// that tried to be clever about parsing here could end up printing the secret it
/// was meant to hide.
pub fn mask_token(url: &str) -> String {
    match url.rsplit_once('/') {
        Some((head, tail)) if !tail.is_empty() => format!("{head}/<token>"),
        _ => url.to_string(),
    }
}

/// Whether a host is this machine, and only this machine.
///
/// A name is accepted only if it is literally `localhost`: resolving names and
/// then checking the address would let a DNS answer decide, and refusing to
/// resolve at all is the property we want.
pub fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(address) => address.is_loopback(),
        Err(_) => false,
    }
}

/// A parsed destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub host: String,
    pub port: u16,
    /// Request target, always starting with `/`.
    pub path: String,
}

impl Target {
    /// Parses `http://host[:port]/path`.
    ///
    /// Hand-rolled because the shape is fixed and a URL crate would be the single
    /// heaviest thing in this binary. `https` is refused rather than silently
    /// downgraded — Turn never configures one, so an https URL means something is
    /// wrong and quietly posting in the clear would be worse than not posting.
    pub fn parse(url: &str) -> Option<Self> {
        let rest = url.trim().strip_prefix("http://")?;
        let (authority, path) = match rest.find('/') {
            Some(index) => (&rest[..index], &rest[index..]),
            None => (rest, "/"),
        };
        if authority.is_empty() {
            return None;
        }

        // An IPv6 literal keeps its brackets, so the port is only whatever follows
        // the closing one.
        let (host, port) = match authority.rfind(']') {
            Some(bracket) => match authority[bracket + 1..].strip_prefix(':') {
                Some(port) => (&authority[..=bracket], port.parse::<u16>().ok()?),
                None => (authority, 80),
            },
            None => match authority.rsplit_once(':') {
                Some((host, port)) => (host, port.parse::<u16>().ok()?),
                None => (authority, 80),
            },
        };
        if host.is_empty() || port == 0 {
            return None;
        }
        // The choke point for "where may a hook payload go". Everything else in
        // this binary is about not bothering the agent; this is the one refusal
        // that matters more than delivering the event.
        if !is_loopback_host(host) {
            return None;
        }

        Some(Self {
            host: host.to_string(),
            port,
            path: path.to_string(),
        })
    }

    /// The bytes of a minimal HTTP/1.1 POST.
    ///
    /// `Connection: close` so neither side has to think about keep-alive, and an
    /// explicit `Content-Length` so the server never waits for more.
    pub fn request(&self, body: &[u8]) -> Vec<u8> {
        let head = format!(
            "POST {} HTTP/1.1\r\n\
             Host: {}:{}\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             User-Agent: turn-hook\r\n\
             Connection: close\r\n\r\n",
            self.path,
            self.host,
            self.port,
            body.len()
        );
        let mut request = head.into_bytes();
        request.extend_from_slice(body);
        request
    }
}

/// Reads the payload: the inline argument if there is one, otherwise stdin.
///
/// Reading is capped, so a runaway producer on the other end of the pipe cannot
/// make the helper grow without bound.
pub fn read_payload(options: &Options, stdin: &mut impl Read) -> Result<Vec<u8>, Failure> {
    if let Some(inline) = &options.inline_payload {
        let bytes = inline.as_bytes();
        if bytes.len() > MAX_PAYLOAD_BYTES {
            return Err(Failure::PayloadTooLarge(bytes.len()));
        }
        if inline.trim().is_empty() {
            return Err(Failure::NoPayload);
        }
        return Ok(bytes.to_vec());
    }

    let mut buffer = Vec::new();
    let mut limited = stdin.take(MAX_PAYLOAD_BYTES as u64 + 1);
    limited
        .read_to_end(&mut buffer)
        .map_err(|error| Failure::Transport(error.to_string()))?;
    if buffer.len() > MAX_PAYLOAD_BYTES {
        return Err(Failure::PayloadTooLarge(buffer.len()));
    }
    if buffer.iter().all(|byte| byte.is_ascii_whitespace()) {
        return Err(Failure::NoPayload);
    }
    Ok(buffer)
}

/// Resolves a destination and keeps only the addresses that are this machine.
///
/// [`Target::parse`] refuses a host that is not loopback, but the one *name* it
/// accepts is still a name: `localhost` goes through the system resolver, and
/// `/etc/hosts` belongs to the machine rather than to Turn. A single line there
/// would answer with a routable address, and this helper would hand it the node's
/// token together with the user's prompt and the agent's reply. So the address
/// that is about to be connected to is what gets checked — the spelling it
/// arrived as has already had its say.
pub fn loopback_addresses(host: &str, port: u16) -> Result<Vec<SocketAddr>, Failure> {
    let resolved: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|error| Failure::Transport(error.to_string()))?
        .collect();

    let loopback: Vec<SocketAddr> = resolved
        .iter()
        .copied()
        .filter(|address| address.ip().is_loopback())
        .collect();
    if loopback.is_empty() {
        return Err(match resolved.first() {
            Some(address) => Failure::NotLoopback(address.ip().to_string()),
            None => Failure::Transport("no address to connect to".to_string()),
        });
    }
    Ok(loopback)
}

/// Posts a payload and returns once the server has acknowledged it.
///
/// The response is read but not interpreted. Turn's server never answers with a
/// hook decision, and this helper would not act on one if it did: deciding
/// anything on the user's behalf is not its job.
pub fn post(target: &Target, body: &[u8], timeout: Duration) -> Result<(), Failure> {
    let addresses = loopback_addresses(&target.host, target.port)?;

    let mut last = Failure::Transport("no address to connect to".to_string());
    for address in addresses {
        match TcpStream::connect_timeout(&address, timeout) {
            Ok(mut stream) => {
                let _ = stream.set_write_timeout(Some(timeout));
                let _ = stream.set_read_timeout(Some(timeout));
                let _ = stream.set_nodelay(true);

                if let Err(error) = stream.write_all(&target.request(body)) {
                    last = Failure::Transport(error.to_string());
                    continue;
                }
                let _ = stream.flush();

                // Read just enough to know the server took it, then drop the
                // connection. The body is irrelevant and may be empty.
                let mut acknowledgement = [0u8; 64];
                let _ = stream.read(&mut acknowledgement);
                return Ok(());
            }
            Err(error) => last = Failure::Transport(error.to_string()),
        }
    }
    Err(last)
}

/// The whole helper: parse, read, post. Errors are returned, never raised.
pub fn run(options: &Options, stdin: &mut impl Read) -> Result<(), Failure> {
    let url = options.url.as_deref().ok_or(Failure::NoUrl)?;
    let target = Target::parse(url).ok_or_else(|| Failure::BadUrl(url.to_string()))?;
    let payload = read_payload(options, stdin)?;
    post(&target, &payload, options.timeout)
}

/// Reads Claude's status JSON once, starts a detached best-effort forward, then
/// supplies the exact same bytes to the user's original command. Network latency
/// is never on the command's critical path; only a bounded in-memory pipe write
/// occurs before delegation.
pub fn run_status_line(
    options: &StatusLineOptions,
    stdin: &mut impl Read,
    stdout: &mut impl Write,
) -> Result<i32, Failure> {
    let read_options = Options {
        url: options.url.clone(),
        timeout: options.timeout,
        debug: options.debug,
        ..Options::default()
    };
    let payload = read_payload(&read_options, stdin)?;
    spawn_status_line_forwarder(options, &payload);

    let Some(script) = options.original_script.as_deref() else {
        stdout
            .write_all(compact_status_line(&payload).as_bytes())
            .map_err(|error| Failure::Transport(error.to_string()))?;
        stdout
            .write_all(b"\n")
            .map_err(|error| Failure::Transport(error.to_string()))?;
        return Ok(0);
    };

    #[cfg(unix)]
    let mut command = {
        let mut command = Command::new("/bin/sh");
        command.arg(script);
        command
    };
    #[cfg(windows)]
    let mut command = {
        let mut command = Command::new("cmd.exe");
        command.arg("/D").arg("/S").arg("/C").arg(script);
        command
    };
    #[cfg(not(any(unix, windows)))]
    return Err(Failure::StatusLineCommand);

    #[cfg(any(unix, windows))]
    {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|_| Failure::StatusLineCommand)?;
        let child_stdin = child.stdin.take().ok_or(Failure::StatusLineCommand)?;
        let writer_payload = payload.clone();
        let writer = std::thread::spawn(move || {
            let mut child_stdin = child_stdin;
            let _ = child_stdin.write_all(&writer_payload);
        });
        let status = child.wait().map_err(|_| Failure::StatusLineCommand)?;
        let _ = writer.join();
        Ok(status.code().unwrap_or(1))
    }
}

/// Spawn a second helper process whose only job is to POST the payload. Its
/// stdin is filled before it performs any network I/O, so writing the bounded
/// payload cannot wait on the daemon.
fn spawn_status_line_forwarder(options: &StatusLineOptions, payload: &[u8]) {
    let Some(url) = options.url.as_deref().filter(|url| !url.trim().is_empty()) else {
        return;
    };
    let mut child = match Command::new(&options.forwarder_exe)
        .arg("--statusline-forward")
        .env("TURN_STATUSLINE_URL", url)
        .env(
            "TURN_STATUSLINE_TIMEOUT_MS",
            options.timeout.as_millis().to_string(),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return,
    };
    if let Some(mut pipe) = child.stdin.take() {
        let payload = payload.to_vec();
        // Do not put child scheduling or pipe pressure on Claude's status-line
        // path. The payload is already bounded, and this disposable writer owns
        // the pipe until either the forwarder reads it or exits.
        let _ = std::thread::Builder::new()
            .name("turn-statusline-forward".into())
            .spawn(move || {
                let _ = pipe.write_all(&payload);
            });
    }
    // Intentionally not waited: the original status line owns stdout, stderr,
    // exit and timeout. The network forward is disposable telemetry.
}

/// Useful, compact output when the operator had no status line of their own.
/// Text fields are reduced to printable one-line ASCII before rendering.
pub fn compact_status_line(payload: &[u8]) -> String {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(payload) else {
        return "TURN · Claude telemetry active".into();
    };
    let model = value
        .pointer("/model/display_name")
        .and_then(serde_json::Value::as_str)
        .and_then(compact_field)
        .or_else(|| {
            value
                .pointer("/model/id")
                .and_then(serde_json::Value::as_str)
                .and_then(compact_field)
        })
        .unwrap_or_else(|| "Claude".into());
    let mut parts = vec![format!("TURN · {model}")];
    if let Some(used) = json_percentage(value.pointer("/context_window/used_percentage")) {
        parts.push(format!("ctx {used:.0}% used"));
    }
    if let Some(used) = json_percentage(value.pointer("/rate_limits/five_hour/used_percentage")) {
        parts.push(format!("5h {:.0}% left", 100.0 - used));
    }
    if let Some(used) = json_percentage(value.pointer("/rate_limits/seven_day/used_percentage")) {
        parts.push(format!("7d {:.0}% left", 100.0 - used));
    }
    parts.join(" · ")
}

fn compact_field(value: &str) -> Option<String> {
    let clean = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, ' ' | '-' | '_' | '.') {
                character
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let clean: String = clean.chars().take(80).collect();
    (!clean.is_empty()).then_some(clean)
}

fn json_percentage(value: Option<&serde_json::Value>) -> Option<f64> {
    value?
        .as_f64()
        .filter(|number| number.is_finite() && (0.0..=100.0).contains(number))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::sync::mpsc;

    fn parse(args: &[&str]) -> Options {
        Options::parse(args.iter().map(|a| a.to_string()), None, false)
    }

    #[test]
    fn the_url_can_come_from_a_flag_or_the_environment() {
        assert_eq!(
            parse(&["--url", "http://127.0.0.1:9/hook/t"])
                .url
                .as_deref(),
            Some("http://127.0.0.1:9/hook/t")
        );
        assert_eq!(
            parse(&["-u", "http://127.0.0.1:9/hook/t"]).url.as_deref(),
            Some("http://127.0.0.1:9/hook/t")
        );
        assert_eq!(
            parse(&["--url=http://127.0.0.1:9/hook/t"]).url.as_deref(),
            Some("http://127.0.0.1:9/hook/t")
        );

        // The environment is the fallback, because a Codex hook handler cannot
        // carry arguments.
        let from_env = Options::parse(
            Vec::<String>::new(),
            Some("http://127.0.0.1:9/hook/env".to_string()),
            false,
        );
        assert_eq!(from_env.url.as_deref(), Some("http://127.0.0.1:9/hook/env"));

        // An explicit flag wins over the environment.
        let both = Options::parse(
            ["--url", "http://127.0.0.1:9/hook/flag"].map(String::from),
            Some("http://127.0.0.1:9/hook/env".to_string()),
            false,
        );
        assert_eq!(both.url.as_deref(), Some("http://127.0.0.1:9/hook/flag"));

        // An empty environment variable is not a URL.
        assert!(
            Options::parse(Vec::<String>::new(), Some("  ".into()), false)
                .url
                .is_none()
        );
    }

    /// Codex's `notify` appends the event JSON as a final argument.
    #[test]
    fn a_positional_argument_is_taken_as_the_payload() {
        let options = parse(&[
            "--url",
            "http://127.0.0.1:9/hook/t",
            r#"{"type":"agent-turn-complete","thread-id":"th_1"}"#,
        ]);
        assert_eq!(
            options.inline_payload.as_deref(),
            Some(r#"{"type":"agent-turn-complete","thread-id":"th_1"}"#)
        );

        // The URL's own value must not be mistaken for the payload.
        assert_eq!(options.url.as_deref(), Some("http://127.0.0.1:9/hook/t"));
    }

    /// The two invocations Codex really performs, recorded off codex-cli 0.146.0
    /// by having it run a script that logged its argv, stdin and environment.
    ///
    /// They are opposites, and the helper has to serve both without being told
    /// which it is: a hook handler gets no arguments and a body on stdin, `notify`
    /// gets a body in argv and nothing on stdin.
    #[test]
    fn both_shapes_codex_actually_invokes_the_helper_with_are_handled() {
        // 1. A hook handler. Codex ran the command with an empty argv — the
        //    handler `args` array its config accepts is silently ignored — and put
        //    the payload on stdin. The URL can only come from the environment.
        let handler = Options::parse(
            Vec::<String>::new(),
            Some("http://127.0.0.1:51257/hook/tok_e2e".to_string()),
            false,
        );
        assert_eq!(
            handler.url.as_deref(),
            Some("http://127.0.0.1:51257/hook/tok_e2e")
        );
        assert!(
            handler.inline_payload.is_none(),
            "a hook handler receives no arguments at all"
        );
        let recorded = br#"{"session_id":"019fcdb2-c194-7d10-810f-13075a093cab","cwd":"/repo","hook_event_name":"SessionStart","model":"gpt-5.6-sol","permission_mode":"bypassPermissions","source":"startup"}"#;
        let mut stdin = std::io::Cursor::new(recorded.to_vec());
        assert_eq!(
            read_payload(&handler, &mut stdin).unwrap(),
            recorded.to_vec()
        );

        // 2. `notify`. Codex appended the payload as the single argument, and this
        //    one uses hyphenated keys rather than snake_case ones.
        let recorded = r#"{"type":"agent-turn-complete","thread-id":"019fcdb3-60d8-7733-83a8-813720d5c490","turn-id":"019fcdb3-6134-7122-9708-cd6f4f9f0718","cwd":"/repo","client":"codex_exec","input-messages":["Reply with exactly: OK"],"last-assistant-message":"OK"}"#;
        let notify = Options::parse(
            [recorded.to_string()],
            Some("http://127.0.0.1:51257/hook/tok_e2e".to_string()),
            false,
        );
        assert_eq!(notify.inline_payload.as_deref(), Some(recorded));
        assert_eq!(
            read_payload(&notify, &mut std::io::empty()).unwrap(),
            recorded.as_bytes().to_vec(),
            "and nothing on stdin is not a problem when the payload is in argv"
        );
    }

    /// A new Codex release adding a flag of its own must not silence the helper.
    #[test]
    fn unknown_flags_are_ignored_rather_than_fatal() {
        let options = parse(&[
            "--some-future-flag",
            "--url",
            "http://127.0.0.1:9/hook/t",
            "{}",
        ]);
        assert_eq!(options.url.as_deref(), Some("http://127.0.0.1:9/hook/t"));
        assert_eq!(options.inline_payload.as_deref(), Some("{}"));
    }

    #[test]
    fn a_dangling_url_flag_with_no_value_does_not_panic() {
        let options = parse(&["--url"]);
        assert!(options.url.is_none());
        let options = parse(&["--timeout-ms"]);
        assert_eq!(options.timeout, Duration::from_millis(DEFAULT_TIMEOUT_MS));
    }

    #[test]
    fn the_timeout_is_configurable_and_clamped_to_something_sane() {
        assert_eq!(
            parse(&["--timeout-ms", "500"]).timeout,
            Duration::from_millis(500)
        );
        assert_eq!(
            parse(&["--timeout-ms", "0"]).timeout,
            Duration::from_millis(1),
            "a zero timeout would fail every connection"
        );
        assert_eq!(
            parse(&["--timeout-ms", "999999"]).timeout,
            Duration::from_millis(30_000),
            "a hook must never hang the agent for minutes"
        );
        assert_eq!(
            parse(&["--timeout-ms", "not a number"]).timeout,
            Duration::from_millis(DEFAULT_TIMEOUT_MS)
        );
    }

    #[test]
    fn stdin_is_read_when_no_payload_was_passed_as_an_argument() {
        let options = parse(&["--url", "http://127.0.0.1:9/hook/t"]);
        let mut stdin = std::io::Cursor::new(br#"{"hook_event_name":"Stop"}"#.to_vec());
        assert_eq!(
            read_payload(&options, &mut stdin).unwrap(),
            br#"{"hook_event_name":"Stop"}"#.to_vec()
        );
    }

    #[test]
    fn an_inline_payload_takes_precedence_over_stdin() {
        let options = parse(&["--url", "http://127.0.0.1:9/hook/t", "{\"from\":\"argv\"}"]);
        let mut stdin = std::io::Cursor::new(b"{\"from\":\"stdin\"}".to_vec());
        assert_eq!(
            read_payload(&options, &mut stdin).unwrap(),
            b"{\"from\":\"argv\"}".to_vec()
        );
    }

    #[test]
    fn an_empty_stdin_is_reported_rather_than_posted_as_nothing() {
        let options = parse(&["--url", "http://127.0.0.1:9/hook/t"]);
        let mut empty = std::io::Cursor::new(Vec::new());
        assert_eq!(read_payload(&options, &mut empty), Err(Failure::NoPayload));

        let mut whitespace = std::io::Cursor::new(b"\n\t  \n".to_vec());
        assert_eq!(
            read_payload(&options, &mut whitespace),
            Err(Failure::NoPayload)
        );
    }

    /// A runaway producer on the pipe must not make the helper grow without
    /// bound, and must not send something the server would only refuse.
    #[test]
    fn an_oversized_payload_is_dropped_before_it_is_read_into_memory() {
        let options = parse(&["--url", "http://127.0.0.1:9/hook/t"]);
        let huge = vec![b'x'; MAX_PAYLOAD_BYTES + 1];
        let mut stdin = std::io::Cursor::new(huge);
        assert!(matches!(
            read_payload(&options, &mut stdin),
            Err(Failure::PayloadTooLarge(_))
        ));

        let inline = "y".repeat(MAX_PAYLOAD_BYTES + 1);
        let options = Options {
            inline_payload: Some(inline),
            ..parse(&["--url", "http://127.0.0.1:9/hook/t"])
        };
        assert!(matches!(
            read_payload(&options, &mut std::io::empty()),
            Err(Failure::PayloadTooLarge(_))
        ));
    }

    #[test]
    fn urls_are_parsed_into_a_host_port_and_request_target() {
        assert_eq!(
            Target::parse("http://127.0.0.1:51234/hook/abc"),
            Some(Target {
                host: "127.0.0.1".into(),
                port: 51_234,
                path: "/hook/abc".into()
            })
        );
        // No path given: the request target is still valid.
        assert_eq!(
            Target::parse("http://127.0.0.1:51234").map(|t| t.path),
            Some("/".to_string())
        );
        // No port given: HTTP's default.
        assert_eq!(
            Target::parse("http://localhost/hook/abc").map(|t| t.port),
            Some(80)
        );
    }

    #[test]
    fn an_unusable_url_is_refused_rather_than_guessed_at() {
        for url in [
            // Never silently downgraded to plaintext.
            "https://127.0.0.1:51234/hook/abc",
            "127.0.0.1:51234/hook/abc",
            "http://",
            "http:///hook/abc",
            "http://127.0.0.1:0/hook/abc",
            "http://127.0.0.1:99999/hook/abc",
            "http://127.0.0.1:notaport/hook",
            "",
            "nonsense",
        ] {
            assert_eq!(Target::parse(url), None, "{url:?} must not parse");
        }
    }

    /// The helper runs with the agent's environment, so `TURN_HOOK_URL` is not
    /// entirely Turn's to control — a repository's `direnv` sets variables too.
    /// A destination that is not this machine must be refused, because the payload
    /// it would carry contains the user's prompt and the agent's reply.
    #[test]
    fn a_destination_that_is_not_loopback_is_refused_so_the_helper_cannot_exfiltrate() {
        for hostile in [
            "http://evil.example/hook/abc",
            "http://169.254.169.254/latest/meta-data",
            "http://10.0.0.5:51234/hook/abc",
            "http://192.168.1.10/hook/abc",
            "http://8.8.8.8/hook/abc",
            // A name that only *looks* local.
            "http://localhost.evil.example/hook/abc",
            "http://127.0.0.1.evil.example/hook/abc",
            // Decimal and hex spellings of a routable address.
            "http://2130706433/hook/abc",
            "http://0x7f000001/hook/abc",
        ] {
            assert_eq!(Target::parse(hostile), None, "{hostile:?} must be refused");
        }

        // And every spelling of this machine still works.
        for local in [
            "http://127.0.0.1:51234/hook/abc",
            "http://127.1.2.3:51234/hook/abc",
            "http://localhost:51234/hook/abc",
            "http://LOCALHOST:51234/hook/abc",
            "http://[::1]:51234/hook/abc",
        ] {
            assert!(Target::parse(local).is_some(), "{local:?} must be accepted");
        }

        // The refusal reaches `run` rather than being silently skipped.
        assert_eq!(
            run(
                &Options {
                    url: Some("http://evil.example/hook/tok_secret".into()),
                    inline_payload: Some("{}".into()),
                    ..Options::default()
                },
                &mut std::io::empty()
            ),
            Err(Failure::BadUrl(
                "http://evil.example/hook/tok_secret".into()
            ))
        );
    }

    /// The other half of the same refusal, and the half a parsed URL cannot give
    /// you: `localhost` is accepted as a name, so what it resolves to decides
    /// where the token goes. A `/etc/hosts` line — or a resolver a repository's
    /// tooling pointed elsewhere — must not be able to answer with a routable
    /// address and be believed.
    ///
    /// A `Target` is built by hand here because that is exactly what a hostile
    /// answer amounts to: a destination that passed the name check and then
    /// resolved somewhere else.
    #[test]
    fn a_resolved_address_that_is_not_this_machine_is_refused_before_any_connection() {
        let target = Target {
            host: "192.0.2.1".into(),
            port: 51_234,
            path: "/hook/tok_verysecret".into(),
        };
        let failure = post(
            &target,
            br#"{"prompt":"private"}"#,
            Duration::from_millis(50),
        )
        .expect_err("a non-loopback address must not be posted to");
        assert_eq!(
            failure,
            Failure::NotLoopback("192.0.2.1".into()),
            "the address must be refused rather than dialled and timed out"
        );
        assert!(
            !failure.to_string().contains("tok_verysecret"),
            "got {failure}"
        );

        // And the same check, applied to what a resolver returned.
        assert_eq!(
            loopback_addresses("192.0.2.1", 51_234),
            Err(Failure::NotLoopback("192.0.2.1".into()))
        );
    }

    /// The refusal must not cost the working case: `localhost` resolves to more
    /// than one address on a dual-stack machine, and the loopback ones are still
    /// tried in turn until one answers.
    #[test]
    fn localhost_still_reaches_a_server_listening_on_this_machine() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("must bind loopback");
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();

        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("a connection must arrive");
            let mut request = [0u8; 512];
            let read = stream.read(&mut request).unwrap_or(0);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
            tx.send(String::from_utf8_lossy(&request[..read]).into_owned())
                .unwrap();
        });

        let addresses = loopback_addresses("localhost", port).expect("localhost must resolve");
        assert!(
            addresses.iter().all(|address| address.ip().is_loopback()),
            "got {addresses:?}"
        );

        let target = Target::parse(&format!("http://localhost:{port}/hook/tok_local")).unwrap();
        post(
            &target,
            br#"{"hook_event_name":"Stop"}"#,
            Duration::from_secs(2),
        )
        .expect("a loopback name must still be posted to");

        let request = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the server must have received the payload");
        assert!(
            request.starts_with("POST /hook/tok_local HTTP/1.1\r\n"),
            "{request}"
        );
        server.join().unwrap();
    }

    /// A debug line lands in the user's terminal and in the agent's transcript.
    /// It must not carry the session's token.
    #[test]
    fn a_reported_failure_never_prints_the_token() {
        let failure = Failure::BadUrl("http://evil.example/hook/tok_verysecret".into());
        let message = failure.to_string();
        assert!(!message.contains("tok_verysecret"), "got {message}");
        assert!(message.contains("<token>"), "got {message}");
        assert_eq!(
            mask_token("http://127.0.0.1:51234/hook/abc"),
            "http://127.0.0.1:51234/hook/<token>"
        );
        assert_eq!(mask_token("nonsense"), "nonsense");
    }

    #[test]
    fn the_request_is_a_well_formed_post_with_an_explicit_length() {
        let target = Target::parse("http://127.0.0.1:51234/hook/abc").unwrap();
        let request = String::from_utf8(target.request(br#"{"a":1}"#)).unwrap();

        assert!(request.starts_with("POST /hook/abc HTTP/1.1\r\n"));
        assert!(request.contains("Host: 127.0.0.1:51234\r\n"));
        assert!(request.contains("Content-Type: application/json\r\n"));
        assert!(request.contains("Content-Length: 7\r\n"));
        assert!(request.contains("Connection: close\r\n"));
        assert!(request.ends_with("\r\n\r\n{\"a\":1}"));
    }

    /// The real thing, against a real socket: the payload arrives byte for byte.
    #[test]
    fn a_payload_reaches_a_listening_server_over_a_real_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("must bind loopback");
        let address = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel();

        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("a connection must arrive");
            let mut reader = BufReader::new(&stream);
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();

            let mut length = 0usize;
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                if header == "\r\n" || header.is_empty() {
                    break;
                }
                if let Some(value) = header.to_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; length];
            reader.read_exact(&mut body).unwrap();

            let mut stream = stream;
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
            tx.send((request_line, String::from_utf8_lossy(&body).into_owned()))
                .unwrap();
        });

        let url = format!("http://{address}/hook/tok_real");
        let options = Options::parse(
            ["--url", url.as_str(), r#"{"type":"agent-turn-complete"}"#].map(String::from),
            None,
            false,
        );
        run(&options, &mut std::io::empty()).expect("the post must succeed");

        let (request_line, body) = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the server must have received the payload");
        assert_eq!(request_line, "POST /hook/tok_real HTTP/1.1\r\n");
        assert_eq!(body, r#"{"type":"agent-turn-complete"}"#);
        server.join().unwrap();
    }

    /// Claude Code's hook protocol lets a response decide whether a tool call
    /// proceeds. Turn's server never sends one — and if something ever did, this
    /// helper would not carry it: it reads the answer only far enough to know the
    /// post landed, and returns the same `Ok(())` either way.
    #[test]
    fn a_response_that_tries_to_decide_something_is_read_and_ignored() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("must bind loopback");
        let address = listener.local_addr().unwrap();

        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("a connection must arrive");
            let mut reader = BufReader::new(&stream);
            let mut line = String::new();
            // Drain the request so the client is not left blocked on a write.
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                if line.ends_with("\r\n\r\n") || line == "\r\n" {
                    break;
                }
                line.clear();
            }
            let decision = br#"{"decision":"allow","permissionDecision":"allow","continue":true}"#;
            let mut stream = stream;
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                    decision.len()
                )
                .as_bytes(),
            );
            let _ = stream.write_all(decision);
        });

        let url = format!("http://{address}/hook/tok_decision");
        let options = Options::parse(
            [
                "--url",
                url.as_str(),
                r#"{"hook_event_name":"PermissionRequest"}"#,
            ]
            .map(String::from),
            None,
            false,
        );
        assert_eq!(
            run(&options, &mut std::io::empty()),
            Ok(()),
            "the helper reports delivery, never a decision"
        );
        let _ = server.join();
    }

    /// The requirement that outranks all the others: nothing about Turn being
    /// absent may become an error the agent sees.
    #[test]
    fn nothing_is_an_error_when_there_is_nothing_to_post_to() {
        // Nothing listening on this port.
        let closed = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = closed.local_addr().unwrap();
        drop(closed);

        let url = format!("http://{address}/hook/t");
        let options = Options::parse(["--url", url.as_str(), "{}"].map(String::from), None, false);
        // It fails, and the failure is a value the caller chooses to ignore —
        // never a panic and never a non-zero exit.
        assert!(matches!(
            run(&options, &mut std::io::empty()),
            Err(Failure::Transport(_))
        ));

        assert_eq!(
            run(&Options::default(), &mut std::io::empty()),
            Err(Failure::NoUrl)
        );
        assert!(matches!(
            run(
                &Options {
                    url: Some("not a url".into()),
                    ..Options::default()
                },
                &mut std::io::empty()
            ),
            Err(Failure::BadUrl(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn status_line_delegate_receives_identical_json_and_owns_the_exit_code() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("original.sh");
        let captured = dir.path().join("captured.json");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ncat > '{}'\nprintf 'original-output\\n'\nprintf 'original-error\\n' >&2\nexit 17\n",
                captured.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let payload = br#"{"model":{"display_name":"Opus"},"session_id":"same-bytes"}"#;
        let options = StatusLineOptions {
            url: None,
            original_script: Some(script),
            forwarder_exe: std::env::current_exe().unwrap(),
            timeout: Duration::from_millis(10),
            debug: false,
        };

        let code = run_status_line(
            &options,
            &mut std::io::Cursor::new(payload),
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(code, 17);
        assert_eq!(std::fs::read(captured).unwrap(), payload);
    }

    #[test]
    fn fallback_status_line_is_compact_useful_and_uses_remaining_capacity() {
        let payload = br#"{
            "model":{"display_name":"Opus"},
            "context_window":{"used_percentage":8.4},
            "rate_limits":{
                "five_hour":{"used_percentage":23.5},
                "seven_day":{"used_percentage":41.2}
            }
        }"#;
        assert_eq!(
            compact_status_line(payload),
            "TURN · Opus · ctx 8% used · 5h 76% left · 7d 59% left"
        );
        assert_eq!(
            compact_status_line(b"not json"),
            "TURN · Claude telemetry active"
        );
    }

    #[test]
    fn every_failure_can_be_explained_when_debugging_is_on() {
        for failure in [
            Failure::NoUrl,
            Failure::BadUrl("x".into()),
            Failure::NoPayload,
            Failure::PayloadTooLarge(9),
            Failure::NotLoopback("192.0.2.1".into()),
            Failure::Transport("refused".into()),
            Failure::StatusLineCommand,
        ] {
            assert!(!failure.to_string().is_empty());
        }
    }
}
