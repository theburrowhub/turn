//! The acceptance criteria of the settings hierarchy, as executable statements.

use super::*;
use serde_json::json;

fn layer(scope: Scope, pairs: &[(&str, Value)]) -> Layer {
    Layer::from_pairs(
        scope,
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone())),
    )
}

/// Nothing set anywhere is the built-in default, and says so.
///
/// `origin: None` rather than `Some(Global)` is the load-bearing half: a surface offering
/// "reset to inherited" needs to know there is nothing to reset, and a value equal to the
/// default is not evidence of that — somebody may have set it globally to the same thing.
#[test]
fn a_key_nobody_set_answers_with_the_built_in_default_and_admits_it() {
    let catalogue = Catalogue::built_in();
    let settings = Settings::new(&catalogue, []);

    let resolved = settings.resolve("appearance.font_size");
    assert_eq!(resolved.value, json!(13));
    assert_eq!(resolved.origin, None);
    assert!(!resolved.is_overridden());
    assert!(resolved.shadowed.is_empty());
}

/// The five levels, in the order the product specifies.
#[test]
fn the_narrowest_level_that_has_an_opinion_wins() {
    let catalogue = Catalogue::built_in();
    let global = layer(Scope::Global, &[("appearance.font_size", json!(11))]);
    let workspace = layer(Scope::Workspace, &[("appearance.font_size", json!(13))]);
    let template = layer(Scope::Template, &[("appearance.font_size", json!(15))]);
    let session = layer(Scope::Session, &[("appearance.font_size", json!(17))]);
    let temporary = layer(Scope::Temporary, &[("appearance.font_size", json!(19))]);

    // Deliberately out of order: the resolver sorts, because the caller assembles these from
    // four unrelated places and must not have to remember which.
    let settings = Settings::new(
        &catalogue,
        [&session, &global, &temporary, &template, &workspace],
    );
    let resolved = settings.resolve("appearance.font_size");

    assert_eq!(resolved.value, json!(19));
    assert_eq!(resolved.origin, Some(Scope::Temporary));
    assert_eq!(
        resolved
            .shadowed
            .iter()
            .map(|shadowed| (shadowed.scope, shadowed.value.clone()))
            .collect::<Vec<_>>(),
        vec![
            (Scope::Global, json!(11)),
            (Scope::Workspace, json!(13)),
            (Scope::Template, json!(15)),
            (Scope::Session, json!(17)),
        ],
        "and the whole chain is reported, weakest first, so the surface can say what \
         resetting would reveal"
    );
    assert_eq!(
        resolved.inherited().map(|shadowed| shadowed.scope),
        Some(Scope::Session),
        "resetting the window's override reveals the Session's"
    );
}

/// Levels with no opinion are skipped rather than counted as empty.
#[test]
fn a_level_with_nothing_to_say_does_not_shadow_anything() {
    let catalogue = Catalogue::built_in();
    let global = layer(Scope::Global, &[("appearance.font_size", json!(11))]);
    let session = layer(Scope::Session, &[("appearance.cursor", json!("bar"))]);
    let settings = Settings::new(&catalogue, [&global, &session]);

    let resolved = settings.resolve("appearance.font_size");
    assert_eq!(resolved.value, json!(11));
    assert_eq!(resolved.origin, Some(Scope::Global));
    assert!(
        resolved.shadowed.is_empty(),
        "the Session had no opinion about the font size: {:?}",
        resolved.shadowed
    );
}

/// **The criterion this whole design exists for.** Changing one level cannot disturb another.
///
/// The failure it rules out is storing the resolved answer: a Workspace that wrote "13"
/// because that was the effective value would swallow the Session's 17, and the user would
/// find out the next time they touched the Workspace. Layers are separate maps, so a write is
/// one entry in one map.
#[test]
fn changing_a_level_never_destroys_an_override_below_it() {
    let catalogue = Catalogue::built_in();
    let mut workspace = layer(Scope::Workspace, &[("appearance.font_size", json!(13))]);
    let session = layer(Scope::Session, &[("appearance.font_size", json!(17))]);

    // The user edits the Workspace while a Session override is in force.
    workspace.set("appearance.font_size", json!(9));

    let settings = Settings::new(&catalogue, [&workspace, &session]);
    let resolved = settings.resolve("appearance.font_size");
    assert_eq!(
        resolved.value,
        json!(17),
        "the Session still wins, because nothing merged the two"
    );
    assert_eq!(
        session.get("appearance.font_size"),
        Some(&json!(17)),
        "and its own record is untouched"
    );
    assert_eq!(
        resolved.inherited().map(|shadowed| shadowed.value.clone()),
        Some(json!(9)),
        "while the edited Workspace value is what resetting the Session would now reveal"
    );
}

/// Reset is a removal, and the level below comes back.
///
/// Removal rather than writing the inherited value: writing it would freeze today's inherited
/// answer as tomorrow's override, and the user would have "reset" a value into permanence.
#[test]
fn resetting_a_level_reveals_the_one_below_rather_than_copying_it() {
    let catalogue = Catalogue::built_in();
    let workspace = layer(Scope::Workspace, &[("appearance.font_size", json!(13))]);
    let mut session = layer(Scope::Session, &[("appearance.font_size", json!(17))]);

    assert!(
        session.clear("appearance.font_size"),
        "there was something to reset"
    );
    let settings = Settings::new(&catalogue, [&workspace, &session]);
    assert_eq!(
        settings.resolve("appearance.font_size").origin,
        Some(Scope::Workspace)
    );

    // And resetting the last level standing goes all the way back to the built-in default,
    // not to the empty value the layer used to hold.
    let mut workspace = workspace;
    workspace.clear("appearance.font_size");
    let settings = Settings::new(&catalogue, [&workspace, &session]);
    let resolved = settings.resolve("appearance.font_size");
    assert_eq!(resolved.origin, None);
    assert_eq!(resolved.value, json!(13), "Turn's own default");
}

/// Resetting something nobody set is not an error and changes nothing.
#[test]
fn resetting_a_level_that_had_no_opinion_is_harmless() {
    let mut session = Layer::new(Scope::Session);
    assert!(!session.clear("appearance.font_size"));
    assert!(session.is_empty());
}

/// An explicit null overrides an inherited value with nothing, which a removal cannot do.
///
/// The case: a Workspace with three init commands, and one Session that must run none. Absent
/// would inherit the three; `null` says no.
#[test]
fn a_deliberate_nothing_is_different_from_having_no_opinion() {
    let catalogue = Catalogue::built_in();
    let workspace = layer(
        Scope::Workspace,
        &[("environment.init_commands", json!(["make dev"]))],
    );
    let mut session = layer(
        Scope::Session,
        &[("environment.init_commands", Value::Null)],
    );

    let settings = Settings::new(&catalogue, [&workspace, &session]);
    let resolved = settings.resolve("environment.init_commands");
    assert_eq!(resolved.value, Value::Null);
    assert_eq!(resolved.origin, Some(Scope::Session));

    // And removing that null is what restores the inheritance.
    session.clear("environment.init_commands");
    let settings = Settings::new(&catalogue, [&workspace, &session]);
    assert_eq!(
        settings.resolve("environment.init_commands").value,
        json!(["make dev"])
    );
}

/// A secret resolves normally — the daemon needs it — and never renders.
#[test]
fn a_secret_is_usable_by_the_daemon_and_invisible_to_everything_that_shows_it() {
    let catalogue = Catalogue::built_in();
    let workspace = layer(
        Scope::Workspace,
        &[("environment.variables", json!({"API_TOKEN": "sk-live-42"}))],
    );
    let session = layer(
        Scope::Session,
        &[("environment.variables", json!({"API_TOKEN": "sk-live-99"}))],
    );
    let settings = Settings::new(&catalogue, [&workspace, &session]);
    let resolved = settings.resolve("environment.variables");

    assert_eq!(resolved.sensitivity, Sensitivity::Secret);
    assert_eq!(
        resolved.value,
        json!({"API_TOKEN": "sk-live-99"}),
        "the value itself is intact: a token redacted on the way in is a token the daemon \
         cannot use"
    );

    let shown = resolved.for_display();
    let rendered = format!("{shown:?}");
    assert!(
        !rendered.contains("sk-live-99") && !rendered.contains("sk-live-42"),
        "neither the winner nor the shadowed value may survive rendering: {rendered}"
    );
    assert!(
        rendered.contains(REDACTED),
        "and the user is told there is something there: {rendered}"
    );
    assert_eq!(
        shown.origin,
        Some(Scope::Session),
        "which level set it is not itself a secret, and hiding it would make the sheet a lie"
    );
}

/// A plain value is not touched by the display form.
#[test]
fn a_plain_value_survives_being_displayed() {
    let catalogue = Catalogue::built_in();
    let session = layer(Scope::Session, &[("appearance.cursor", json!("bar"))]);
    let settings = Settings::new(&catalogue, [&session]);
    assert_eq!(
        settings.resolve("appearance.cursor").for_display().value,
        json!("bar")
    );
}

/// A key from a newer build survives, and is treated as a secret because nothing is known
/// about it.
///
/// The alternative — dropping it — is a downgrade that silently deletes the user's settings,
/// and the symptom is a value that comes back when they upgrade again.
#[test]
fn a_key_this_build_does_not_know_is_kept_and_never_shown() {
    let catalogue = Catalogue::built_in();
    let session = layer(
        Scope::Session,
        &[("adapters.gemini.token", json!("from-a-newer-build"))],
    );
    let settings = Settings::new(&catalogue, [&session]);

    let resolved = settings.resolve("adapters.gemini.token");
    assert_eq!(resolved.value, json!("from-a-newer-build"));
    assert_eq!(resolved.sensitivity, Sensitivity::Unknown);
    assert!(
        !format!("{:?}", resolved.for_display()).contains("from-a-newer-build"),
        "unknown means unknown: it may be a token this build has never heard of"
    );

    assert!(
        settings
            .resolve_all()
            .iter()
            .any(|resolution| resolution.key == "adapters.gemini.token"),
        "and it appears in the list, so the user can see and delete it"
    );
}

/// Writes are checked where they happen.
#[test]
fn a_value_of_the_wrong_shape_is_refused_with_a_sentence_the_user_can_act_on() {
    let catalogue = Catalogue::built_in();

    let refusal = catalogue
        .check("appearance.font_size", Scope::Session, &json!("large"))
        .expect_err("a font size is a number");
    assert!(
        matches!(refusal, Refusal::WrongShape { .. }),
        "got {refusal:?}"
    );
    assert!(
        refusal.to_string().contains("6 to 32"),
        "the message names the range: {refusal}"
    );

    assert!(
        catalogue
            .check("appearance.font_size", Scope::Session, &json!(400))
            .is_err(),
        "a bound is a bound in both directions"
    );
    assert!(catalogue
        .check("appearance.font_size", Scope::Session, &json!(18))
        .is_ok());
}

/// `null` is accepted for every kind, because that is how a level says "nothing here".
#[test]
fn nothing_is_a_legal_value_for_every_kind_of_setting() {
    let catalogue = Catalogue::built_in();
    for definition in catalogue.all() {
        assert!(
            definition.kind.accepts(&Value::Null),
            "{} must accept a deliberate absence",
            definition.key
        );
    }
}

/// A preference that only makes sense as the user's own cannot be set per Session.
///
/// Keyboard bindings are the case: five places to look for one chord is worse than one place,
/// and a Session-level binding would be invisible from the Session the user is in.
#[test]
fn a_setting_is_refused_at_a_level_it_does_not_belong_to() {
    let catalogue = Catalogue::built_in();
    let refusal = catalogue
        .check("keyboard.bindings", Scope::Session, &json!({}))
        .expect_err("a chord is the user's, not a Session's");
    assert!(
        matches!(refusal, Refusal::WrongScope { .. }),
        "got {refusal:?}"
    );
    assert!(
        refusal.to_string().contains("Session"),
        "and it names the level that was refused: {refusal}"
    );
    assert!(catalogue
        .check("keyboard.bindings", Scope::Global, &json!({}))
        .is_ok());
}

/// A key nothing defines cannot be written. A typo that persisted would look exactly like a
/// preference that does not work.
#[test]
fn a_key_nothing_defines_cannot_be_written() {
    let catalogue = Catalogue::built_in();
    assert!(matches!(
        catalogue.check("apearance.font_size", Scope::Global, &json!(13)),
        Err(Refusal::UnknownKey { .. })
    ));
}

/// Every definition's own default satisfies its own kind.
///
/// The tripwire for adding a preference: a default outside its own bounds would be refused by
/// the writer the moment a user tried to set it back to it.
#[test]
fn every_default_is_a_value_its_own_setting_would_accept() {
    let catalogue = Catalogue::built_in();
    for definition in catalogue.all() {
        assert!(
            definition.kind.accepts(&definition.default),
            "{}'s default {:?} is not {}",
            definition.key,
            definition.default,
            definition.kind.describe()
        );
    }
}

/// Every definition can be written at at least one level, and `Temporary` is never the only
/// one — a preference that could only be set temporarily could never be saved.
#[test]
fn every_setting_can_be_saved_somewhere() {
    let catalogue = Catalogue::built_in();
    for definition in catalogue.all() {
        assert!(
            definition.scopes.iter().any(|scope| scope.is_persistent()),
            "{} can only be set temporarily, so the user could never keep it",
            definition.key
        );
    }
}

/// Keys are unique. Two definitions for one key would make `get` return whichever came first
/// and the other one dead.
#[test]
fn no_key_is_defined_twice() {
    let catalogue = Catalogue::built_in();
    let mut keys: Vec<&str> = catalogue.keys().collect();
    let count = keys.len();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(
        count,
        keys.len(),
        "a duplicate key hides one of its definitions"
    );
}

/// Every key is namespaced by its area, so a settings file is readable on its own and two
/// areas cannot collide over a short name like "font_size".
#[test]
fn every_key_is_namespaced() {
    let catalogue = Catalogue::built_in();
    for definition in catalogue.all() {
        assert!(
            definition.key.contains('.'),
            "{} needs an area prefix",
            definition.key
        );
    }
}

/// Resolving by area gives a settings sheet its sections without it having to know the keys.
#[test]
fn an_area_resolves_as_a_group() {
    let catalogue = Catalogue::built_in();
    let session = layer(Scope::Session, &[("appearance.cursor", json!("underline"))]);
    let settings = Settings::new(&catalogue, [&session]);

    let appearance = settings.resolve_area(Area::Appearance);
    assert!(
        appearance.len() >= 2,
        "the appearance section is not empty: {appearance:?}"
    );
    assert!(appearance
        .iter()
        .all(|resolution| resolution.key.starts_with("appearance.")));
    assert_eq!(
        appearance
            .iter()
            .find(|resolution| resolution.key == "appearance.cursor")
            .map(|resolution| resolution.origin),
        Some(Some(Scope::Session))
    );
}

/// Every area in the product's specification exists, including the ones with no keys yet.
///
/// An empty section is honest — it says the section exists and has nothing in it — and the
/// alternative is a catalogue full of controls that nothing reads.
#[test]
fn every_area_has_a_title_and_the_list_is_complete() {
    assert_eq!(Area::ALL.len(), 10);
    for area in Area::ALL {
        assert!(!area.title().is_empty());
    }
    let catalogue = Catalogue::built_in();
    for definition in catalogue.all() {
        assert!(
            Area::ALL.contains(&definition.area),
            "{} is in an area that is not in the list",
            definition.key
        );
    }
}

/// The one scope that does not survive the window says so, and the other four do.
#[test]
fn only_a_temporary_override_is_forgotten() {
    for scope in Scope::ALL {
        assert_eq!(scope.is_persistent(), scope != Scope::Temporary);
        assert!(!scope.label().is_empty());
    }
}

/// Precedence is the enum's own order, so a new scope cannot be inserted without deciding
/// where it goes.
#[test]
fn the_scopes_are_ordered_weakest_first() {
    let mut sorted = Scope::ALL;
    sorted.sort();
    assert_eq!(sorted, Scope::ALL);
}
