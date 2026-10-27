use super::*;

fn parse(args: &[&str], interactive: bool) -> Result<Invocation> {
    normalize(command().try_get_matches_from(args)?, interactive)
}

#[test]
fn completion_policy_is_explicit_scoped_and_defaults_to_inbox() {
    use ion_core::CompletionDelivery;
    assert_eq!(
        parse(&["ion", "rpc"], false).unwrap().completion_delivery,
        CompletionDelivery::Inbox
    );
    for policy in CompletionDelivery::ALL {
        for args in [
            vec![
                "ion",
                "--completion-delivery",
                policy.as_str(),
                "run",
                "task",
            ],
            vec!["ion", "rpc", "--completion-delivery", policy.as_str()],
        ] {
            assert_eq!(parse(&args, false).unwrap().completion_delivery, policy);
        }
    }
    for args in [
        vec!["ion", "--completion-delivery", "wake", "inspect"],
        vec!["ion", "usage", "--completion-delivery", "wake"],
        vec!["ion", "--completion-delivery", "always", "chat"],
    ] {
        assert!(parse(&args, true).is_err(), "{args:?}");
    }
}

#[test]
fn model_patterns_preserve_provider_scope_namespaces_and_literal_suffixes() {
    for (args, provider, text) in [
        (
            vec!["ion", "--model", "openrouter/vendor/model", "run", "hello"],
            None,
            "openrouter/vendor/model",
        ),
        (
            vec!["ion", "run", "hello", "--model", "openrouter/vendor/model"],
            None,
            "openrouter/vendor/model",
        ),
        (
            vec![
                "ion",
                "--provider",
                "openrouter",
                "run",
                "--model",
                "vendor/model:high",
                "hello",
            ],
            Some("openrouter"),
            "vendor/model:high",
        ),
        (
            vec!["ion", "--model", "Friendly model"],
            None,
            "Friendly model",
        ),
    ] {
        assert_eq!(
            parse(&args, true).unwrap().model.unwrap(),
            ModelPattern {
                provider: provider.map(str::to_owned),
                text: text.into(),
            }
        );
    }
    for args in [
        vec!["ion", "--provider", "openai"],
        vec!["ion", "--model", ""],
        vec!["ion", "--model", "  "],
        vec!["ion", "--provider", "", "--model", "model"],
    ] {
        assert!(parse(&args, true).is_err(), "{args:?}");
    }
}

#[test]
fn headless_modes_share_one_action_and_explicit_chat_stays_explicit() {
    for (args, interactive, expected) in [
        (vec!["ion", "-p"], true, None),
        (vec!["ion", "-p", "hello"], true, Some("hello")),
        (
            vec!["ion", "-p", "--model", "first/vendor/model", "hello"],
            true,
            Some("hello"),
        ),
        (
            vec!["ion", "-p", "--", "- Summarize these points"],
            true,
            Some("- Summarize these points"),
        ),
        (vec!["ion", "run"], true, None),
        (vec!["ion", "run", "hello"], true, Some("hello")),
        (vec!["ion"], false, None),
        (
            vec!["ion", "  bare 🦀 prompt\n"],
            false,
            Some("  bare 🦀 prompt\n"),
        ),
    ] {
        let cli = parse(&args, interactive).unwrap();
        let Action::Run { input } = cli.action else {
            panic!("expected headless action")
        };
        assert_eq!(
            input.prompts().collect::<Vec<_>>(),
            expected.into_iter().collect::<Vec<_>>()
        );
    }
    assert!(matches!(
        parse(&["ion"], true).unwrap().action,
        Action::Chat { input } if input.prompts().next().is_none()
    ));
    assert!(matches!(
        parse(&["ion", "chat"], false).unwrap().action,
        Action::Chat { input } if input.prompts().next().is_none()
    ));
    assert!(matches!(
        parse(&["ion", "rpc"], false).unwrap().action,
        Action::Rpc
    ));
    assert!(parse(&["ion", "-p", "hello", "auth"], true).is_err());
}

#[test]
fn file_operands_are_explicit_and_literal_escape_preserves_prompt_order() {
    for prefix in [vec!["ion"], vec!["ion", "chat"], vec!["ion", "run"]] {
        let mut args = prefix.clone();
        args.extend(["@first.md", "review 🦀", "@image.png", "check"]);
        let cli = parse(&args, true).unwrap();
        let (Action::Run { input } | Action::Chat { input }) = cli.action else {
            panic!("launch")
        };
        assert_eq!(input.prompts().collect::<Vec<_>>(), ["review 🦀", "check"]);
        assert_eq!(
            input.files().collect::<Vec<_>>(),
            vec![
                std::path::Path::new("first.md"),
                std::path::Path::new("image.png")
            ]
        );

        let mut args = prefix;
        args.extend(["@first.md", "before", "--", "@literal", "usage"]);
        let cli = parse(&args, true).unwrap();
        let (Action::Run { input } | Action::Chat { input }) = cli.action else {
            panic!("launch")
        };
        assert_eq!(
            input.prompts().collect::<Vec<_>>(),
            ["before", "@literal", "usage"]
        );
        assert_eq!(input.files().count(), 1);
    }
    for args in [
        vec!["ion", "@"],
        vec!["ion", "@file", "inspect"],
        vec!["ion", "usage", "@file"],
    ] {
        assert!(parse(&args, true).is_err(), "{args:?}");
    }
}

#[test]
fn interactive_launch_preserves_prompts_and_rejects_unrecoverable_or_unused_text() {
    let text = "  launch 🦀\nwith context  ";
    for args in [
        vec!["ion", text],
        vec!["ion", "chat", text],
        vec!["ion", "--model", "first/vendor/model", text],
        vec!["ion", "chat", text, "--model", "first/vendor/model"],
        vec!["ion", "--resume", text],
    ] {
        let cli = parse(&args, true).unwrap();
        assert!(
            matches!(cli.action, Action::Chat { input } if input.prompts().eq([text])),
            "{args:?}"
        );
    }
    for args in [vec!["ion", text, "auth"], vec!["ion", text, "chat"]] {
        assert!(parse(&args, true).is_err(), "{args:?}");
    }
    let exact = "🦀".repeat(crate::terminal_client::MAX_DRAFT / 4);
    assert!(parse(&["ion", "chat", &exact], true).is_ok());
    assert!(parse(&["ion", "chat", "first", &format!("{exact}x")], true).is_err());
}

#[test]
fn resume_is_an_explicit_terminal_chooser_not_a_session_operand() {
    for args in [
        vec!["ion", "-r"],
        vec!["ion", "chat", "--resume"],
        vec!["ion", "--resume", "run", "hello"],
        vec!["ion", "inspect", "--resume"],
        vec!["ion", "usage", "--resume"],
    ] {
        assert!(parse(&args, true).unwrap().resume_session, "{args:?}");
        assert!(parse(&args, false).is_err(), "{args:?}");
    }
    for args in [
        vec!["ion", "--resume", "--session", "saved"],
        vec!["ion", "--continue", "chat", "--resume"],
        vec!["ion", "--resume", "rpc"],
        vec!["ion", "--resume", "run", "--json", "hello"],
        vec!["ion", "auth", "--resume"],
    ] {
        assert!(parse(&args, true).is_err(), "{args:?}");
    }
    assert!(!parse(&["ion"], true).unwrap().resume_session);
    let cli = parse(&["ion", "--resume", "saved"], true).unwrap();
    assert!(cli.resume_session);
    assert!(matches!(cli.action, Action::Chat { input } if input.prompts().eq(["saved"])));
}

#[test]
fn scopes_reject_unused_options_in_both_positions_and_help_matches() {
    for (verb, flag, value) in [
        ("auth", "--model", "openai/gpt-5.4"),
        ("models", "--session", "saved"),
        ("use", "--cwd", "/tmp"),
        ("sessions", "--reasoning", "high"),
        ("inspect", "--code-mode", ""),
        ("usage", "--model", "openai/gpt-5.4"),
        ("rpc", "--image", "photo.png"),
        ("run", "--tui-mode", "fullscreen"),
        ("chat", "--json", ""),
        ("doctor", "--model", "openai/gpt-5.4"),
        ("doctor", "--session", "saved"),
        ("doctor", "--json", ""),
    ] {
        let mut before = vec!["ion", flag];
        if !value.is_empty() {
            before.push(value);
        }
        before.push(verb);
        let mut after = vec!["ion", verb, flag];
        if !value.is_empty() {
            after.push(value);
        }
        assert!(parse(&before, true).is_err(), "{before:?}");
        assert!(parse(&after, true).is_err(), "{after:?}");
    }
    for verb in [
        "auth",
        "models",
        "use",
        "login",
        "logout",
        "syntax-licenses",
    ] {
        let help = command()
            .try_get_matches_from(["ion", verb, "--help"])
            .err()
            .unwrap();
        let help = help.to_string();
        assert!(!help.contains("--model"), "{verb}: {help}");
        assert!(!help.contains("--cwd"), "{verb}: {help}");
    }
    let help = command()
        .try_get_matches_from(["ion", "run", "--help"])
        .err()
        .unwrap()
        .to_string();
    assert!(help.contains("--model"));
    assert!(help.contains("--json"));
    assert!(!help.contains("--tui-mode"));
    for args in [
        vec!["ion", "doctor", "--cwd", "/tmp"],
        vec!["ion", "--cwd", "/tmp", "doctor"],
    ] {
        let cli = parse(&args, false).unwrap();
        assert!(matches!(cli.action, Action::Doctor));
        assert_eq!(cli.cwd, Some(PathBuf::from("/tmp")));
    }
    let doctor_help = command()
        .try_get_matches_from(["ion", "doctor", "--help"])
        .err()
        .unwrap()
        .to_string();
    assert!(doctor_help.contains("--cwd") && !doctor_help.contains("--model"));
    let literal = parse(&["ion", "--", "doctor"], false).unwrap();
    assert!(matches!(literal.action, Action::Run { input } if input.prompts().eq(["doctor"])));
    let use_help = command()
        .try_get_matches_from(["ion", "use", "--help"])
        .err()
        .unwrap()
        .to_string();
    assert!(use_help.contains("Full HTTP endpoint"));
    assert!(use_help.contains("openai-responses"));
}

#[test]
fn normalize_cross_level_conflicts_and_append_repeatable_options() {
    for args in [
        vec!["ion", "--session", "saved", "chat", "--continue"],
        vec!["ion", "--read-only", "run", "hello", "--tools", "read"],
        vec!["ion", "--tools", "read", "chat", "--read-only"],
        vec!["ion", "--cwd", "/one", "sessions", "--cwd", "/two"],
        vec![
            "ion",
            "--model",
            "openai/one",
            "run",
            "hello",
            "--model",
            "openai/two",
        ],
        vec!["ion", "--json", "run", "hello", "--json"],
        vec!["ion", "--tools", "read,,write"],
    ] {
        assert!(parse(&args, true).is_err(), "{args:?}");
    }
    let cli = parse(
        &[
            "ion", "--image", "one.png", "run", "hello", "--image", "two.png",
        ],
        true,
    )
    .unwrap();
    assert_eq!(
        cli.image,
        vec![PathBuf::from("one.png"), PathBuf::from("two.png")]
    );
}

#[test]
fn metadata_positionals_and_nested_working_directory_have_separate_custody() {
    let cli = parse(
        &[
            "ion",
            "use",
            "custom",
            "vendor/model",
            "--endpoint",
            "http://localhost:1234",
            "--wire",
            "openai-responses",
        ],
        true,
    )
    .unwrap();
    assert!(
        matches!(cli.action, Action::Use { provider, model, .. } if provider == "custom" && model == "vendor/model")
    );
    assert!(cli.model.is_none());
    for args in [
        vec!["ion", "--cwd", "/one", "mcp", "list"],
        vec!["ion", "mcp", "--cwd", "/one", "list"],
        vec!["ion", "mcp", "list", "--cwd", "/one"],
        vec!["ion", "commands", "list", "--cwd", "/one"],
    ] {
        assert_eq!(
            parse(&args, true).unwrap().cwd,
            Some(PathBuf::from("/one")),
            "{args:?}"
        );
    }
    assert!(
        parse(
            &["ion", "--cwd", "/one", "mcp", "list", "--cwd", "/two"],
            true
        )
        .is_err()
    );
    let cli = parse(&["ion", "login", "openai"], true).unwrap();
    assert!(matches!(cli.action, Action::Login { provider } if provider == "openai"));
}

#[test]
fn mcp_server_arguments_keep_leading_and_overlapping_flags_literal() {
    for tail in [
        vec!["-c", "import sys", "--cwd", "server-dir", "--project"],
        vec!["--", "--cwd", "server-dir", "-c", "import sys", "--project"],
    ] {
        let mut args = vec![
            "ion",
            "mcp",
            "add",
            "--cwd",
            "/project",
            "probe",
            "--exposure",
            "hidden",
            "python3",
        ];
        args.extend(tail.iter().copied());
        let cli = parse(&args, true).unwrap();
        assert_eq!(cli.cwd, Some(PathBuf::from("/project")));
        let Action::Mcp {
            action:
                McpAction::Add {
                    command,
                    args,
                    project,
                    exposure,
                    ..
                },
        } = cli.action
        else {
            panic!("expected stdio server configuration");
        };
        assert_eq!(command, "python3");
        assert_eq!(args, tail.strip_prefix(&["--"]).unwrap_or(&tail));
        assert!(!project);
        assert!(matches!(exposure, ExposureArg::Hidden));
    }
}

#[test]
fn pipe_input_is_bounded_utf8_and_only_the_argument_gets_a_separator() {
    let input = "  /literal-template\n\n";
    assert_eq!(read_pipe(input.as_bytes()).unwrap(), input);
    assert_eq!(with_piped_input(String::new(), input.into()), input);
    assert_eq!(
        with_piped_input("argument".into(), String::new()),
        "argument"
    );
    assert_eq!(
        with_piped_input("argument".into(), input.into()),
        format!("{input}\nargument")
    );
    assert!(read_pipe([0xff].as_slice()).is_err());
    assert!(read_pipe(std::io::repeat(b'a').take(8 * 1024 * 1024 + 1)).is_err());
    assert_eq!(
        read_pipe(std::io::repeat(b'a').take(8 * 1024 * 1024))
            .unwrap()
            .len(),
        8 * 1024 * 1024
    );
}
