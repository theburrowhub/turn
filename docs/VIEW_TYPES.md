# Pane and view types

This document is the operator and implementation contract for the view names shown by Turn. It describes
the current executable, including compatibility values that remain in persisted Layouts but are not offered
as operational display choices.

`PaneKind::slug()` is the stable documentation and telemetry identifier for each entry. It is deliberately
not the JSON discriminator: the exact snake-case wire value is listed separately below. The explicit HTML
anchors use the slug, so links, help text and tests do not depend on Markdown heading generation.

## Six facts that must not be collapsed

A Pane can show an Agent while a Shell owns the terminal. Those are complementary facts, not a
classification conflict:

- **Semantic subject** — the `Node` the operator is looking at: Agent, Subagent, Shell, TUI, Server,
  TestRunner, Build, tmux process, or an unclassified process. Agent identity, provider, model, quota,
  turn state and Attention belong here.
- **Runtime host / PTY owner** — the process whose terminal Turn reads and writes. An Agent started inside
  an interactive shell has the Agent as its semantic subject and the Shell as its runtime host.
- **Launch kind** — the durable intent returned by `Pane::launch_kind()`. The additive optional
  `Pane.launch_kind` field carries it when it differs from the current presentation; otherwise the accessor
  falls back to `Pane.kind`. New display changes preserve this value.
- **Presentation kind** — the renderer label in required `Pane.kind`, whose historical v4 meaning is what the
  Pane shows. `Pane::presentation_kind()` returns this field.
- **Presentation provenance** — `Pane.kind_is_user_set`. False means Automatic and permits daemon detection
  to update presentation; true means an operator pinned **Display as…**. Provenance cannot be inferred from
  whether `launch_kind` is present: pinning the same type as the launch kind stores no optional value
  but still sets this bit.
- **Detected capability under a pin** — optional `Pane.detected_kind`. Automatic stores its current result
  directly in `kind`; a manually pinned Pane keeps the operator's renderer in `kind` and the daemon's latest
  capability result in `detected_kind`. This lets a Terminal pin remain selected without borrowing Agent B's
  Shell after exact Agent A loses that PTY.

The inspector therefore reports `subject type`, `detected view` and `runtime host` separately. A Claude
Code or Codex Agent hosted by `zsh` is not a Shell Agent and does not own `zsh`'s PTY: it is an Agent view
whose runtime host is a Shell. Technical wrapper processes may sit between those nodes; the inspector walks
the proved ancestry to the daemon-marked PTY host instead of assuming that the immediate parent owns it.

These identities have independent lifetimes. Selecting a Node, detecting a different presentation,
resetting to Automatic, or choosing **Display as…** never launches, resumes, stops or kills a process;
never sends terminal input; and never acknowledges Attention. Closing a Pane removes a view binding, not
the work behind it. Lifecycle operations remain separate, explicitly named actions.

## Persisted representation and v4 compatibility

A new automatic Shell Pane that is currently presenting a hosted Agent has this relevant shape:

```json
{
  "kind": "agent",
  "launch_kind": "shell",
  "kind_is_user_set": false
}
```

`kind` is required and remains the historical presentation field. `launch_kind` is additive and optional;
absent means launch intent falls back to `kind`. `kind_is_user_set` is additive and defaults to false.
`detected_kind` is additive and optional; it is present only when a manual pin needs the current Automatic
result kept separately. A newly created Pane therefore starts in Automatic with presentation and launch both
in `kind`. Detection changes `kind` and records the original launch intent in `launch_kind` whenever the two
differ.

For example, an exact Agent A view manually pinned to Terminal while A no longer owns a PTY is represented
without changing either its identity or the pin:

```json
{
  "kind": "terminal",
  "launch_kind": "shell",
  "kind_is_user_set": true,
  "detected_kind": "process_details"
}
```

Its selected renderer remains Terminal, but `Pane::has_terminal_capability()` is false. Turn detaches the
old generation-fenced feed; it does not display Agent B or the parent Shell under A's name. If A returns to
the foreground, `detected_kind` becomes `agent` and that exact view attaches again automatically.

This split preserves protocol-v4 presentation compatibility, with one unavoidable old-writer limit:

- a Pane stored before these additive fields existed decodes as Automatic and uses its historical `kind` for
  both presentation and launch;
- a newer client receiving an older payload does the same;
- an older client receiving a newer payload ignores the additive fields but still sees the correct current
  presentation in `kind`;
- if that older client rewrites a complete Layout, it cannot preserve a distinct `launch_kind` or manual
  provenance. The rewritten Pane collapses both axes to the visible `kind`, returns to Automatic and may
  therefore change what a later restore launches. Commands and current runtimes are not changed by merely
  decoding the newer payload, but a complete old-writer round trip is not lossless.

`NewPane.kind` means launch intent at creation time. `NewPane` does not accept `launch_kind` or provenance:
the returned Pane begins in Automatic with the same type on both axes, and a display pin is a separate
`change_pane_kind` operation after creation.

Capturing a live Layout as a Template removes transient Automatic detection, including `detected_kind`,
because it describes the
process to start rather than the process observed while it was captured. A valid manual operational display
pin is preserved; legacy pins to internal or renderer-less kinds encountered on this capture path are
normalised back to Automatic. Create/update RPCs reject such invalid manual pins with `invalid_argument`
instead of silently accepting or rewriting an untrusted draft. Editing a Template cell replaces its launch
kind: an Automatic presentation follows that new intent, while an explicit display pin remains pinned and
stores the new launch kind separately. Editing a command can therefore neither leave a stale relaunch type
nor manufacture a manual view override.

## Automatic detection and manual overrides

**Automatic** is the default. While `kind_is_user_set` is false, the Pane menu shows
`Automatic — <effective type>` and the daemon may update presentation `kind` from the semantic subject it
proved while preserving `launch_kind`. The effective order is:

1. A registered Agent adapter identifies an executable. The built-in registry has dedicated adapters for
   Claude Code (`claude`), Codex CLI (`codex`), Gemini CLI (`gemini`) and OpenCode (`opencode`). Adapter
   identity takes precedence over the Pane's original launch label. Direct Windows process names are
   matched case-insensitively after removing `.exe` and accepting either path separator.
2. A process observed inside a Turn-owned Shell is classified by the same adapter registry. The Pane is
   promoted only when the observed child's process group matches the Shell PTY's foreground process group;
   a background Agent child remains in the hierarchy and cannot steal the visible Pane. The foreground Agent
   becomes the semantic subject while the Shell remains PTY owner. Every debounced supervisor sweep
   reconciles already-known children as well as new ones: `fg` promotes a background Agent, Ctrl+Z returns
   the Pane to Shell without declaring the Agent dead, and an Agent A replaced by Agent B changes subject in
   the same sweep. The command Turn launched (`hosted`) retains lifecycle, relaunch and authenticated-hook
   authority while `observed_subject` independently follows whichever Agent owns the foreground process
   group. A hook from background A therefore remains A's fact and cannot disable foreground B's heuristic;
   `fg` restores A's own adapter/integration tier without relaunching it. Enter, Ctrl+C, Ctrl+D and Ctrl+Z
   schedule bounded reconciliation. Before publishing the first output from a newly observed foreground
   process group, Turn performs at most one eager reconciliation for that job and fences every durable and
   temporary exact-A feed; verbose output from the same job does not trigger a process-table scan per batch.
   When an inferred child ends, the automatic Pane also returns to its Shell subject.
3. Non-agent executables are classified conservatively as Shell, terminal app, Server, TestRunner, Build,
   tmux or another known `NodeKind`.
4. A terminal-backed subject that cannot be classified remains **Terminal**. Unknown is visible; screen
   text, a similar title or a shared directory is never enough to invent Agent identity.
5. A semantic Node with no terminal uses **Process details** rather than borrowing an ancestor's terminal.

For package launchers whose OS process is a language runtime, detection accepts only an adapter-owned script
at an exact path-component boundary. Absolute shims are canonicalised through symlinks; relative scripts are
resolved against the observed process cwd and then canonicalised. A shim name alone, an unrelated target or
an application-local `dist/index.js` is not evidence. The kernel-reported executable path is authoritative
and is treated as one structured path even when it contains spaces; mutable `argv[0]`, `exec -a`, a process
title and later prompt arguments cannot replace it. An adapter may explicitly declare a kernel-only
executable alias; it is accepted only when argv[0] is also one of that adapter's launch executables, and the
alias is not exposed as a launch command. Filesystem aliases are accepted only when the detected command and
kernel executable canonicalise to the same target. If a path exists, its canonical target is checked before
even an exact package-shaped spelling is accepted, so a symlink to unrelated code fails closed. Current and
retained compatible signatures are Claude Code
`node_modules/@anthropic-ai/claude-code/cli-wrapper.cjs` (plus legacy `cli.js`), Codex
`node_modules/@openai/codex/bin/codex.js`, Gemini CLI `node_modules/@google/gemini-cli/bundle/gemini.js`
or `dist/index.js`, and legacy/source OpenCode `node_modules/opencode-ai/bin/opencode`. Current native Claude
and OpenCode npm launchers are detected by their actual `claude.exe` and `opencode.exe` process names.
`node.exe` is treated like `node`. Code-evaluation forms such as `node --eval=...`, `--print=...`, `-e...`
or `-p...` terminate classification: a later path is an argument to evaluated code, not an executed Agent
script. Unknown option grammar, arbitrary prompt text, URL-like operands and merely similar path suffixes
fail closed to a generic terminal. When a package wrapper starts its same-provider native binary in the same
job, Turn projects one Agent rather than duplicate wrapper/native rows; an ephemeral PID alias still places
the native binary's children and sub-processes beneath that semantic Agent. Gemini's intentional Node-to-Node
relaunch is coalesced under the same rule only when both processes resolve to the exact same canonical bundle;
the provider name alone is insufficient and a second genuine Gemini invocation remains a separate Agent.

Non-agent launch classification also keeps executable and argv boundaries. Exact supported subcommands
include `cargo test|build|watch`, `go test|build`, `dotnet test|build|watch`, package-manager `test`/`start`
or exact `run`/`run-script test|build|dev|serve|start|watch` forms for npm, yarn, pnpm and bun,
`python -m http.server`, `python manage.py runserver`, direct pytest/Jest/Vitest, make/ninja and watchman.
Words appearing later in unrelated data are not intent: `echo test`, `node --eval "npm run dev"`,
`cargo metadata test`, `npm exec echo run dev` and a single argument `"run dev"` all fail closed.

Detection is about the current subject, not about provider health. A dedicated adapter may degrade from
structured events to terminal inference while the executable is still truthfully an Agent. Model, quota,
permission and turn facts retain their own source, confidence and freshness; unavailable facts stay
unavailable.

**Display as…** is a presentation override. It can pin only a kind with an operational Pane renderer. The
daemon rejects internal, reserved and renderer-less values even if a custom client sends one. The override
does not create Agent metadata, change `NodeKind`, transfer the PTY, alter the command, or affect future
launch/restore behavior. While the pin is active, automatic detection does not overwrite its presentation,
but it still updates `detected_kind` so terminal capability remains truthful. If the exact subject loses its
PTY, Turn fences and detaches that Pane's feed even when the chosen renderer is terminal-shaped; a renderer
override is never runtime authority. When that subject regains the PTY, the capability change triggers a
fresh attach.
Choosing **Automatic** again removes the pin, re-derives the current presentation from the bound semantic
subject and terminal capability, and falls back to the immutable launch kind when the Pane is unbound.

The eight operator-selectable terminal overrides are Terminal, Agent terminal, Shell, Terminal app, Logs,
Test output, Server and tmux terminal. Process details has an operational saved-Pane renderer but remains
automatic/internal because it requires an exact semantic Node binding; presenting it as a free display pin
would let users select a renderer with no subject. Event log, Agent tree, Preview and Placeholder are absent
because they are compatibility, migration or reserved values without a saved-Pane renderer.

## Terminal input, including multiline prompts

All eight operational types use Turn's terminal renderer and the exact PTY bound to the Pane's current
runtime host. Input is accepted only by the focused Pane while the current surface owns the input lease and
no modal or safety boundary is holding it.

- **Enter** sends carriage return (`0x0d`), the normal submit/execute key.
- A pure **Shift+Enter** sends line feed (`0x0a`), the terminal sequence produced by **Ctrl+J**. Agent
  composers that use Ctrl+J for a newline therefore receive multiline input without submitting the prompt.
- Shift+Enter emits exactly one byte on key-down. Key release and modifier transitions emit no duplicate
  input.
- Adding Alt, Control or Command is not treated as the pure multiline gesture. Turn uses each chord's
  legacy terminal encoding: Alt+Shift+Enter is Meta-Enter, while Control/Cmd+Shift+Enter are
  indistinguishable from ordinary Enter until Turn supports an enhanced keyboard protocol.

Turn guarantees the byte sequence, not an arbitrary program's key binding. A terminal application that
assigns Ctrl+J another meaning remains authoritative for its own input. Non-terminal compatibility kinds
accept no PTY input.

## Lifecycle, restore and common fallback states

Presentation and lifecycle are independent. A live bound runtime attaches to the current view; attachment
does not launch it. A Pane without a usable live grid renders an explicit state rather than a blank success:

- **restoring / attaching** — Turn is reconnecting or safely materialising the saved launch intent;
- **stopped** — the bound process ended and no automatic action is currently running;
- **archived** — archived work remains stopped;
- **survived outside Turn / disconnected** — the old runtime cannot be claimed as a current PTY merely
  because metadata survived;
- **no process in this Pane** — no supported process or non-terminal renderer supplies content.

A bound semantic Node without a proved live or recovered terminal fails `attach_pane` with `conflict`; it
never receives a blank surface that could later be mistaken for a valid terminal. A blank terminal attachment
is reserved for an intentionally empty Pane whose `node_id` is absent.

The persisted restore behavior is also independent of display type:

- `reattach_only` attaches a proved survivor. A consequential command that did not survive remains stopped.
  Its sole launch exception is a bare commandless non-Agent terminal intent with no arguments or Agent launch
  profile; that shape may reopen the configured interactive shell because it has no automated checkout
  action. A commandless Agent intent is consequential because it can resolve the configured/default Agent.
- `relaunch` permits Turn to materialise the saved launch intent when the Session is presented and the
  applicable safety/checkout authority is satisfied. It is not permission to reinterpret a display
  override as a command.
- `skip` leaves the saved process stopped.

For an explicit command, the adapter registry still decides whether the command is an Agent regardless of
the requested launch kind. For a commandless terminal-backed kind other than Agent terminal, the configured
Session/Workspace shell is the safe fallback. A commandless Agent terminal resolves the Workspace default
Agent, then an installed registered Agent, and otherwise opens the configured shell with a visible
explanation. No missing integration may silently substitute a different Agent.

## Complete catalog

| Stable anchor | Wire value | Operator label | Operational override | Automatic use | Current renderer/source |
| --- | --- | --- | --- | --- | --- |
| [`terminal`](#terminal) | `terminal` | Terminal | yes | generic terminal, watcher, background or unknown terminal subject | PTY terminal grid |
| [`agent-terminal`](#agent-terminal) | `agent` | Agent terminal | yes | Agent/Subagent with an exact terminal binding | PTY terminal grid plus semantic Agent details |
| [`shell`](#shell) | `shell` | Shell | yes | Shell subject | PTY terminal grid |
| [`terminal-app`](#terminal-app) | `tui` | Terminal app | yes | TUI subject | PTY terminal grid, including alternate-screen/mouse modes |
| [`logs`](#logs) | `logs` | Logs | yes | manual display; `Background` currently detects as generic Terminal | PTY terminal grid; no specialised log parser |
| [`test-output`](#test-output) | `test_output` | Test output | yes | TestRunner or Build subject | PTY terminal grid |
| [`server`](#server) | `server` | Server | yes | Server subject | PTY terminal grid |
| [`event-log`](#event-log) | `event_log` | Event log | no | none in a saved Pane | compatibility value; no operational renderer |
| [`agent-tree`](#agent-tree) | `agent_tree` | Agent tree | no | none; obsolete navigation value | migrated to Shell on load |
| [`process-details`](#process-details) | `process_details` | Process details | no | semantic Node without its own terminal | Node WorkSurface/details in selected, tiled, floating and temporary views |
| [`preview`](#preview) | `preview` | Preview | no | explicit quick/temporary Node preview path | preview projection, not a saved-Pane renderer |
| [`tmux-terminal`](#tmux-terminal) | `tmux_terminal` | tmux terminal | yes | tmux Session or Pane subject | PTY terminal grid bound to the tmux runtime |
| [`placeholder`](#placeholder) | `placeholder` | Placeholder | no | none | reserved compatibility value; no renderer |

<a id="terminal"></a>
## Terminal

`PaneKind::Terminal` is the honest generic terminal presentation.

- **Status:** operational and offered in **Display as…**.
- **Automatic detection:** used for terminal, watcher, background and unknown subjects that have a terminal.
  It is also the fallback when the daemon cannot prove a more specific kind.
- **Data and renderer:** the current bounded terminal `Grid` obtained from the Pane's PTY feed. ANSI color,
  scrollback, search, links, resize and ordinary terminal modes use the shared terminal engine.
- **Input:** full focused-PTY input, including Shift+Enter as Ctrl+J.
- **Launch and restore:** an explicit non-agent command runs directly; a registered Agent command is hosted
  as an Agent regardless of this launch label. With no command, the configured shell is opened. Restore
  uses the immutable launch kind and saved command, never a later display override.
- **Fallback:** unknown stays Terminal rather than being guessed. A missing feed shows the applicable
  restore/stopped/disconnected/no-process state.
- **Truth boundary:** manually displaying an Agent as Terminal hides no semantic Agent facts in its Node
  view and does not turn the Agent into a generic process.

<a id="agent-terminal"></a>
## Agent terminal

`PaneKind::Agent` presents an Agent whose interactive content is available through a terminal.

- **Status:** operational, automatically selected for a terminal-backed Agent/Subagent, and available as a
  manual display override.
- **Automatic detection:** the adapter registry, not the original Pane label, identifies Claude Code,
  Codex CLI, Gemini CLI, OpenCode and other registered Agent executables. The same rule applies when the
  operator types an Agent command into an existing Shell.
- **Data and renderer:** terminal cells come from the exact runtime host. Identity, provider/tool, model,
  quota, launch receipt, turn state and Attention come from the semantic Agent Node and its independently
  sourced observations.
- **Input:** bytes go to the verified PTY owner. For a hosted Agent this is normally its parent Shell; the
  inspector names both. Shift+Enter sends Ctrl+J to that PTY.
- **Launch and restore:** a commandless Agent launch tries the Workspace default Agent, then the first
  installed registered Agent. A named registered command keeps that exact command and adapter. Restore uses
  the saved Agent launch intent, subject to lifecycle and checkout safety.
- **Fallback:** if the requested/default Agent is unavailable, Turn opens the configured shell and prints
  why; it does not silently run another configured Agent. An Agent with semantic activity but no terminal
  detects as Process details. Missing model/quota/structured events remain unknown or degraded without
  changing Agent identity.
- **Truth boundary:** choosing this as an override cannot make a Shell or arbitrary command an Agent and
  cannot fabricate provider, model, quota, permission or Attention state.

<a id="shell"></a>
## Shell

`PaneKind::Shell` presents an interactive command shell.

- **Status:** operational and offered in **Display as…**.
- **Automatic detection:** selected for a semantic Shell subject. Known shell executables are classified as
  Shell rather than inferred from their title or prompt.
- **Data and renderer:** the Shell-owned PTY grid through the common terminal engine.
- **Input:** full focused-PTY input, including Shift+Enter/Ctrl+J. The shell decides how Ctrl+J behaves at
  its current prompt or inside its foreground program.
- **Launch and restore:** with no explicit command, Turn resolves the configured Session shell, then the
  Workspace/environment fallback. Commandless shells are the safe terminal restore exception described
  above.
- **Fallback:** an unavailable shell or detached/stopped PTY produces an explicit runtime state. Turn does
  not label it as an Agent merely because its title mentions one.
- **Subject transition:** when a registered Agent is started inside the Shell, Automatic may change the
  Pane subject and presentation to Agent terminal while this Shell remains the PTY owner. When an inferred
  Agent child ends, Automatic returns to Shell. A manual Shell override pins only the presentation.
- **Truth boundary:** Shell is the PTY runtime host, not proof that the foreground semantic subject is a
  Shell; Agent identity follows adapter and foreground-process evidence.

<a id="terminal-app"></a>
## Terminal app

`PaneKind::Tui` is labelled **Terminal app** and represents a full-screen terminal UI.

- **Status:** operational and offered in **Display as…**.
- **Automatic detection:** selected for a TUI subject proved from its executable/process classification.
- **Data and renderer:** the common PTY grid with alternate-screen, cursor, mouse and resize handling. There
  is no separate application framebuffer or GUI embedding path.
- **Input:** full terminal keyboard/mouse input while focused. Shift+Enter still sends Ctrl+J; the TUI owns
  its interpretation.
- **Launch and restore:** an explicit program is required for a meaningful TUI launch. Because this is a
  terminal-backed launch kind, omitting the command opens the configured shell rather than inventing a file
  browser or monitor. Relaunch/reattach uses the saved exact command.
- **Fallback:** an unrecognised full-screen program may remain Terminal. Missing executable, stopped PTY or
  unsupported modes are reported explicitly; Turn does not substitute another TUI.
- **Truth boundary:** the label adds no Agent turn, model, quota or permission semantics.

<a id="logs"></a>
## Logs

`PaneKind::Logs` is a terminal presentation hint for a command whose output is being followed.

- **Status:** operational as a terminal display override; it is not a specialised log viewer.
- **Automatic detection:** a launch with Logs intent creates a Background semantic subject when no stronger
  executable classification exists. The current automatic presentation for Background is generic Terminal,
  so **Logs** normally appears only when manually pinned.
- **Data and renderer:** the same PTY grid, scrollback and search as Terminal. There is no structured log
  source, severity parser, rotation protocol or read-only guarantee attached to this kind.
- **Input:** full focused-PTY input. It is not made read-only by the Logs label; Shift+Enter sends Ctrl+J.
- **Launch and restore:** use an explicit log-producing/following command. With no command, the terminal
  launch fallback is the configured shell. Restore retains the exact command and launch intent.
- **Fallback:** a background process with no specialised evidence remains Terminal automatically. Missing
  command/feed uses the common shell or explicit stopped/no-process state.
- **Truth boundary:** displaying output as Logs does not classify the process, parse its records or suppress
  lifecycle/Attention facts.

<a id="test-output"></a>
## Test output

`PaneKind::TestOutput` presents a test or build process in a terminal.

- **Status:** operational and offered in **Display as…**.
- **Automatic detection:** selected for TestRunner and Build subjects recognised from the executable and
  arguments.
- **Data and renderer:** the exact PTY grid and bounded scrollback. Test result summaries may come from
  separate semantic observations; the Pane itself remains a terminal.
- **Input:** full focused-PTY input, useful for interactive test runners; Shift+Enter sends Ctrl+J. The label
  is not a read-only policy.
- **Launch and restore:** an explicit test/build command runs directly. With no command, the configured
  shell fallback opens and Automatic may present it as Shell. Restore uses the saved command.
- **Fallback:** an unrecognised runner remains Terminal, and an ended run shows stopped rather than a false
  passing/failing summary. No output text alone may fabricate a test result.
- **Truth boundary:** a manual Test output override changes presentation only and cannot mark a process as a
  test, build, success or failure.

<a id="server"></a>
## Server

`PaneKind::Server` presents a long-running service or development server terminal.

- **Status:** operational and offered in **Display as…**.
- **Automatic detection:** selected for a Server subject recognised from its executable/arguments.
- **Data and renderer:** the service's PTY grid and scrollback. Port, health or endpoint facts require their
  own observations; they are not inferred from this label.
- **Input:** full focused-PTY input, including Ctrl+C and Shift+Enter/Ctrl+J according to the program's
  terminal behavior.
- **Launch and restore:** use an explicit server command. With no command, the configured shell fallback
  opens. Relaunch remains governed by the saved restore behavior and checkout authority.
- **Fallback:** an unrecognised service remains Terminal; a stopped process is shown as stopped and never as
  healthy merely because the Pane says Server.
- **Truth boundary:** changing the display neither starts a listener nor grants network or lifecycle
  authority.

<a id="event-log"></a>
## Event log

`PaneKind::EventLog` is a persisted compatibility value for a Turn-owned, non-PTY view.

- **Status:** not offered in **Display as…** and not an operational saved-Pane renderer in the current
  executable.
- **Automatic detection:** none.
- **Data and renderer:** no Event log Pane data source/renderer is currently wired. Event history that is
  available belongs to the selected WorkSurface/inspector rather than this Pane kind.
- **Input:** no terminal input.
- **Launch and restore:** non-terminal; it launches no process and has no PTY to reattach.
- **Fallback:** an old Layout containing it can show the explicit no-process/unavailable state. Turn must not
  present that as a working event stream.
- **Truth boundary:** it remains serialisable so old data can be read, not as a promise of a hidden feature.

<a id="agent-tree"></a>
## Agent tree

`PaneKind::AgentTree` is an obsolete navigation value from before the unified Workspace hierarchy.

- **Status:** migration-only; never offered in **Display as…**.
- **Automatic detection:** none. The left Workspace hierarchy is Turn's only persistent navigation tree.
- **Data and renderer:** no second Agent-tree Pane renderer exists.
- **Input:** no terminal input as Agent tree.
- **Launch and restore:** loading a stored Session or Template migrates this Pane to a commandless Shell,
  clears obsolete bindings/configuration and uses the normal Shell restore path.
- **Fallback:** migration yields a useful Shell instead of an empty duplicate navigator.
- **Truth boundary:** documentation and UI must never imply that two independent hierarchy authorities are
  available.

<a id="process-details"></a>
## Process details

`PaneKind::ProcessDetails` is the automatic presentation capability for a semantic Node without an exact
terminal of its own.

- **Status:** automatic/internal; not offered as a saved-Pane display override.
- **Automatic detection:** selected when a Node has no terminal capability, including semantic-only
  subagents and external applications whose UI lives elsewhere.
- **Data and renderer:** `TreeNodeView`, bounded inspector details, activity preview, relationships,
  lifecycle and capability evidence in the Node WorkSurface renderer. The same renderer fills a durable
  tiled or floating Process-details Pane; the active durable Pane requests the bounded projection for its
  exact Node. It must never borrow a parent's terminal merely to fill space.
- **Input:** no PTY input. Any typed semantic action is separately scoped and named.
- **Launch and restore:** opening details attaches no process and performs no lifecycle action. A temporary
  view leaves Layout untouched; explicitly keeping it creates a durable semantic Pane with the same Node
  binding and renderer.
- **Fallback:** loading, empty, stopped, lost, stale and unsupported facts are labelled inside the Node view.
  A saved Pane whose Node no longer exists shows the explicit unavailable/no-process state instead of stale
  semantic data.
- **Truth boundary:** Process details is a Node capability, not evidence that the Node owns a terminal.

<a id="preview"></a>
## Preview

`PaneKind::Preview` is a compatibility/internal value for bounded semantic preview content.

- **Status:** not offered in **Display as…** and not an operational saved-Pane renderer.
- **Automatic detection:** none as a durable Pane. Quick Preview and temporary Node views are invoked through
  their explicit surface-scoped paths.
- **Data and renderer:** bounded `ActivityPreview` history when that separate preview path is available; no
  raw terminal clone is fabricated.
- **Input:** no PTY input. Preview navigation cannot submit a prompt or acknowledge Attention.
- **Launch and restore:** launches nothing, owns no runtime and does not mutate the saved Layout unless the
  operator explicitly promotes a temporary view.
- **Fallback:** absent preview evidence is shown as unavailable/empty on the preview path. A legacy saved
  Preview Pane has no operational renderer and may show no process.
- **Truth boundary:** preview text is a bounded status projection, not a transcript or proof of completion.

<a id="tmux-terminal"></a>
## tmux terminal

`PaneKind::TmuxTerminal` presents a terminal bound to a tmux Session or Pane subject.

- **Status:** operational and offered in **Display as…**.
- **Automatic detection:** selected for semantic `TmuxSession` and `TmuxPane` nodes with terminal capability.
- **Data and renderer:** the exact terminal grid attached through Turn's runtime binding; the common terminal
  engine renders it.
- **Input:** full focused-PTY input while Turn owns the input lease. Shift+Enter sends Ctrl+J.
- **Launch and restore:** use an explicit tmux command/binding for a real tmux launch. A commandless
  terminal-backed launch falls back to the configured shell and does not silently create a tmux Session.
  Reattachment requires proved matching runtime identity; a same-named tmux object is not sufficient.
- **Fallback:** without a proved tmux binding, Automatic uses the actually detected subject, commonly Shell
  or Terminal. Stale/disconnected tmux state is labelled rather than attached optimistically.
- **Truth boundary:** pinning this label cannot create tmux durability or make a plain shell reattachable.

<a id="placeholder"></a>
## Placeholder

`PaneKind::Placeholder` is reserved so persisted layouts can survive future integration evolution.

- **Status:** reserved/internal; never offered in **Display as…**.
- **Automatic detection:** none.
- **Data and renderer:** none in the current executable.
- **Input:** no terminal input.
- **Launch and restore:** launches and reattaches nothing.
- **Fallback:** a persisted Placeholder is shown as unavailable/no process until a version with an explicit,
  documented migration understands it.
- **Truth boundary:** Placeholder is not a generic extension hook and must never be shown as a functional
  choice.

## Adding or changing a type

A Pane type is complete only when all of the following change together:

1. `PaneKind::ALL`, its snake-case wire value, `slug()`, `label()`, terminal capability, override exposure
   and automatic detection are exhaustive and agree.
2. Its renderer has explicit live, loading, empty, stopped, lost, stale, disconnected and unsupported
   behavior where applicable.
3. Launch intent and presentation remain separate; display changes are lifecycle- and input-neutral.
4. The menu exposes only operational renderers and shows Automatic versus manual override truthfully.
5. This catalog contains the exact anchor immediately followed by its exact heading and documents wire value,
   data source, detection, input, restore and fallback behavior.
6. Tests enumerate every `PaneKind`, compare its serialized wire value and override status with this table,
   exercise legacy/new serialization plus automatic/manual transitions, and prove that an override cannot
   mutate launch intent, process, binding, lifecycle, input or Attention.

An enum variant without this evidence may remain a readable migration/reserved value, but it is not an
operator-facing view type.
