# Gemini CLI, OpenCode, and external-app acceptance

Run the complete reproducible check without opening Turn:

```sh
make adapter-acceptance
```

This verifies the dedicated adapter selection, safe launch degradation, hook or
plugin configuration, normalized lifecycle/permission/question/failure events,
confidence and source attribution, resume arguments, and graphical-process tree
placement.

## Supported contracts

- Gemini CLI `0.46.0`: `crates/turn-agents/tests/fixtures/gemini-cli-0.46.0.json`
  follows the hook reference bundled with that release. Turn subscribes to
  `SessionStart`, `BeforeAgent`, `AfterAgent`, `BeforeModel`, `BeforeTool` for
  `ask_user`, `Notification`, and `SessionEnd`.
- OpenCode `1.18.16`: `crates/turn-agents/tests/fixtures/opencode-1.18.16.json`
  follows the event bridge and schema sources at Git tag `v1.18.16`. The fixture
  includes the tool's own `info.version` field.

The version is part of each fixture name and adapter contract constant. Do not
edit an old fixture in place for a new tool release: add a newly versioned file,
run the contract tests against both, then remove the old one only when support is
intentionally dropped.

## Live smoke test

The contract suite is offline and deterministic. Before changing a supported
version, repeat this live smoke in a disposable Turn Session using the real CLI:

1. Start `gemini`, send one prompt, provoke one `ask_user` question and one tool
   permission, then exit. Confirm the Agent moves active → question/permission →
   completed, reports its session id and model, and can resume.
2. Start `opencode`, send one prompt, provoke a question and permission, create a
   child session, then exit. Confirm the parent/child hierarchy and the same state
   transitions, then resume with the recorded session id.
3. From either agent launch Godot or Blender. Confirm an `EXTERNAL APP` child is
   shown under the launching process, selection leaves desktop focus untouched,
   and its inspector says the interface lives outside Turn.
4. Repeat with Gemini hooks disabled and with `opencode --pure`. Both CLIs must
   still start; the inspector must report inferred integration.
5. Stop `turnd` during a callback. The CLI must continue without a warning or an
   extra interaction.

Capture callback JSON from the disposable hook endpoint, remove user content and
secrets without changing field names or types, save it under the exact observed
tool version, and rerun `make adapter-acceptance`.

Primary contract references:

- Gemini CLI: <https://geminicli.com/docs/hooks/reference/>
- OpenCode plugins: <https://opencode.ai/docs/plugins/>
- OpenCode configuration merging: <https://opencode.ai/docs/config/>
- OpenCode CLI session flag: <https://opencode.ai/docs/cli/>
