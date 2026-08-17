# Local voice input

**Status:** accepted target for M15; not implemented in v0.1.

This document is normative for Turn's first speech-to-text feature. It adds fast operator dictation without
weakening the product's central job: routing the operator to the exact Agent that needs a decision, answer or
result review. “Local” has one precise meaning in this milestone: microphone audio and inference remain on
the physical operator device that owns the foreground Turn surface.

M15 uses one packaged local Whisper-compatible inference engine. There is no engine selector because the
milestone has no cloud or remote alternative; model, execution backend (for example CPU versus an available
local accelerator), engine version and requested/effective mismatch remain visible facts.

## 1. Goals and non-goals

M15 provides push-to-talk dictation for an already verified editable Agent or Shell input. The common path is
hold the configured shortcut, speak, release, review the focused inline draft and press the normal send key.
The draft is part of the selected WorkSurface rather than a modal or a second composer. Zero bytes reach the
target before the operator chooses **Send** or **Insert**, so review and the execution boundary stay explicit
without adding a hunt or a mandatory mouse click.

The first release deliberately does not provide:

- a voice-command language, wake word, background listening or automatic recording;
- semantic intent detection, automatic prompt answering or spoken allow/deny;
- cloud transcription, browser-to-server audio, Session-host transcription or silent fallback of any kind;
- meeting capture, transcript history, speaker identification or audio-file import;
- an independent chat composer, second navigation surface or voice-owned Attention queue.

Only a future, separately reviewed capability may move PCM off the operator device. Calling inference on a
remote Session host “local” is forbidden even when that host is under the operator's control.

## 2. User flow with the fewest safe interactions

Dictation starts from the global shortcut or the microphone control beside the currently verified input. It
is off by default, downloads nothing until the operator explicitly installs a model and asks for microphone
permission only on the first capture attempt. Hold-to-talk is the default; an accessible toggle mode exposes
the same start/stop/cancel states without requiring a held key.

Foreground Session selection may independently auto-attach/start its safe current/default Shell under
`ACP-LIF-009`, but dictation never invokes or authorises that operation. Until activation has produced one
current input-ready attempt, the microphone control is disabled with the exact preflight/starting reason and
the shortcut opens no microphone. A shortcut pressed during activation is not queued or replayed. Browser,
Web, NativeJob, ConversationInventory, WorkItem and Activity Inbox views have no dictation target merely
because they are selected.

At capture start, Turn freezes a client-local `DictationTarget`: one exact protocol `InputTarget` plus the
capture generation and draft bound. It contains:

- foreground `surface_id`, surface connection generation and daemon generation;
- exact Workspace, Session, Node and, for an Agent, AgentInstance;
- current RuntimeAttempt/generation and verified input-owner Node/Pane or structured composer;
- exact pending-interaction id/revision when the input belongs to a question;
- expected input revision and the 32 KiB UTF-8 draft bound.

No eligible target means no microphone open. A pending permission has no editable voice target. Selection,
manual navigation, window blur, device loss, Escape or closing the surface stops capture immediately and
discards its PCM. Turn never redirects a recording to whatever happens to be selected later.

During capture and transcription, the bottom status bar shows microphone state, exact target, model,
`on this device`, duration/progress and a cancel action. It is not a transient top-of-content error banner.
The selected Node View may mirror a compact microphone state beside the safe input. Screen readers announce
start, target, transcription and cancellation without continuously announcing the audio meter.

On successful release:

1. the client strips controls and invisible directional characters, normalises CR/LF to spaces for a PTY,
   applies the declared Unicode/byte bound and shows truncation rather than hiding it;
2. it places the result in an editable memory-only inline draft labelled with both the semantic Agent and the
   real input owner; no PTY, adapter or provider has received it yet;
3. **Insert** performs one bracketed paste with `submit=false`, while **Send** performs exactly one submit
   through the verified input path. If the target changed, both actions are disabled and the draft offers
   **Return to target**, **Copy** and **Discard**; explicit retarget creates a new target review and never
   follows selection implicitly.

The draft receives keyboard focus, so the common final action is the same Enter/send gesture used for typed
input. A late worker answer after cancellation is dropped by capture generation. Empty/silence-only output
creates no draft.

## 3. Attention remains authoritative

Voice is an input method, not an attention authority:

- recording, transcription and draft editing never create, rank, acknowledge, dismiss or resolve an
  `AttentionEntry`;
- dictation never invokes `route_attention`, `activate_session`, launch/resume, focus or an approval action;
- it may populate a verified question composer, but that question remains pending until the operator submits
  and adapter evidence confirms the exact interaction ended;
- it cannot target a permission control, synthesize allow/deny or interpret words such as “yes” as a decision;
- an explicitly granted remote/Companion typed permission response does not add a microphone, audio or
  free-text answer path; dictation remains physical-foreground-client only;
- transcription errors use operational status and do not masquerade as Agent failures or Attention demands.

Capture, transcription and a visible voice draft set `UserContext.sensitive_operation` on that exact surface.
The global queue, badges, unread state and notifications continue updating, while governor-initiated
automatic Focus is deferred with its original exact route. It is never transferred to another surface. A
user-invoked `Next Attention`, badge or notification route wins: live capture is cancelled, a completed draft
remains bound to its original target, and the Attention entry itself is unchanged.

## 4. Process and trust architecture

Microphone capture and the draft belong to the native client. The daemon never receives PCM. Inference runs
in a disposable, supervised `SpeechWorker` child, not inside the GUI, daemon or a runtime Agent process:

```text
foreground surface ── mic PCM in memory ──► sandboxed SpeechWorker
        │                                      │
        │ exact DictationTarget/InputTarget    └── bounded transcript
        ▼
inline review ── explicit Insert/Send ──► daemon validates target ──► one safe text commit
```

The client gives the worker only a read-only verified model descriptor and one bounded audio buffer through
an inherited descriptor/shared-memory handle. The worker has no network, daemon socket, repository,
credential, clipboard, accessibility or arbitrary filesystem access. CPU/GPU/RAM and wall-clock limits keep
transcription below terminal/UI responsiveness; only one inference job runs per worker and queued work is
bounded to one pending capture per surface. One device-scoped microphone lease prevents two Turn windows
from recording concurrently without an explicit handoff.

A hang, native abort, segmentation fault, corrupt model or OOM kills/restarts only the worker. Sessions,
Attention and terminal processes remain live. The job becomes a recoverable local status; it is not replayed
and does not insert partial text. Shutdown cancels work and then terminates the worker within a fixed budget;
Turn never waits indefinitely for a native inference library.

The sandbox and separate process reduce consequences of a malicious/corrupt model but do not make model
parsing safe. Unsupported platforms expose dictation as unavailable with the exact reason, not a cloud or
remote fallback.

An unavailable local accelerator may fall back automatically to the supported local CPU backend because
that changes performance, not data placement or authority. The status bar records requested versus effective
backend before capture; if no compatible local backend exists, capture is unavailable rather than remote.

The daemon owns the closed model catalogue, installation state and Turn data-directory artifacts, while the
native client owns microphone permission, PCM and drafts. A platform broker gives the worker a read-only
descriptor for one verified model; neither GUI nor worker chooses an arbitrary model path, and no PCM crosses
the daemon administrative protocol.

## 5. Model lifecycle

Model installation is an explicit operator action separate from microphone consent. A versioned Turn-owned
manifest allowlists each engine/model artifact with stable id, engine compatibility, HTTPS origin, exact
byte size, cryptographic digest, display size, language capability, provenance and licence/notice. The
installer:

- validates the manifest through the normal signed-update trust root or an equivalently pinned signature;
- checks declared size and free space before download, enforces a hard streaming byte cap and verifies the
  digest before any native parser sees the artifact;
- permits no redirect outside the exact manifest origin and uses a direct connection whose complete A/AAAA
  answer set is public and whose socket is pinned to one approved address while retaining TLS SNI/Host;
- writes an owner-only create-new temporary file in the model directory, never follows symlinks, fsyncs and
  atomically renames only after validation;
- generation-fences concurrent download/delete/select operations and removes partial files on cancel/failure;
- records requested versus installed/effective model and engine versions without pretending an unavailable
  accelerator or model was used.

Total installed weights obey `records.local_speech_models_mib` (8,192 MiB by default). At capacity the
installer refuses the new model and offers explicit model removal; it never evicts the selected model or
silently substitutes another. One stable model id has at most one active artifact plus one fenced upgrade
partial, and an old artifact is removed only after every worker descriptor releases it.

Model bytes are the only network transfer in M15. Download UI names the origin, size and licence before the
request. Deleting a selected/in-use model first cancels its worker job and retires the worker descriptor;
physical deletion never races a live parser. Unclassified files in the model directory fail privacy
inventory rather than being silently adopted.

## 6. Settings

Daemon-owned settings use the normal Global → Workspace → Template → Session → Temporary hierarchy where
declared:

| Key | Default | Meaning |
| --- | --- | --- |
| `input.dictation.model` | `none` | Global; closed manifest id. `none` means disabled and downloads/loads nothing |
| `input.dictation.language` | `auto` | All levels; `auto` or a validated language hint, not an accuracy promise |
| `input.dictation.max_seconds` | `120` | Global; range 15–300, with 300 as an absolute capture ceiling |

The shortcut is only `keyboard.bindings["input.dictate"]`, which the window already owns. The platform
default is shown before enabling and must pass the existing collision/accessibility validator rather than
being accepted blindly. The accessible microphone button always provides toggle start/stop/cancel alongside
hold-to-talk. M15 stores no input-device id: it uses the OS-selected device for each consented capture.
There is no `cloud`, `auto_send`, `voice_commands` or skip-review setting.

The Node View/status bar shows configured, installed and effective model separately. A Session asking for a
model absent on the active client remains `model_unavailable`; it never downloads, substitutes or falls back
without the operator's explicit install/select action.

## 7. Protocol target

The public protocol cannot start a microphone or request transcription. The client snapshots its exact
`InputTarget` from the selected revision; only the operator gesture invokes OS capture. M15 adds model
management plus one generic reviewed-text commit that typed/pasted input may also reuse:

| Operation | Required request | Result |
| --- | --- | --- |
| `list_local_speech_models` | none | `local_speech_models` |
| `install_local_speech_model` | foreground surface, `operation_id`, closed `model_id` | `local_speech_model_state` |
| `cancel_local_speech_model_install` | foreground surface, `operation_id`, `model_id`, expected generation | `local_speech_model_state` |
| `remove_local_speech_model` | foreground surface, `operation_id`, `model_id`, expected generation | `local_speech_model_state` |
| `commit_operator_text` | foreground surface/connection/daemon generation, `operation_id`, exact `InputTarget`, expected input revision, either `insert` or `submit`, bounded UTF-8 text | `operator_text_delivery` |

Model state/progress may be pushed, but carries no audio. `InputTarget` repeats the semantic subject, verified
input owner, attempt/prompt generations and bound. `commit_operator_text` revalidates every identity,
foreground surface and pending interaction immediately before write. It accepts only
`InputCapability::FreeText`; password/credential, permission, provisional/unassigned, raw TTY and unverified
alternate-screen targets are refused. `origin=dictation` is non-authoritative provenance and raises no
confidence or capability.

PTY **Insert** reuses the verified input owner's injection-safe bracketed-paste path after replacing CR/LF
with spaces and cannot append Enter. **Send** is exactly one fenced paste-and-submit; a pending question uses
the same exact interaction identity and remains delivery-pending until adapter evidence. Structured adapters
use their existing reviewed composer/response operation. One operation id fences duplicates. A definitely
rejected pre-write request is safely retryable by an explicit operator action; a disconnected or possibly
partial insertion/submission becomes `submitted_unconfirmed` and is never retried automatically.

There is no `voice_started` daemon event and no durable `VoiceDraft` record. Non-content protocol/status may
name availability, installed/effective model id, placement and worker health, but never PCM, transcript,
device id, waveform or a transcript hash.

Foreground review is a supported authenticated-flow invariant, not isolation from a malicious same-uid
process that steals the administrative capability. Such a process can already impersonate terminal input;
M15 does not call that operator consent. Crucially, the daemon protocol still has no operation with which it
can open the GUI microphone, obtain PCM or read the client-local draft.

## 8. Privacy and retention

PCM lives only in bounded client/worker memory, is zeroed/released on stop, cancel, failure and completion,
and never enters SQLite, files, event payloads, logs, terminal journals, analytics, diagnostics or Turn crash
reports. This is an application guarantee, not a claim that a general-purpose OS can prevent swap, physical
memory inspection or a privileged debugger.

Before insertion, transcript text exists only in the foreground client's memory. Cancel/discard/window close
removes it. Once inserted, it becomes ordinary target input and may remain in the Agent/provider transcript,
visible terminal/scrollback and ADR-052 journal; the enabling UI states that boundary. Turn cannot revoke
text already observed downstream. Choosing **Copy** is an explicit transfer to the OS clipboard, which is
outside Turn's deletion authority and is disclosed beside that recovery action.

ADR-057 report/export/delete covers:

- installed model files with id, digest, size, origin, licence and engine compatibility;
- model download partials, worker availability/version and non-secret health state;
- daemon dictation settings and the keyboard binding;
- zero audio/transcript-draft records as an asserted, tested category.

Model files are installation-owned and never exported as content. `privacy-delete installation` removes
them; Settings model deletion removes one verified file and its partials without following symlinks. Session,
Node or Agent deletion has no audio/draft record to remove. The existing bounded daemon/client log policies
apply to non-content failures; error strings are mapped to closed codes before logging.

## 9. Failure behavior

- Microphone permission denial, missing device/model, unsupported accelerator and disk exhaustion are visible
  local status with one relevant recovery action and no hidden fallback.
- Capture timeout stops at the configured bound and transcribes only the bounded buffer unless cancelled.
- Target/surface/attempt/prompt generation change prevents insertion; it never retargets by title, Pane, cwd
  or current selection.
- Worker crash/hang/cancel produces no partial or late insertion and cannot crash the daemon/GUI.
- Model digest/size/manifest/engine mismatch is rejected before native load and the partial is deleted.
- Losing the daemon after transcription preserves only the client-local editable draft; reconnect must obtain
  a new target and requires explicit retarget/reinsert.
- Automatic Focus deferred during dictation either applies its original still-valid route afterward or
  degrades to queue/badge under the normal governor; voice never chooses a replacement route.

## 10. Acceptance contract

M15 is not complete until native, daemon and adversarial tests prove:

- disabled-by-default behavior: no model download, microphone prompt, worker or network request before an
  explicit operator action;
- exact target freezing across two windows, two Sessions, sibling Agents sharing a PTY and a PTY-less Agent;
- hold/release, accessible toggle, Escape, blur, selection change, window close, device loss, timeout and a
  late transcription result all stop tracks and never redirect/insert unexpectedly;
- zero target bytes before explicit **Insert**/**Send**; afterward only the verified editable owner receives
  one bounded fenced operation, with CR/LF/control/invisible input unable to escape bracketed paste or answer
  a permission;
- Session autoactivation, Browser/Web selection, native-job controls, conversation adoption/resume and remote
  permission response cannot be triggered by the dictate shortcut; capture attempted before input readiness
  opens no device and is never queued/replayed;
- attempt/prompt/surface/daemon generation changes fail closed, and an uncertain insertion is never replayed;
- queue order, badge, unread and resolution state remain byte-for-byte equivalent apart from the operator's
  later explicit send; automatic Focus defers only on the exact sensitive surface and retains its route;
- corrupt/truncated/oversized/wrong-digest models, full disk, symlink races, concurrent install/delete,
  worker SIGABRT/SIGSEGV/OOM/hang and shutdown during inference leave Sessions and Attention healthy;
- worker sandbox denies network, daemon socket, repository, credentials and arbitrary filesystem access;
- requested/installed/effective model and placement are honest, and no cloud/Session-host fallback exists;
- privacy inventory/export/delete handles models/partials/settings, while fixture markers in PCM/transcript
  are absent from database, files, journals, logs, events, diagnostics and crash artifacts before insertion;
- inserted text is correctly reported as subject to normal terminal/provider retention;
- status-bar, Node View, keyboard, reduced-motion and screen-reader behavior require no hidden extra step.
