use super::*;
use crate::{CredentialStatus, ModelCycleDirection};

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

fn resolve(patterns: &[&str]) -> Result<Vec<ModelChoice>> {
    scope::resolve(
        &patterns.iter().map(|s| (*s).into()).collect::<Vec<_>>(),
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
    let scoped = scope::resolve(&["**".into(), "first/one".into()], repeated).unwrap();
    assert_eq!(identities(&scoped), expected);
}

#[test]
fn an_unavailable_scope_does_not_widen_cycling() {
    let mut all = choices();
    all[0].selected.requires_key = true;
    let scoped = scope::resolve(&["first/one".into()], all).unwrap();
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
        scope: Some(vec!["first/two".into(), "first/one".into()]),
    };
    let path = root.join("models.json");
    write_json(&path, &configuration).unwrap();
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
    let overridden = ModelStore::new(root.clone()).with_scope(vec!["first/one".into()]);
    assert_eq!(choose(&overridden, None, None), identity("first", "one"));
    let cleared = ModelStore::new(root.clone()).with_scope(Vec::new());
    assert_eq!(choose(&cleared, None, None), identity("second", "one"));
    assert_eq!(fs::read(&path).unwrap(), original);
    configuration.default = Some(identity("first", "one"));
    write_json(&path, &configuration).unwrap();
    assert_eq!(choose(&store, None, None), identity("first", "one"));
    configuration.scope = Some(vec!["no-such-model".into()]);
    write_json(&path, &configuration).unwrap();
    assert!(store.choose(None, None, &credentials).is_err());
    assert!(store.cycle_choices(&credentials).is_err());
    assert_eq!(
        choose(&store, None, Some(identity("second", "one"))),
        identity("second", "one")
    );
    configuration.scope = Some(vec!["first/*".into()]);
    write_json(&path, &configuration).unwrap();
    store.save_default(&configuration.custom[7]).unwrap();
    let saved = store.configuration().unwrap();
    assert_eq!(saved.scope.unwrap(), ["first/*", "second/one"]);
    fs::remove_dir_all(root).unwrap();
}
