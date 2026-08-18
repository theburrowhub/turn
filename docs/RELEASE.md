# macOS release and safe update acceptance

Turn is one installed app bundle and three sibling executables:

```text
Turn.app/Contents/
├── Info.plist
├── MacOS/
│   ├── turn
│   ├── turnd
│   └── turn-hook
└── Resources/
    ├── release.plist
    └── turn-icon.icns
```

`turn` and `turnd` must report the same Cargo version and protocol window through
`--build-info`; `turn-hook` must report the same version. Packaging refuses the bundle
before signing if any component differs. `release.plist` records those versions and the
signed companion hashes. The app's sealed signature protects the main executable and
the whole resource envelope.

## Reproducible local acceptance

On macOS, this builds the release binaries, creates an ad-hoc hardened-runtime bundle,
checks the three build identities, verifies the companion hashes and validates every
nested signature:

```sh
make release-acceptance
```

The deterministic half also runs on Linux. It proves:

- `get_update_status` is a read-only protocol operation whose answer counts live PTY
  handles rather than stored lifecycle labels;
- a protocol-compatible UI update preserves the daemon even with live PTYs;
- an incompatible update is deferred while any PTY remains;
- even an idle incompatible daemon requires an explicit restart instead of being
  stopped by the installer;
- every packaging/update script parses under Bash without optional tools.

CI repeats the exact macOS bundle creation after the full workspace suite. This is not
a mock layout: it is the same `package-macos-app.sh` path the Developer ID release uses,
with only the signing identity and notary credentials changed.

## Production release

Pushing a tag that matches the workspace version (`v0.1.0`, for example) runs
`.github/workflows/release.yml` on native arm64 and Intel macOS runners. Each job:

1. imports the Developer ID Application certificate into an ephemeral keychain;
2. builds all three binaries with `--locked --release`;
3. applies hardened runtime signatures to the helpers and the outer app;
4. submits the archive to Apple's notary service, waits for acceptance and staples the
   ticket;
5. verifies Gatekeeper assessment, versions, protocols, hashes and signing Team ID;
6. emits one architecture-specific zip and stable-channel plist.

The publish job uses `gh release create`; it does not use a repository connector. The
workflow expects these repository secrets:

- `APPLE_DEVELOPER_ID_CERT_P12_BASE64`
- `APPLE_DEVELOPER_ID_CERT_PASSWORD`
- `APPLE_DEVELOPER_ID_APPLICATION`
- `APPLE_TEAM_ID`
- `APPLE_NOTARY_KEY_BASE64`
- `APPLE_NOTARY_KEY_ID`
- `APPLE_NOTARY_ISSUER_ID`

The same release can be reproduced locally with those credentials:

```sh
export TURN_CODESIGN_IDENTITY='Developer ID Application: …'
export TURN_EXPECT_TEAM_ID='TEAMID'
export TURN_NOTARY_KEY='/absolute/path/AuthKey_KEYID.p8'
export TURN_NOTARY_KEY_ID='KEYID'
export TURN_NOTARY_ISSUER='ISSUER-UUID'
export TURN_RELEASE_TAG="v$(cargo pkgid -p turn-gui | sed 's/.*[@#]//')"
export TURN_RELEASE_BASE_URL="https://github.com/theburrowhub/turn/releases/download/$TURN_RELEASE_TAG"
make macos-release
```

An existing output is never overwritten. `TURN_NOTARY_PROFILE` may replace the three
App Store Connect key variables for a developer who already has a local notarytool
profile.

## Clean install and update channel

The release contains:

- `Turn-VERSION-macos-arm64.zip` and `turn-stable-arm64.plist`;
- `Turn-VERSION-macos-x86_64.zip` and `turn-stable-x86_64.plist`.

For a clean install, expand the zip and move `Turn.app` to `/Applications`. The app
finds `turnd` beside its own executable, and the daemon finds `turn-hook` beside itself;
no development variables or `PATH` fallback are involved.

The supported updater chooses the host architecture automatically:

```sh
./scripts/install-macos-update.sh
```

It downloads the stable channel over HTTPS, checks archive size and SHA-256, verifies
the notarized bundle and bundle identifier, pins updates to the installed app's signing
Team ID, rejects a downgrade, then asks the authenticated live daemon for update status.
The install is staged beside the destination and renamed with rollback on failure. It
never sends a signal and never launches a daemon.

| Live state | Result |
| --- | --- |
| No daemon | Install the complete bundle. The next app launch starts its sibling. |
| Compatible daemon, any number of PTYs | Replace files only. Keep the daemon and every PTY alive; quit/reopen only the window when convenient. |
| Incompatible daemon, one or more PTYs | Exit 20 and replace nothing. Finish those Sessions first. |
| Incompatible daemon, zero PTYs | Exit 21 and replace nothing. Stop the idle daemon explicitly, then retry. |

This distinction matters because replacing files on disk does not replace the executable
image of an already-running daemon. A compatible new UI reconnects to that old daemon.
The newly installed `turnd` becomes active only after the old daemon has ended normally.

The vNext in-app lifecycle uses the same safety contract, not a second installer policy.
`ApplicationUpdate` has one current Installation-owned intent. Its pre-discovery QueryOnly record pins the
operation, channel/platform/architecture/current version, both expected trust-store revisions and
anti-rollback high-water without claiming an accepted manifest or package. Available/download states require
accepted manifest evidence; verified and later release states require the separately accepted
`SignedEnvelopeV1(update_package)` too. The package signature binds the SHA-256 of the complete canonical signed-manifest envelope,
including its signature, and both envelopes must agree on channel/platform/architecture/version/size/digest/
minimum-protocol/anti-rollback. It follows discover → bounded resumable download →
verify → stage → explicit foreground apply → applied or phase-specific reconcile/rollback-required/rolling-
back/rolled-back.
Exact `TURN-SIGNED-V1`/RFC-8785 verification, payload/package digest, parent-manifest binding, current store
revision, revocation/high-water, Team ID, platform, architecture, bundle id and downgrade checks all precede
stage and are repeated immediately before apply; code signing/notarisation is additional, not a substitute.
Apply reuses the live-state matrix above: it may preserve a compatible
daemon, refuses an incompatible daemon with live PTYs, and never silently stops/restarts
one. Discovery, offline/failure and a staged package leave every terminal usable; package
bytes and credentials are absent from logs, diagnostics and privacy export.
An exact-query retry returns the current intent; a different query while it is active or owns bytes refuses
before network. Before byte one, the daemon reserves the declared size in one combined ≤2-GiB owner-only
allocation shared by download temporary, downloaded, verified and staged states. Cancel/discard/replacement
must prove those bytes absent; crash retains the owner/chunk ledger. Terminal replacement reserves one of at
most 100 rich receipts and never discards independent signing, anti-rollback or operation replay fences.
Apply and rollback persist intent before filesystem replacement. A crash/timeout enters
`apply_reconcile_required` or `rollback_reconcile_required`; recovery inspects the exact installed bundle and
backup identities and never repeats either effect.

`turn --update-status [--socket PATH]` performs the same authenticated read without
opening a window or starting a companion. A version/protocol refusal is actionable and
terminal; the existing status bar displays the handshake's “which side is old” recovery
message rather than attempting a partially compatible session.

## Accepted M15 local dictation packaging

This is an ADR-060 target, not part of the current three-binary v0.1 bundle. M15 adds a
version-checked `turn-speech-worker` packaged sibling. Release preflight must include its Cargo/engine
version and hash in `release.plist`, sign it with the hardened runtime before the outer app and reject a
missing, mismatched, ad-hoc-substituted or independently downloaded helper. The worker must retain its
no-network/no-arbitrary-files sandbox in the signed package; debug/source launch is not evidence for that
boundary.

The macOS bundle declares one clear microphone usage string and requests consent only after a foreground
capture gesture. It contains the required hardened-runtime microphone entitlement for `turn-gui`, never for
the daemon/worker. Linux packaging records the audio backend and portal/session used. Third-party notices
include the inference engine and every catalogue model's licence/provenance before its installer is offered;
no model weights are silently bundled with Turn.

`make dictation-acceptance` must run deterministic fake-engine tests on both platforms. Packaged acceptance
then uses one catalogue-verified real local model and microphone to prove consent, visible capture, exact-
target draft, explicit Send, worker crash isolation, offline inference and zero network fallback. It scans
the package/data dir, protocol capture, logs, journal, diagnostics and crash artifacts for seeded PCM/draft
markers. Production release cannot advertise dictation until this packaged pass, VoiceOver/Orca checks and
model download/delete/privacy inventory all pass for that architecture.

## Accepted control-plane release matrix

ADR-064/065/066 are post-v0.1 targets and do not change the three-binary acceptance record below. A release that
advertises the successor control plane must extend package/preflight/live evidence as follows:

- a fresh packaged foreground Session is selected once under a current safe activation plan and restores/
  attaches or starts its exact bounded eligible saved-runtime set—or exactly one configured default Shell
  when empty—without a second action. A second fixture changes
  target/account/command/authority generation before selection and proves the package starts nothing and
  presents one exact recovery action;
- every shipped external WorkItemSource and agent adapter records implementation/version/capabilities in
  build diagnostics without embedding credentials. Package smokes cover bounded page/cursor gaps, source
  revision conflict and timeout reconciliation, and prove local projection deletion cannot close an external
  item;
- each advertised provider-native Job capability has a deterministic contract test plus a current
  authenticated smoke for all nine exact key/wire mappings, atomic Node/CreationId reservation, create/run-now
  correlation, definition privacy, independent state axes, exact reconciliation, projection fences and
  provider-dependent survival. Flow recurrence is separate, and terminal heuristics cannot advertise jobs;
- advertised ConversationInventory, title read and rename paths record the exact adapter/provider/profile/
  target versions used. Live smoke proves bounded history/search and stopped adoption separately from resume,
  and proves `title_read` and `conversation_rename` degrade independently;
- WebPreview and Browser use different packaged capability paths. The inert renderer's no-script/no-network/
  no-file/control boundary and the Browser's isolated partition, reviewed local/loopback origin and
  no-automatic-restore-load behaviour are tested in the sealed package. Any required rendering helper/engine
  identity and hash joins release metadata and signature verification rather than being downloaded silently;
- account context, quota and bounded activity-inbox projections are tested with two profiles plus missing,
  partial, stale, rate-limited and failed sources. Release UI must show those states and never false zero or
  cross-profile data;
- a full remote GUI, a headless status client and a companion are version-negotiated as distinct client
  classes. Release evidence proves the full GUI's scoped revision/input lease, the two bounded clients'
  smaller allowlists and fail-closed version skew independently. No package may describe a headless snapshot
  or companion as full remote control;
- typed permission is advertised only after all fact/transport states pass: distinct local and remote ops,
  E2EE grant delivery/ack, atomic consume, binary/non-binary option, race/crash/receipt-recovery and provider-
  evidence cases. Raw typed-state bytes, offline replay, widening and remote fallback fail; only fresh supported
  verified-local-PTY admits daemon-encoded desktop fallback. Credentials/admin/trust remain local.
- a packaged 128-level recursive Group fixture plus Session-owned CheckoutScope create/adopt/missing/
  reconcile/unbind/remove proves cycle/depth/corruption bounds, non-cascading tree changes, retained adopted
  worktrees and a continuously switchable primary `main`. No package may label Group itself a repository or
  runtime owner;
- the release adapter roster is exactly the six dedicated adapters, each with all 23 capability cells and
  current versioned/live evidence where claimed. Kimi/MiniMax remain quota-only connector entries. Every
  shipped ModelEndpointProfile path proves target-local secret resolution, bounded discovery, route/model
  receipt, failed-switch continuity and absence of secret canaries from package/config/argv/PTY/log/export;
- local and remote ResourceInventory smokes include host RAM/swap/pressure, reuse-safe current/closed/unmatched
  process attribution and complete-empty versus failed collection. Any packaged helper identity/hash is
  sealed, and an exact terminate test proves no sibling/PID-reuse or local-fallback effect;
- background notification support is advertised only after paired/revoked/offline/batched/dead-token and
  live-end/replay tests prove encrypted minimal payloads, no Attention resolution and no late resurrection.
  The notification-only service package/socket manifest is scanned at runtime and must expose zero public
  listener or inbound port; remote GUI helpers cannot be smuggled into that mode;
- New/Open/Clone/SSH onboarding in the package is cancelled/crashed at every phase and reconciles one exact
  preassigned operation without duplicate clone or implicit publish. Local/generated Node and Group naming
  proves revision/redaction/manual pinning and sends no provider/terminal rename; and
- packaged dense-tree snapshots exercise restore, collapse/filter, variable-height rows, resize/zoom and
  topology churn with the exact bounded row-gap/prefix-sum invariant and zero overlap, cleanup control, extra
  domain revision or focus/selection jump; and
- bounded Directory/DAG/TextSearch, Media, repository-host/proposal, TransferTicket and content-projection
  packaged paths hit their exact caps, crash/race states and sandbox/confinement checks while preserving the
  terminal-input and Attention budgets;
- all catalogue entry surfaces share one typed CommandCatalogue, all five signed-artifact domains pass
  noncanonical/wrong-store/rotation/revocation/replay tests, announcement content remains inert and update
  package verification proves the separate signature plus exact parent signed-manifest binding;
- WorkItem activity ordering/gap/echo-dedup and all eight presentation-only undo/redo operations, including
  constant-size expand-all, pass with
  proofs that excluded runtime/provider/SCM/input/context/Attention/credential/destructive effects are
  structurally absent; and
- the release commit passes the exact 112-row neutral capability ledger plus 185 PRD/ACP trace, selector-v1 source census and adversarial
  mutation suite. Before freezing a changed ledger, the generic source verifier recomputes the opaque snapshot
  tree, all 112 evidence-reference digests (covering 89 unique locator/digest pairs) and the full selected candidate set from an independent clone. The recorded authority root and protected
  repository pin must match the merged commit;
  deleting, weakening or marking a row unknown cannot be compensated by a product badge or manual smoke.

The release manifest records which of these capabilities passed for each architecture, adapter, provider and
client class. `unsupported`, `unknown`, stale live-smoke evidence or a missing helper remains visible and
cannot be converted into a product-wide supported badge by another provider's pass.

## Packaged Claude vertical

The final notarized archive uses the same bundle topology as the authenticated harness.
Extract the release asset, set the isolated `TURN_DATA_DIR`/`TURN_SOCKET`, open that
`Turn.app`, and run the two ignored tests and visible checklist in
[REVIEWER_ACCEPTANCE.md](REVIEWER_ACCEPTANCE.md). The first test must pass before the
window is closed; the second reconnects a new UI process to the unchanged daemon and
Claude PTY. Record the release tag, archive SHA-256, macOS version, architecture, Claude
version and PIDs in that document for each release candidate.

## Acceptance record — 2026-08-11

`make release-acceptance` passed on arm64 macOS against version 0.1.0/protocol 4. It
built the final three-binary layout, applied hardened-runtime ad-hoc signatures and
verified the sealed app plus both nested companions. Production Developer ID identity,
Apple notarization and both architecture archives are deliberately exercised only by
the tag workflow because their credentials are repository secrets; the workflow makes
notarization mandatory and cannot emit a release asset if it is skipped or rejected.
