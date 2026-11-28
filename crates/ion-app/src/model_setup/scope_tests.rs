use super::*;
use crate::{CredentialStatus, ModelCycleDirection};
use std::fs;

fn choices() -> Vec<ModelChoice> {
    let store = ModelStore::new(PathBuf::new());
    [
        ("first", "one"),
        ("first", "two"),
        ("first", "vendor/name:high"),
        ("first", "bracketed[1m]"),
        ("first", "stars*literal"),
        ("first", "stars-otherliteral"),
        ("first", "🦀-甲"),
        ("second", "one"),
        ("second", "first/shadow"),
        ("second", "model-20260101"),
        ("second", "model-latest"),
        ("second", "first/one"),
        ("second", "first/*"),
    ]
    .into_iter()
    .map(|(provider, model)| ModelChoice {
        selected: store
            .resolve_saved(&SavedSelection {
                provider: provider.into(),
                model: model.into(),
                endpoint: Some("http://127.0.0.1:1/v1/chat/completions".into()),
                wire: Some(Wire::ChatCompletions),
                api_key_env: None,
                context_window_tokens: None,
                max_output_tokens: None,
                image_input: false,
            })
            .unwrap(),
        label: "Custom endpoint",
        credential: CredentialStatus::Missing,
    })
    .collect()
}

fn pattern(text: &str) -> ModelScopeEntry {
    ModelScopeEntry::Pattern(text.into())
}

fn literal(text: &str) -> ModelScopeEntry {
    ModelScopeEntry::Identity(text.parse().unwrap())
}

fn resolve(patterns: &[&str]) -> Result<Vec<ModelChoice>> {
    scope::resolve(
        &patterns.iter().map(|s| pattern(s)).collect::<Vec<_>>(),
        choices(),
    )
}

fn identities(choices: &[ModelChoice]) -> Vec<String> {
    choices
        .iter()
        .map(|choice| format!("{}/{}", choice.selected.provider, choice.selected.model))
        .collect()
}

#[test]
fn ordered_scope_expansion_deduplicates_and_preserves_literal_ids() {
    assert_eq!(
        identities(
            &resolve(&[
                "FIRST/TWO",
                "first/[ot]*",
                "first/one",
                "second/model",
                "first/vendor/name:high",
                "first/bracketed[1m]",
                "first/stars*literal",
                "first/🦀*"
            ])
            .unwrap()
        ),
        [
            "first/two",
            "first/one",
            "second/model-latest",
            "first/vendor/name:high",
            "first/bracketed[1m]",
            "first/stars*literal",
            "first/🦀-甲",
        ]
    );
    assert_eq!(
        identities(&resolve(&["first/???"]).unwrap()),
        ["first/one", "first/two"]
    );
    let all_first = resolve(&["first/**"]).unwrap();
    assert_eq!(all_first.len(), 7);
    assert!(
        all_first
            .iter()
            .all(|choice| choice.selected.provider == "first")
    );
    // Another provider's literal "first/*" must not hide this provider's glob.
    assert_eq!(
        identities(&resolve(&["first/*"]).unwrap()),
        [
            "first/one",
            "first/two",
            "first/bracketed[1m]",
            "first/stars*literal",
            "first/stars-otherliteral",
            "first/🦀-甲",
        ]
    );
    assert_eq!(
        identities(&resolve(&["second/first/**"]).unwrap()),
        ["second/first/shadow", "second/first/one", "second/first/*"]
    );
    assert_eq!(
        identities(&resolve(&["second/first/*"]).unwrap()),
        ["second/first/*"]
    );
    // A recognized provider prefix is authority, even if another route has
    // that complete namespaced ID. Literal IDs never enable provider fallback.
    assert_eq!(identities(&resolve(&["first/one"]).unwrap()), ["first/one"]);
    assert!(resolve(&["first/shadow"]).is_err());
    assert_eq!(
        identities(&resolve(&["second/first/one"]).unwrap()),
        ["second/first/one"]
    );
    for patterns in [
        &["custom"][..],
        &["endpoint"],
        &["one"],
        &["first/absent"],
        &["no-such-model*"],
        &[""],
        &["first/[unterminated"],
    ] {
        assert!(resolve(patterns).is_err(), "{patterns:?}");
    }
}

#[test]
fn repeated_route_entries_do_not_duplicate_scoped_identities() {
    let expected = identities(&choices());
    let mut repeated = choices();
    repeated.extend(choices());
    let scoped = scope::resolve(&[pattern("**"), literal("first/one")], repeated).unwrap();
    assert_eq!(identities(&scoped), expected);
}

#[test]
fn an_unavailable_scope_does_not_widen_cycling() {
    let mut all = choices();
    all[0].selected.requires_key = true;
    let scoped = scope::resolve(&[literal("first/one")], all).unwrap();
    assert_eq!(scoped.len(), 1, "scope retains a known missing-key choice");
    assert!(!scoped[0].is_ready());
    assert!(
        ModelCycleDirection::Forward
            .candidate(
                scoped,
                &ModelRef {
                    provider: "second".into(),
                    model: "one".into(),
                }
            )
            .is_none()
    );
}

#[test]
fn configured_and_invocation_scopes_keep_selection_and_default_ownership() {
    let root = std::env::temp_dir().join(format!("ion-model-scope-{}", uuid::Uuid::now_v7()));
    fs::create_dir_all(&root).unwrap();
    let store = ModelStore::new(root.clone());
    let credentials = CredentialStore::new(root.join("keys"));
    let mut configuration = ModelConfiguration {
        custom: choices()
            .into_iter()
            .map(|choice| SavedSelection {
                provider: choice.selected.provider,
                model: choice.selected.model,
                endpoint: Some(choice.selected.endpoint),
                wire: Some(Wire::ChatCompletions),
                api_key_env: None,
                context_window_tokens: None,
                max_output_tokens: None,
                image_input: false,
            })
            .collect(),
        default: Some(ModelRef {
            provider: "second".into(),
            model: "one".into(),
        }),
        scope: Some(vec![pattern("first/two"), pattern("first/one")]),
    };
    let path = root.join("models.json");
    write_json(&path, &configuration, Visibility::Private).unwrap();
    let original = fs::read(&path).unwrap();
    let choose = |store: &ModelStore, explicit, previous| {
        store
            .choose(explicit, previous, &credentials)
            .unwrap()
            .identity()
    };
    let identity = |provider: &str, model: &str| ModelRef {
        provider: provider.into(),
        model: model.into(),
    };
    assert_eq!(choose(&store, None, None), identity("first", "two"));
    assert_eq!(
        choose(&store, None, Some(identity("second", "one"))),
        identity("second", "one")
    );
    assert_eq!(
        choose(&store, Some(identity("second", "one")), None),
        identity("second", "one")
    );
    assert_eq!(
        identities(&store.cycle_choices(&credentials).unwrap()),
        ["first/two", "first/one"]
    );
    let snapshot = store.selection_choices(&credentials).unwrap();
    assert_eq!(
        snapshot.preferred.unwrap(),
        [identity("first", "two"), identity("first", "one")]
    );
    assert!(
        snapshot
            .all
            .iter()
            .any(|choice| choice.selected.identity() == identity("second", "one"))
    );
    let overridden = ModelStore::new(root.clone()).with_scope(vec!["first/one".into()]);
    assert_eq!(
        overridden
            .selection_choices(&credentials)
            .unwrap()
            .preferred
            .unwrap(),
        [identity("first", "one")]
    );
    assert_eq!(choose(&overridden, None, None), identity("first", "one"));
    let cleared = ModelStore::new(root.clone()).with_scope(Vec::new());
    assert_eq!(choose(&cleared, None, None), identity("second", "one"));
    assert!(
        cleared
            .selection_choices(&credentials)
            .unwrap()
            .preferred
            .is_none()
    );
    assert_eq!(fs::read(&path).unwrap(), original);
    configuration.default = Some(identity("first", "one"));
    write_json(&path, &configuration, Visibility::Private).unwrap();
    assert_eq!(choose(&store, None, None), identity("first", "one"));
    configuration.scope = Some(vec![pattern("no-such-model")]);
    write_json(&path, &configuration, Visibility::Private).unwrap();
    assert!(store.choose(None, None, &credentials).is_err());
    assert!(store.cycle_choices(&credentials).is_err());
    assert!(store.selection_choices(&credentials).is_err());
    assert!(
        store.choices(&credentials).is_ok(),
        "listing all routes can diagnose an invalid scope"
    );
    assert_eq!(
        choose(&store, None, Some(identity("second", "one"))),
        identity("second", "one")
    );
    configuration.scope = Some(vec![pattern("first/*")]);
    write_json(&path, &configuration, Visibility::Private).unwrap();
    store.save_default(&configuration.custom[7]).unwrap();
    let saved = store.configuration().unwrap();
    assert_eq!(
        saved.scope.unwrap(),
        [pattern("first/*"), literal("second/one")]
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn checklist_save_pins_missing_identities_and_leaves_invocation_and_routes_intact() {
    let root = std::env::temp_dir().join(format!("ion-save-scope-{}", uuid::Uuid::now_v7()));
    let store = ModelStore::new(root.clone());
    let credentials = CredentialStore::new(root.join("credentials"));
    for model in ["one", "one-new", "two"] {
        store
            .save_default(&SavedSelection {
                provider: "desktop".into(),
                model: model.into(),
                endpoint: Some("http://127.0.0.1:1/v1/chat/completions".into()),
                wire: Some(Wire::ChatCompletions),
                api_key_env: None,
                context_window_tokens: None,
                max_output_tokens: None,
                image_input: false,
            })
            .unwrap();
    }
    let path = root.join("models.json");
    let mut configuration = store.configuration().unwrap();
    configuration.scope = Some(vec![
        pattern("desktop/one-n"),
        pattern("desktop/t*"),
        pattern("not-known*"),
    ]);
    write_json(&path, &configuration, Visibility::Private).unwrap();
    let overridden = ModelStore::new(root.clone()).with_scope(vec!["desktop/one-n".into()]);
    let snapshot = overridden.scope_choices(&credentials).unwrap();
    assert!(snapshot.invocation_override);
    assert_eq!(
        snapshot.saved.unwrap(),
        [
            literal("desktop/one-n"),
            literal("desktop/two"),
            pattern("not-known*")
        ]
    );
    assert!(store.cycle_choices(&credentials).is_err());
    let reopened = ModelStore::new(root.clone());
    // A custom ID containing a glob metacharacter is not a pattern, even while absent.
    let ordered = vec![
        literal("desktop/one*"),
        literal("desktop/one-n"),
        literal("desktop/two"),
        literal("desktop/one"),
    ];
    overridden.save_scope(Some(ordered.clone())).unwrap();
    assert_eq!(
        reopened.scope_choices(&credentials).unwrap().saved.unwrap(),
        ordered
    );
    assert_eq!(
        identities(&reopened.cycle_choices(&credentials).unwrap()),
        ["desktop/two", "desktop/one"]
    );
    assert_eq!(
        identities(&overridden.cycle_choices(&credentials).unwrap()),
        ["desktop/one-new"]
    );
    let after = store.configuration().unwrap();
    assert_eq!(after.default, configuration.default);
    assert_eq!(
        serde_json::to_value(&after.custom).unwrap(),
        serde_json::to_value(&configuration.custom).unwrap()
    );

    store.save_scope(Some(Vec::new())).unwrap();
    assert!(store.cycle_choices(&credentials).unwrap().is_empty());
    assert_eq!(
        store.selection_choices(&credentials).unwrap().preferred,
        Some(Vec::new())
    );
    assert!(
        ModelStore::new(root.clone())
            .with_scope(Vec::new())
            .selection_choices(&credentials)
            .unwrap()
            .preferred
            .is_none()
    );
    store.save_scope(None).unwrap();
    assert!(
        reopened
            .scope_choices(&credentials)
            .unwrap()
            .saved
            .is_none()
    );
    assert_eq!(
        reopened.choose(None, None, &credentials).unwrap().model,
        "two"
    );
    fs::remove_dir_all(root).unwrap();
}
