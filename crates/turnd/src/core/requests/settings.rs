//! Settings: assembling the levels, resolving them, and writing one of them.
//!
//! The daemon is the only resolver. That is the general rule for every projection here — the
//! window renders and never decides — and it matters more for this one than for most, because
//! resolution *is* the feature: a client that applied the precedence order itself would be a
//! second implementation of it, and the symptom would be a preferences sheet showing one
//! value while the terminal used another.
//!
//! ## Which levels exist
//!
//! Not always five. A Session made without a Template has no Template level, and a request
//! with no Session named has only the Global one. What the window is told is the list that
//! exists, each with the owner id a write has to quote back — so a control offered at a level
//! is a control that can actually write there.
//!
//! ## The temporary level is not here
//!
//! It lives in the window and dies with it, which is the whole point of it. The daemon neither
//! stores it nor resolves it: a window that has one applies it on top of what it is told.
//! `SetSetting` refuses it rather than accepting and dropping it, because silently discarding
//! a write is worse than declining one.

use super::Answer;
use crate::core::Core;
use serde_json::Value;
use turn_core::ids::SessionId;
use turn_core::settings::{Catalogue, Layer, Refusal, Resolution, Scope, Settings};
use turn_proto::{ErrorCode, ProtoError, Response, SettingsEntry, SettingsLevel, SettingsView};

impl Core {
    /// Every preference in force for one Session, with where each value came from.
    pub(super) fn get_settings(&mut self, session_id: Option<SessionId>) -> Answer {
        let levels = self.settings_levels(session_id.as_ref())?;
        let layers = self.settings_layers(&levels)?;
        Ok(Response::Settings {
            settings: Box::new(self.resolve_settings(session_id, levels, &layers)),
        })
    }

    /// Records one preference at one level.
    pub(super) fn set_setting(
        &mut self,
        scope: Scope,
        owner_id: Option<String>,
        key: String,
        value: Value,
        now_ms: i64,
    ) -> Answer {
        let owner = owner_id.unwrap_or_default();
        // Checked before it is written, and by the catalogue rather than here: an unknown key
        // is refused because a typo that persisted would look exactly like a preference that
        // does not work, and a level a key does not belong to is refused because a control
        // offered there would be a control that lies.
        self.settings_catalogue()
            .check(&key, scope, &value)
            .map_err(refusal)?;
        if !scope.is_persistent() {
            return Err(ProtoError::refused(
                "A temporary override belongs to the window that made it",
            )
            .with_detail(
                "The daemon does not store one. Apply it in the window, or set it at the \
                 Session level to keep it.",
            ));
        }
        self.require_settings_owner(scope, &owner)?;
        self.store
            .setting_layers()
            .set(scope, &owner, &key, &value, now_ms)
            .map_err(super::workspaces::store)?;
        tracing::info!(scope = ?scope, owner = %owner, %key, "a preference was set");
        // Answered with the whole resolved set rather than an ack: one write can move what is
        // in force for a Session in a different Workspace, and a client patching its own copy
        // would drift from the daemon's answer.
        self.answer_settings_for_owner(scope, &owner)
    }

    /// Removes one level's opinion, so the level below is in force again.
    pub(super) fn reset_setting(
        &mut self,
        scope: Scope,
        owner_id: Option<String>,
        key: String,
    ) -> Answer {
        let owner = owner_id.unwrap_or_default();
        // Deliberately not checked against the catalogue. Resetting a key this build does not
        // define is exactly how a user gets rid of a value a newer Turn wrote, and refusing it
        // would leave them with a row they can see and cannot remove.
        let removed = self
            .store
            .setting_layers()
            .clear(scope, &owner, &key)
            .map_err(super::workspaces::store)?;
        if removed {
            tracing::info!(scope = ?scope, owner = %owner, %key, "a preference was reset to inherited");
        }
        self.answer_settings_for_owner(scope, &owner)
    }

    /// Turn's catalogue. A method rather than a constant so a future build can extend it per
    /// installed adapter without every caller learning about that.
    pub(crate) fn settings_catalogue(&self) -> Catalogue {
        Catalogue::built_in()
    }

    /// The value in force for one key in one Session, for the daemon's own use.
    ///
    /// This is what makes a preference a preference rather than a row in a table: the code
    /// that opens a shell, sizes a scrollback or decides whether to send a preview asks here.
    /// Failures resolve to the built-in default rather than propagating — a Session whose
    /// Workspace row will not load is a Session that should still open a terminal.
    pub(crate) fn setting_for(&self, session_id: Option<&SessionId>, key: &str) -> Value {
        let catalogue = self.settings_catalogue();
        let levels = self.settings_levels(session_id).unwrap_or_default();
        let layers = self.settings_layers(&levels).unwrap_or_default();
        Settings::new(&catalogue, layers.iter()).resolve(key).value
    }

    /// The levels that exist for this Session, weakest first.
    fn settings_levels(
        &self,
        session_id: Option<&SessionId>,
    ) -> std::result::Result<Vec<SettingsLevel>, ProtoError> {
        let mut levels = vec![SettingsLevel::global()];
        let Some(session_id) = session_id else {
            return Ok(levels);
        };
        let session = self.session(session_id)?;
        if let Ok(workspace) = self.workspace(&session.workspace_id) {
            levels.push(SettingsLevel::workspace(&workspace.id, &workspace.name));
        }
        // A Session made from no Template has no Template level. Offering one would be a
        // control writing to an owner that does not exist, and the write would be kept
        // against an id nothing resolves.
        if let Some(template_id) = session.template_id.as_ref() {
            if let Some(template) = self.templates.get(template_id) {
                levels.push(SettingsLevel::template(&template.id, &template.name));
            }
        }
        levels.push(SettingsLevel::session(&session.id, &session.name));
        Ok(levels)
    }

    /// Reads each level's stored opinion.
    fn settings_layers(
        &self,
        levels: &[SettingsLevel],
    ) -> std::result::Result<Vec<Layer>, ProtoError> {
        levels
            .iter()
            .map(|level| {
                self.store
                    .setting_layers()
                    .layer(level.scope, &level.owner_id)
                    .map_err(super::workspaces::store)
            })
            .collect()
    }

    /// Builds the projection the window renders.
    fn resolve_settings(
        &self,
        session_id: Option<SessionId>,
        levels: Vec<SettingsLevel>,
        layers: &[Layer],
    ) -> SettingsView {
        let catalogue = self.settings_catalogue();
        let settings = Settings::new(&catalogue, layers.iter());
        let existing: Vec<Scope> = levels.iter().map(|level| level.scope).collect();
        let entries = settings
            .resolve_all()
            .into_iter()
            .map(|resolution| entry_for(&catalogue, resolution, &existing))
            .collect();
        SettingsView {
            session_id,
            levels,
            entries,
        }
    }

    /// The answer to a write: resolved for a Session at that level when there is exactly one,
    /// and for the level alone otherwise.
    ///
    /// A Workspace with four Sessions has no single "the Session this changed", so what comes
    /// back is the Global-plus-that-level view. The window asks again for whichever Session it
    /// is showing; what it must never do is patch its own copy, because one write can move
    /// several keys at once.
    fn answer_settings_for_owner(&mut self, scope: Scope, owner: &str) -> Answer {
        let session_id = (scope == Scope::Session)
            .then(|| SessionId::from_stored(owner))
            .filter(|id| self.sessions.contains_key(id));
        self.get_settings(session_id)
    }

    /// Refuses a write to a level whose owner does not exist.
    ///
    /// Without this a typo in an id would be stored happily and resolve for nobody: the user
    /// would set a preference, see nothing change, and have no way to find the row.
    fn require_settings_owner(
        &self,
        scope: Scope,
        owner: &str,
    ) -> std::result::Result<(), ProtoError> {
        let known = match scope {
            Scope::Global => true,
            Scope::Workspace => self
                .workspaces
                .contains_key(&turn_core::ids::WorkspaceId::from_stored(owner)),
            Scope::Template => self
                .templates
                .contains_key(&turn_core::ids::TemplateId::from_stored(owner)),
            Scope::Session => self.sessions.contains_key(&SessionId::from_stored(owner)),
            // Refused earlier, on its own terms.
            Scope::Temporary => false,
        };
        if known {
            Ok(())
        } else {
            Err(ProtoError::not_found(
                match scope {
                    Scope::Workspace => "workspace",
                    Scope::Template => "template",
                    Scope::Session => "session",
                    _ => "settings owner",
                },
                owner,
            ))
        }
    }
}

/// One resolved preference, dressed for the window.
fn entry_for(catalogue: &Catalogue, resolution: Resolution, existing: &[Scope]) -> SettingsEntry {
    let definition = catalogue.get(&resolution.key);
    let hidden = resolution.must_be_hidden();
    // Redacted here, on the way out, rather than at rest: the daemon needs the real value in
    // order to use it, and this is the boundary past which nothing does.
    let shown = resolution.for_display();
    SettingsEntry {
        area: definition
            .map(|definition| definition.area)
            .unwrap_or(turn_core::settings::Area::Adapters),
        area_title: definition
            .map(|definition| definition.area.title().to_string())
            .unwrap_or_else(|| "Unrecognised".to_string()),
        title: definition
            .map(|definition| definition.title.to_string())
            // A key this build does not define is shown under its own name. The user cannot
            // be told what it means — nothing here knows — and inventing a friendly title
            // for it would be a guess presented as a fact.
            .unwrap_or_else(|| shown.key.clone()),
        description: definition
            .map(|definition| definition.description.to_string())
            .unwrap_or_else(|| {
                "Set by a newer version of Turn. This build does not know what it does, so it \
                 is shown as it was stored and can only be reset."
                    .to_string()
            }),
        accepts: definition
            .map(|definition| definition.kind.describe())
            .unwrap_or_else(|| "unknown".to_string()),
        settable_at: definition
            .map(|definition| {
                definition
                    .scopes
                    .iter()
                    .copied()
                    .filter(|scope| existing.contains(scope) || *scope == Scope::Temporary)
                    .collect()
            })
            .unwrap_or_default(),
        known: definition.is_some(),
        hidden,
        resolution: shown,
    }
}

/// A catalogue refusal, as the protocol error the window can act on.
fn refusal(refusal: Refusal) -> ProtoError {
    match &refusal {
        // Not found rather than refused: the key does not exist, which is a different repair
        // from "you may not do that here".
        Refusal::UnknownKey { key } => ProtoError::not_found("setting", key),
        Refusal::WrongScope { allowed, .. } => {
            ProtoError::new(ErrorCode::Refused, refusal.to_string()).with_detail(format!(
                "It can be set at: {}",
                allowed
                    .iter()
                    .map(|scope| scope.label())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        }
        Refusal::WrongShape { .. } => {
            ProtoError::new(ErrorCode::InvalidArgument, refusal.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::testing::Harness;
    use serde_json::json;
    use turn_core::ids::PaneId;
    use turn_proto::ErrorCode;

    const T0: i64 = 1_700_000_000_000;

    fn view(answer: Answer) -> SettingsView {
        match answer.expect("settings resolve") {
            Response::Settings { settings } => *settings,
            other => panic!("expected settings, got {other:?}"),
        }
    }

    /// The levels offered are the ones that exist, and no more.
    ///
    /// A Session made without a Template has no Template level. Offering one would be a
    /// control that writes against an id nothing resolves: the user would set a preference,
    /// see nothing change, and have no way to find the row afterwards.
    #[tokio::test]
    async fn only_the_levels_that_exist_are_offered() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_settings_levels");
        harness.add_session(
            session_id.clone(),
            PaneId::from_stored("pane_settings_levels"),
            T0,
        );

        let scopes: Vec<Scope> = view(harness.core.get_settings(Some(session_id)))
            .levels
            .iter()
            .map(|level| level.scope)
            .collect();
        assert_eq!(
            scopes,
            vec![Scope::Global, Scope::Workspace, Scope::Session],
            "no Template level for a Session that came from no Template"
        );

        // And with no Session named at all, only the Global level — which is the state the
        // preferences sheet opens in before anything is selected.
        let scopes: Vec<Scope> = view(harness.core.get_settings(None))
            .levels
            .iter()
            .map(|level| level.scope)
            .collect();
        assert_eq!(scopes, vec![Scope::Global]);
    }

    /// A write at one level is visible as the origin of the value, and the level below is
    /// reported as what resetting would reveal.
    #[tokio::test]
    async fn a_preference_says_which_level_it_came_from_and_what_it_shadowed() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_settings_origin");
        harness.add_session(
            session_id.clone(),
            PaneId::from_stored("pane_settings_origin"),
            T0,
        );
        let workspace_id = harness.core.sessions[&session_id]
            .workspace_id
            .as_str()
            .to_string();

        harness
            .core
            .set_setting(
                Scope::Workspace,
                Some(workspace_id),
                "appearance.font_size".into(),
                json!(13),
                T0,
            )
            .expect("a Workspace may set a font size");
        let answer = harness.core.set_setting(
            Scope::Session,
            Some(session_id.as_str().to_string()),
            "appearance.font_size".into(),
            json!(17),
            T0 + 1,
        );

        // The write answers with the whole resolved set, so the window never has to guess
        // what a change did to keys other than the one it wrote.
        let settings = view(answer);
        let entry = settings
            .entry("appearance.font_size")
            .expect("the key is in the catalogue");
        assert_eq!(entry.resolution.value, json!(17));
        assert_eq!(entry.resolution.origin, Some(Scope::Session));
        assert_eq!(
            entry
                .resolution
                .inherited()
                .map(|shadowed| shadowed.value.clone()),
            Some(json!(13)),
            "resetting the Session would reveal the Workspace's 13"
        );
    }

    /// Reset removes one level and the one below comes back, with the other level untouched.
    #[tokio::test]
    async fn resetting_one_level_leaves_every_other_level_alone() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_settings_reset");
        harness.add_session(
            session_id.clone(),
            PaneId::from_stored("pane_settings_reset"),
            T0,
        );
        let workspace_id = harness.core.sessions[&session_id]
            .workspace_id
            .as_str()
            .to_string();
        for (scope, owner, value) in [
            (Scope::Workspace, workspace_id.clone(), json!(13)),
            (Scope::Session, session_id.as_str().to_string(), json!(17)),
        ] {
            harness
                .core
                .set_setting(scope, Some(owner), "appearance.font_size".into(), value, T0)
                .expect("both levels accept a font size");
        }

        harness
            .core
            .reset_setting(
                Scope::Session,
                Some(session_id.as_str().to_string()),
                "appearance.font_size".into(),
            )
            .expect("resetting is always allowed");

        let settings = view(harness.core.get_settings(Some(session_id)));
        let entry = settings.entry("appearance.font_size").unwrap();
        assert_eq!(entry.resolution.value, json!(13));
        assert_eq!(
            entry.resolution.origin,
            Some(Scope::Workspace),
            "the Workspace's own value was never touched by the Session's reset"
        );
    }

    /// A value of the wrong shape is refused, and the message says what would be accepted.
    #[tokio::test]
    async fn a_bad_value_is_refused_before_it_is_stored() {
        let mut harness = Harness::new().await;
        let error = harness
            .core
            .set_setting(
                Scope::Global,
                None,
                "appearance.font_size".into(),
                json!("enormous"),
                T0,
            )
            .expect_err("a font size is a number");
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert!(
            error.message.contains("6 to 32"),
            "the refusal names the range: {}",
            error.message
        );
        // And nothing was written, so the sheet does not show a value the daemon rejected.
        let settings = view(harness.core.get_settings(None));
        assert_eq!(
            settings
                .entry("appearance.font_size")
                .unwrap()
                .resolution
                .origin,
            None
        );
    }

    /// A level a key does not belong to is refused, and the refusal says where it can go.
    #[tokio::test]
    async fn a_key_is_refused_at_a_level_it_does_not_belong_to() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_settings_scope");
        harness.add_session(
            session_id.clone(),
            PaneId::from_stored("pane_settings_scope"),
            T0,
        );
        let error = harness
            .core
            .set_setting(
                Scope::Session,
                Some(session_id.as_str().to_string()),
                "keyboard.bindings".into(),
                json!({}),
                T0,
            )
            .expect_err("a chord is the user's, not a Session's");
        assert_eq!(error.code, ErrorCode::Refused);
        assert!(
            error.detail.unwrap_or_default().contains("Global"),
            "the refusal says where it can be set instead"
        );
    }

    /// A write to a level whose owner does not exist is refused rather than kept.
    ///
    /// Otherwise a typo in an id is stored happily and resolves for nobody: the user sets a
    /// preference, sees nothing change, and cannot find the row to remove it.
    #[tokio::test]
    async fn a_write_to_a_level_that_does_not_exist_is_refused() {
        let mut harness = Harness::new().await;
        let error = harness
            .core
            .set_setting(
                Scope::Session,
                Some("sess_never_existed".into()),
                "appearance.font_size".into(),
                json!(15),
                T0,
            )
            .expect_err("there is no such Session");
        assert_eq!(error.code, ErrorCode::NotFound);
    }

    /// A temporary override is refused rather than accepted and dropped.
    #[tokio::test]
    async fn a_temporary_override_is_declined_rather_than_silently_discarded() {
        let mut harness = Harness::new().await;
        let error = harness
            .core
            .set_setting(
                Scope::Temporary,
                None,
                "appearance.font_size".into(),
                json!(24),
                T0,
            )
            .expect_err("the daemon does not hold a window's override");
        assert_eq!(error.code, ErrorCode::Refused);
    }

    /// An unknown key is refused on write and allowed on reset.
    ///
    /// The asymmetry is the point: refusing the write stops a typo from persisting, and
    /// allowing the reset is how a user removes a value a newer Turn wrote and this build
    /// cannot explain.
    #[tokio::test]
    async fn an_unknown_key_cannot_be_written_and_can_always_be_reset() {
        let mut harness = Harness::new().await;
        let error = harness
            .core
            .set_setting(
                Scope::Global,
                None,
                "apearance.font_size".into(),
                json!(13),
                T0,
            )
            .expect_err("a typo is not a preference");
        assert_eq!(error.code, ErrorCode::NotFound);

        harness
            .core
            .reset_setting(Scope::Global, None, "adapters.from_the_future".into())
            .expect("resetting anything at all is allowed");
    }

    /// A secret never reaches the projection the window renders.
    ///
    /// The daemon keeps the real value — it has to, in order to use it — and this is the
    /// boundary past which nothing does. What the sheet shows, and therefore what a
    /// screenshot of it shows, is the replacement.
    #[tokio::test]
    async fn a_secret_is_replaced_on_its_way_to_the_window() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_settings_secret");
        harness.add_session(
            session_id.clone(),
            PaneId::from_stored("pane_settings_secret"),
            T0,
        );
        let workspace_id = harness.core.sessions[&session_id]
            .workspace_id
            .as_str()
            .to_string();
        harness
            .core
            .set_setting(
                Scope::Workspace,
                Some(workspace_id),
                "environment.variables".into(),
                json!({"ORDINARY": "yes"}),
                T0,
            )
            .expect("a Workspace may hold environment variables");

        let settings = view(harness.core.get_settings(Some(session_id)));
        let entry = settings.entry("environment.variables").unwrap();
        assert!(entry.hidden, "the sheet has to know not to show it");
        assert_eq!(
            entry.resolution.value,
            json!(turn_core::settings::REDACTED),
            "and it arrives already replaced rather than trusted to be hidden"
        );
    }

    /// The setting is not a row in a table: it decides what the daemon runs.
    ///
    /// The shell is the case that proves the wiring, because it is read at launch and can be
    /// checked without starting anything: a Session-level `shell.command` beats its
    /// Workspace's, which beats the Workspace's own `default_shell` field.
    #[tokio::test]
    async fn the_resolved_shell_is_what_a_pane_would_actually_open() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_settings_shell");
        harness.add_session(
            session_id.clone(),
            PaneId::from_stored("pane_settings_shell"),
            T0,
        );
        let workspace_id = harness.core.sessions[&session_id]
            .workspace_id
            .as_str()
            .to_string();

        harness
            .core
            .set_setting(
                Scope::Workspace,
                Some(workspace_id),
                "shell.command".into(),
                json!("/bin/ksh"),
                T0,
            )
            .expect("a Workspace may name a shell");
        assert_eq!(harness.core.shell_for(Some(&session_id)), "/bin/ksh");

        harness
            .core
            .set_setting(
                Scope::Session,
                Some(session_id.as_str().to_string()),
                "shell.command".into(),
                json!("/bin/zsh"),
                T0 + 1,
            )
            .expect("and a Session may disagree");
        assert_eq!(
            harness.core.shell_for(Some(&session_id)),
            "/bin/zsh",
            "the narrower level wins where it is actually used, not only in the sheet"
        );

        // Reset, and the Workspace's answer is in force again — the proof that nothing
        // collapsed the two levels into one stored value.
        harness
            .core
            .reset_setting(
                Scope::Session,
                Some(session_id.as_str().to_string()),
                "shell.command".into(),
            )
            .expect("resetting is allowed");
        assert_eq!(harness.core.shell_for(Some(&session_id)), "/bin/ksh");
    }

    /// Every entry says which of the existing levels it can be set at, so a control is never
    /// offered where the write would be refused.
    #[tokio::test]
    async fn an_entry_only_offers_levels_a_write_would_be_accepted_at() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_settings_settable");
        harness.add_session(
            session_id.clone(),
            PaneId::from_stored("pane_settings_settable"),
            T0,
        );

        let settings = view(harness.core.get_settings(Some(session_id)));
        let bindings = settings.entry("keyboard.bindings").unwrap();
        assert_eq!(
            bindings.settable_at,
            vec![Scope::Global],
            "a chord is the user's own and belongs nowhere else"
        );

        let font = settings.entry("appearance.font_size").unwrap();
        assert!(
            !font.settable_at.contains(&Scope::Template),
            "there is no Template level here, so it must not be offered: {:?}",
            font.settable_at
        );
        assert!(font.settable_at.contains(&Scope::Session));
    }
}
