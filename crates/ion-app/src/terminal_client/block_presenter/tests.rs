use super::super::conversation::fixtures;
use super::*;
use crate::{
    display_text::{Component, Field, Origin, ParsedComponent},
    transcript_render::{ContentRoot, ReadingPoint, TextPoint},
};
use ion_core::{TranscriptMessage, TranscriptProjection};

fn item(entry: u64) -> TranscriptItem {
    TranscriptItem::User(TranscriptMessage {
        source: Some(TranscriptSource { entry, part: 0 }),
        turn: Some(entry),
        steering: false,
        parts: vec![TranscriptPart::Text(format!("ROOT{entry:02}"))],
    })
}

#[test]
fn reading_anchor_survives_stream_growth_and_reflow_then_returns_to_follow_end() {
    let mut document = fixtures::document(TranscriptProjection {
        items: (1..=5).map(item).collect(),
    });
    let mut presenter = BlockPresenter::default();
    let mut view = ConversationView::default();
    let keys = crate::keybindings::KeyBindings::default();
    let theme = Theme::default();
    let options = RenderOptions::new(
        OutputDetail::Compact,
        &keys,
        &theme,
        ThinkingVisibility::Hidden,
    );
    presenter.window(&document, &mut view, (20, 3), false, false, &options);
    view.scroll(-2);
    let before = presenter.window(&document, &mut view, (20, 3), false, false, &options);
    assert!(before.iter().any(|row| row.to_string().contains("ROOT03")));
    fixtures::append(&mut document, item(6));
    let after = presenter.window(&document, &mut view, (20, 3), false, false, &options);
    assert_eq!(before, after, "new facts must not pull a reader down");
    let narrow = presenter.window(&document, &mut view, (6, 3), false, false, &options);
    assert_eq!(
        view.anchor.as_ref().unwrap().key,
        Key::Fact(TranscriptSource { entry: 3, part: 0 })
    );
    assert!(narrow.iter().all(|row| row.width() <= 6));
    view.scroll(isize::MAX);
    let tail = presenter.window(&document, &mut view, (20, 3), false, false, &options);
    assert!(!view.is_reading());
    assert!(tail.iter().any(|row| row.to_string().contains("ROOT06")));
}

#[test]
fn resizing_a_hundred_line_message_keeps_line_ninety_in_view() {
    let text = (0..100)
        .map(|line| format!("MARK{line:03} body\n\n"))
        .collect::<String>();
    let document = fixtures::document(TranscriptProjection {
        items: vec![assistant(1, vec![TranscriptPart::Text(text)])],
    });
    let keys = crate::keybindings::KeyBindings::default();
    let theme = Theme::default();
    let options = RenderOptions::new(
        OutputDetail::Compact,
        &keys,
        &theme,
        ThinkingVisibility::Hidden,
    );
    let mut presenter = BlockPresenter::default();
    let mut view = ConversationView::default();
    presenter.window(&document, &mut view, (80, 5), false, false, &options);
    view.scroll(-14);
    let before = presenter.window(&document, &mut view, (80, 5), false, false, &options);
    assert_eq!(before[0].to_string(), "MARK090 body");
    let narrow = presenter.window(&document, &mut view, (8, 5), false, false, &options);
    assert!(
        narrow
            .iter()
            .map(ToString::to_string)
            .collect::<String>()
            .contains("MARK090")
    );
    assert_eq!(
        presenter.window(&document, &mut view, (80, 5), false, false, &options),
        before
    );
}

fn assistant(entry: u64, parts: Vec<TranscriptPart>) -> TranscriptItem {
    TranscriptItem::Assistant(TranscriptMessage {
        source: Some(TranscriptSource { entry, part: 0 }),
        turn: Some(1),
        steering: false,
        parts,
    })
}

// Enter reading through the actual scroll contract, targeting an original byte
// in a particular Core source/part, not a matching occurrence of fitted text.
fn read_at(
    presenter: &mut BlockPresenter,
    document: &Conversation,
    view: &mut ConversationView,
    target: (Key, ReadingPoint),
    width: usize,
    options: &RenderOptions<'_>,
) -> Vec<Line<'static>> {
    presenter.window(document, view, (width, 8), false, false, options);
    view.scroll(isize::MIN);
    presenter.window(document, view, (width, 8), false, false, options);
    let mut offset = 0;
    for block in BlockPresenter::blocks(document, false, options) {
        let layout = presenter.layout(&block, document.revision(), width, options);
        let gap = usize::from(block.gap && !layout.content.rows.is_empty());
        if block.key == target.0 {
            let row = layout
                .content
                .provenance
                .iter()
                .position(|origins| {
                    origins.iter().any(|(root, origin)| {
                        *root == target.1.root
                            && match (target.1.point, origin) {
                                (TextPoint::Raw(byte), Origin::Raw(range)) => range.contains(&byte),
                                (
                                    TextPoint::Component { component, byte },
                                    Origin::Component {
                                        component: actual,
                                        bytes,
                                    },
                                ) => component == *actual && bytes.contains(&byte),
                                (TextPoint::Node(byte), Origin::Node(node)) => byte == *node,
                                _ => false,
                            }
                    })
                })
                .expect("requested source point is visible");
            offset += gap + row;
            view.scroll(offset as isize);
            return presenter.window(document, view, (width, 8), false, false, options);
        }
        offset += layout.content.rows.len() + gap;
    }
    panic!("source exists");
}

#[test]
fn long_source_reading_survives_bidirectional_reflow_without_text_identity_or_drift() {
    let prose = (0..100)
        .map(|line| format!("MARK{line:03} alpha beta gamma delta epsilon zeta eta theta.\n\n"))
        .collect::<String>();
    let continuous = prose.replace("\n\n", " ");
    let markdown = (0..100).map(|line| format!("**MARK{line:03}** &amp; *emphasis* [label](https://example.org/{line}) and `literal_code`.\n\n")).collect::<String>();
    let code = format!(
        "```rust\n{}\n```",
        (0..100)
            .map(|line| format!(
                "MARK{line:03}(); // literal technical line with a_long_identifier\n"
            ))
            .collect::<String>()
    );
    let table = format!(
        "| Key | Value |\n| --- | --- |\n{}",
        (0..100)
            .map(|line| format!("| MARK{line:03} | **value** &amp; `technical_code` |\n"))
            .collect::<String>()
    );
    let keys = crate::keybindings::KeyBindings::default();
    let theme = Theme::default();
    let options = RenderOptions::new(
        OutputDetail::Compact,
        &keys,
        &theme,
        ThinkingVisibility::Hidden,
    );
    for source_text in [prose, continuous, markdown, code, table] {
        let source = TranscriptSource { entry: 11, part: 0 };
        let parts = vec![
            TranscriptPart::Thinking("hidden before target".into()),
            TranscriptPart::Text(source_text.clone()),
            TranscriptPart::Text(source_text.clone()),
        ];
        let mut document = fixtures::document(TranscriptProjection {
            items: vec![assistant(10, parts.clone()), assistant(11, parts)],
        });
        let mut presenter = BlockPresenter::default();
        let mut view = ConversationView::default();
        let byte = source_text.find("MARK090").unwrap();
        let before = read_at(
            &mut presenter,
            &document,
            &mut view,
            (
                Key::Fact(source),
                ReadingPoint {
                    root: ContentRoot::Part(1),
                    point: TextPoint::Raw(byte),
                },
            ),
            100,
            &options,
        );
        assert!(before[0].to_string().contains("MARK090"));
        let anchor = view.anchor.unwrap();
        assert_eq!(anchor.key, Key::Fact(source));
        let Some(ReadingPoint {
            root: ContentRoot::Part(1),
            point: TextPoint::Raw(retained),
        }) = anchor.position
        else {
            panic!("reading the selected source/part")
        };
        assert!(retained <= byte && byte - retained < 100);
        for (iteration, width) in [23, 140, 5, 100, 37, 100].into_iter().enumerate() {
            fixtures::append(&mut document, item(1000 + iteration as u64));
            let rows = presenter.window(&document, &mut view, (width, 8), true, false, &options);
            let joined = rows
                .iter()
                .map(|row| {
                    let text = row.to_string();
                    if source_text.starts_with("```") {
                        text.strip_prefix("  ").unwrap_or(&text).to_owned()
                    } else {
                        text
                    }
                })
                .collect::<String>();
            if source_text.contains('\n') {
                assert!(joined.contains("MARK090"), "width={width}: {joined}");
            }
            assert!(rows.iter().all(|row| row.width() <= width));
            assert_eq!(
                view.anchor.as_ref().unwrap().position,
                anchor.position,
                "reflow must not round the source point to a new row start"
            );
            if width == 100 {
                assert_eq!(rows, before);
            }
        }
        if source_text.starts_with('|') {
            // Every value cell has identical text. A stacked second-column
            // reading point must unfold to its actual row, not the first value.
            let byte = source_text.match_indices("**value**").nth(90).unwrap().0 + 2;
            let point = ReadingPoint {
                root: ContentRoot::Part(1),
                point: TextPoint::Raw(byte),
            };
            let stacked = read_at(
                &mut presenter,
                &document,
                &mut view,
                (Key::Fact(source), point),
                8,
                &options,
            );
            assert!(stacked[0].to_string().contains("value"));
            let aligned = presenter.window(&document, &mut view, (100, 8), false, false, &options);
            assert!(aligned[0].to_string().contains("MARK090"));
            assert_eq!(view.anchor.as_ref().unwrap().position, Some(point));
            assert_eq!(
                presenter.window(&document, &mut view, (8, 8), false, false, &options),
                stacked
            );
        }
    }
}

#[test]
fn disclosure_and_theme_changes_reuse_the_reading_parts_original_bytes() {
    let keys = crate::keybindings::KeyBindings::default();
    let theme = Theme::default();
    let compact = RenderOptions::new(
        OutputDetail::Compact,
        &keys,
        &theme,
        ThinkingVisibility::Hidden,
    );
    let text = (0..100)
        .map(|line| format!("MARK{line:03} alpha beta gamma delta\n\n"))
        .collect::<String>();
    let source = TranscriptSource { entry: 1, part: 0 };
    let document = fixtures::document(TranscriptProjection {
        items: vec![assistant(
            1,
            vec![
                TranscriptPart::Thinking(text.clone()),
                TranscriptPart::Text(text.clone()),
            ],
        )],
    });
    let mut presenter = BlockPresenter::default();
    let mut view = ConversationView::default();
    let byte = text.find("MARK090").unwrap();
    let before = read_at(
        &mut presenter,
        &document,
        &mut view,
        (
            Key::Fact(source),
            ReadingPoint {
                root: ContentRoot::Part(1),
                point: TextPoint::Raw(byte),
            },
        ),
        80,
        &compact,
    );
    let mut changed = theme.clone();
    changed.set_color(
        crate::presentation_style::Role::Text,
        ratatui::style::Color::Red,
    );
    let expanded = RenderOptions::new(
        OutputDetail::Expanded,
        &keys,
        &changed,
        ThinkingVisibility::Visible,
    );
    let after = presenter.window(&document, &mut view, (25, 8), false, false, &expanded);
    assert!(after[0].to_string().contains("MARK090"));
    assert_eq!(
        view.anchor.as_ref().unwrap().position,
        Some(ReadingPoint {
            root: ContentRoot::Part(1),
            point: TextPoint::Raw(byte)
        })
    );
    assert_eq!(
        presenter.window(&document, &mut view, (80, 8), false, false, &compact),
        before
    );
    let point = ReadingPoint {
        root: ContentRoot::Part(0),
        point: TextPoint::Raw(byte),
    };
    let thinking = read_at(
        &mut presenter,
        &document,
        &mut view,
        (Key::Fact(source), point),
        80,
        &expanded,
    );
    presenter.window(&document, &mut view, (25, 8), false, false, &compact);
    assert_eq!(view.anchor.as_ref().unwrap().position, Some(point));
    assert_eq!(
        presenter.window(&document, &mut view, (80, 8), false, false, &expanded),
        thinking
    );
}

#[test]
fn activity_reading_uses_occurrences_and_children_through_fold_and_reflow() {
    use ion_core::{
        ActivityGroup, ActivityResult, ActivityState, ToolActivity, ToolActivityKind,
        ToolOccurrence, TranscriptActivity, TranscriptActivityIdentity,
    };
    let activity = |ordinal, subject: &str| TranscriptActivity {
        identity: TranscriptActivityIdentity::Tool(ToolOccurrence {
            assistant_entry: 10,
            ordinal,
        }),
        call_id: "reused-provider-id".into(),
        name: "read".into(),
        activity: ToolActivity {
            kind: ToolActivityKind::Read,
            subject: Some(subject.into()),
        },
        arguments: serde_json::Value::Null,
        state: ActivityState::Completed,
        result: Some(ActivityResult {
            projection: Some(ion_core::ToolResultProjection::Observed),
            value: serde_json::json!({"content":"same observed result"}),
            image_mime_types: vec![],
            is_error: false,
        }),
        children: vec![],
    };
    let first = activity(0, "earlier");
    let second = activity(1, "selected");
    let mut parent = activity(2, "composition");
    parent.activity.kind = ToolActivityKind::External;
    let mut child = activity(0, "nested-selected");
    child.identity = TranscriptActivityIdentity::Child {
        parent: ToolOccurrence {
            assistant_entry: 10,
            ordinal: 2,
        },
        child: 0,
    };
    parent.children.push(child.clone());
    let source = TranscriptSource { entry: 10, part: 0 };
    let mut document = fixtures::document(TranscriptProjection {
        items: vec![TranscriptItem::ActivityGroup(ActivityGroup {
            source,
            turn: 1,
            open: false,
            activities: vec![first, second.clone(), parent],
        })],
    });
    let keys = crate::keybindings::KeyBindings::default();
    let theme = Theme::default();
    let expanded = RenderOptions::new(
        OutputDetail::Expanded,
        &keys,
        &theme,
        ThinkingVisibility::Hidden,
    );
    let compact = RenderOptions::new(
        OutputDetail::Compact,
        &keys,
        &theme,
        ThinkingVisibility::Hidden,
    );
    let mut presenter = BlockPresenter::default();
    let mut view = ConversationView::default();
    for (activity, marker) in [(second, "selected"), (child, "nested-selected")] {
        let point = ReadingPoint {
            root: ContentRoot::Activity(activity.identity),
            point: TextPoint::Component {
                component: Component::Field(Field::Summary),
                byte: 0,
            },
        };
        let before = read_at(
            &mut presenter,
            &document,
            &mut view,
            (Key::Fact(source), point),
            100,
            &expanded,
        );
        assert!(before[0].to_string().contains(marker));
        fixtures::append(
            &mut document,
            item(100 + usize::from(marker == "selected") as u64),
        );
        for (width, options) in [(30, &compact), (120, &expanded), (100, &expanded)] {
            let rows = presenter.window(&document, &mut view, (width, 8), false, false, options);
            assert!(rows[0].to_string().contains(marker), "{}", rows[0]);
            assert_eq!(view.anchor.as_ref().unwrap().position, Some(point));
        }
        // A measured layout must be disposable even between two redraws.
        view.scroll(1);
        let moved = presenter.window(&document, &mut view, (100, 8), false, false, &expanded);
        presenter.layouts.clear();
        assert_eq!(
            presenter.window(&document, &mut view, (100, 8), false, false, &expanded),
            moved
        );
    }
}

#[tokio::test]
async fn growing_uncommitted_text_keeps_its_original_byte_location() {
    use ion_core::AgentEvent;
    let mut fixture = crate::terminal_client::tests::Fixture::new();
    let operation = fixture
        .control
        .compact(
            None,
            crate::terminal_client::EditorDraft::at_end(String::new()),
        )
        .unwrap();
    let mut document = fixtures::document(TranscriptProjection::default());
    assert!(!document.observe(
        operation,
        &AgentEvent::TurnAccepted {
            entry: 1,
            turn: 1,
            input: ion_ai::Message::user_input("request".into(), [])
        }
    ));
    let text = (0..100)
        .map(|line| format!("MARK{line:03} **alpha** beta gamma\n\n"))
        .collect::<String>();
    assert!(!document.observe(operation, &AgentEvent::TextDelta(text.clone())));
    let keys = crate::keybindings::KeyBindings::default();
    let theme = Theme::default();
    let options = RenderOptions::new(
        OutputDetail::Compact,
        &keys,
        &theme,
        ThinkingVisibility::Hidden,
    );
    let mut presenter = BlockPresenter::default();
    let mut view = ConversationView::default();
    let point = ReadingPoint {
        root: ContentRoot::Part(0),
        point: TextPoint::Raw(text.find("MARK090").unwrap()),
    };
    let before = read_at(
        &mut presenter,
        &document,
        &mut view,
        (
            Key::Provisional(Some(TranscriptSource { entry: 1, part: 0 })),
            point,
        ),
        80,
        &options,
    );
    assert!(before[0].to_string().contains("MARK090"));
    for width in [22, 120, 80] {
        assert!(!document.observe(
            operation,
            &AgentEvent::TextDelta("more streamed content\n\n".into())
        ));
        let rows = presenter.window(&document, &mut view, (width, 8), false, false, &options);
        assert!(rows[0].to_string().contains("MARK090"));
        assert_eq!(view.anchor.as_ref().unwrap().position, Some(point));
    }
    let event = AgentEvent::AssistantCommitted {
        entry: 2,
        turn: 1,
        content: vec![ion_ai::Content::Text(format!(
            "{text}{}",
            "more streamed content\n\n".repeat(3)
        ))],
        tool_activities: vec![],
        termination: ion_ai::ResponseTermination::Completed,
    };
    view.commit_response(document.live().unwrap(), &event);
    assert!(!document.observe(operation, &event));
    document.clear_live();
    presenter.layouts.clear();
    let committed = presenter.window(&document, &mut view, (22, 8), true, false, &options);
    assert!(committed[0].to_string().contains("MARK090"));
    assert_eq!(
        view.anchor.unwrap().key,
        Key::Fact(TranscriptSource { entry: 2, part: 0 })
    );
    assert_eq!(view.anchor.unwrap().position, Some(point));
    fixture.control.shutdown().await;
}

#[test]
fn normalized_html_code_and_link_destinations_reflow_within_the_parser_component() {
    use pulldown_cmark::{Event, Options, Parser, Tag};
    for source in [
        "> before <span\n> attribute=\"MARK000 MARK001 MARK002 MARK003\"> after",
        "a [x](https://example.com/MARK000/MARK001/MARK002/MARK003) z",
        "> `MARK000 MARK001\n> MARK002 MARK003` after",
    ] {
        let (component, text) = Parser::new_ext(source, Options::ENABLE_TABLES)
            .into_offset_iter()
            .find_map(|(event, range)| {
                let (kind, text) = match event {
                    Event::InlineHtml(text) | Event::Html(text) if text.contains("MARK002") => {
                        (ParsedComponent::Html, text.into_string())
                    }
                    Event::Code(text) if text.contains("MARK002") => {
                        (ParsedComponent::Code, text.into_string())
                    }
                    Event::Start(Tag::Link { dest_url, .. }) => {
                        (ParsedComponent::LinkDestination, dest_url.into_string())
                    }
                    _ => return None,
                };
                Some((
                    Component::Parsed {
                        node: range.start,
                        kind,
                    },
                    text,
                ))
            })
            .expect("fixture has a parser-owned normalized component");
        let byte = text.find("MARK002").unwrap();
        let document = fixtures::document(TranscriptProjection {
            items: vec![assistant(1, vec![TranscriptPart::Text(source.into())])],
        });
        let keys = crate::keybindings::KeyBindings::default();
        let theme = Theme::default();
        let mut changed = theme.clone();
        changed.set_color(
            crate::presentation_style::Role::Text,
            ratatui::style::Color::Red,
        );
        let options = RenderOptions::new(
            OutputDetail::Expanded,
            &keys,
            &theme,
            ThinkingVisibility::Hidden,
        );
        let themed = RenderOptions::new(
            OutputDetail::Expanded,
            &keys,
            &changed,
            ThinkingVisibility::Hidden,
        );
        let mut presenter = BlockPresenter::default();
        let mut view = ConversationView::default();
        let before = read_at(
            &mut presenter,
            &document,
            &mut view,
            (
                Key::Fact(TranscriptSource { entry: 1, part: 0 }),
                ReadingPoint {
                    root: ContentRoot::Part(0),
                    point: TextPoint::Component { component, byte },
                },
            ),
            15,
            &options,
        );
        let anchor = view.anchor.unwrap();
        assert!(
            matches!(anchor.position, Some(ReadingPoint { point: TextPoint::Component { component: actual, byte: retained }, .. }) if actual == component && retained <= byte && byte - retained < 15)
        );
        for (width, options) in [(18, &themed), (16, &options), (15, &options)] {
            presenter.layouts.clear();
            let rows = presenter.window(&document, &mut view, (width, 8), false, false, options);
            assert_eq!(view.anchor.unwrap().position, anchor.position);
            let joined = rows
                .iter()
                .map(|row| {
                    let text = row.to_string();
                    text.strip_prefix("│ ").unwrap_or(&text).to_owned()
                })
                .collect::<String>();
            assert!(joined.contains("MARK002"), "{source} at {width}: {joined}");
            if width == 15 {
                assert_eq!(rows, before);
            }
        }
    }
}

#[test]
fn deep_tool_child_shell_and_diff_fields_survive_remeasurement_and_hidden_disclosure() {
    use ion_core::{
        ActivityGroup, ActivityResult, ActivityState, ToolActivity, ToolActivityKind,
        ToolOccurrence, TranscriptActivity, TranscriptActivityIdentity, UserShellActivity,
        UserShellOutcome,
    };
    use serde_json::json;
    let text = (0..100)
        .map(|line| format!("MARK{line:03} literal technical content\r\n"))
        .collect::<String>();
    let diff = format!("--- old\n+++ new\n{text}");
    let activity = |ordinal, kind, state, value| TranscriptActivity {
        identity: TranscriptActivityIdentity::Tool(ToolOccurrence {
            assistant_entry: 10,
            ordinal,
        }),
        call_id: "same-provider-id".into(),
        name: "same-display-name".into(),
        activity: ToolActivity {
            kind,
            subject: Some("same subject".into()),
        },
        arguments: serde_json::Value::Null,
        state,
        result: Some(ActivityResult {
            projection: Some(ion_core::ToolResultProjection::Observed),
            value,
            image_mime_types: vec![],
            is_error: state == ActivityState::Failed,
        }),
        children: vec![],
    };
    let read = activity(
        0,
        ToolActivityKind::Read,
        ActivityState::Completed,
        json!({"content":text}),
    );
    let command = activity(
        1,
        ToolActivityKind::Command,
        ActivityState::Completed,
        json!({"stdout":text,"stderr":text}),
    );
    let edit = activity(
        2,
        ToolActivityKind::Edit,
        ActivityState::Completed,
        json!({"diff":{"text":diff,"truncated":false}}),
    );
    let failure = activity(
        3,
        ToolActivityKind::Write,
        ActivityState::Failed,
        json!({"error":text}),
    );
    let mut parent = activity(
        4,
        ToolActivityKind::External,
        ActivityState::Completed,
        json!({}),
    );
    let mut child = command.clone();
    child.identity = TranscriptActivityIdentity::Child {
        parent: ToolOccurrence {
            assistant_entry: 10,
            ordinal: 4,
        },
        child: 0,
    };
    parent.children.push(child.clone());
    let group_source = TranscriptSource { entry: 10, part: 0 };
    let shell_source = TranscriptSource { entry: 20, part: 0 };
    let cases = [
        (
            group_source,
            ContentRoot::Activity(read.identity),
            Field::Content,
            text.find("MARK090").unwrap(),
        ),
        (
            group_source,
            ContentRoot::Activity(command.identity),
            Field::Stdout,
            text.find("MARK090").unwrap(),
        ),
        (
            group_source,
            ContentRoot::Activity(command.identity),
            Field::Stderr,
            text.find("MARK090").unwrap(),
        ),
        (
            group_source,
            ContentRoot::Activity(edit.identity),
            Field::Diff,
            diff.find("MARK090").unwrap(),
        ),
        (
            group_source,
            ContentRoot::Activity(failure.identity),
            Field::Error,
            text.find("MARK090").unwrap(),
        ),
        (
            group_source,
            ContentRoot::Activity(child.identity),
            Field::Stdout,
            text.find("MARK090").unwrap(),
        ),
        (
            shell_source,
            ContentRoot::Block,
            Field::Command,
            text.find("MARK090").unwrap(),
        ),
        (
            shell_source,
            ContentRoot::Block,
            Field::Stdout,
            text.find("MARK090").unwrap(),
        ),
        (
            shell_source,
            ContentRoot::Block,
            Field::Stderr,
            text.find("MARK090").unwrap(),
        ),
    ];
    let mut document = fixtures::document(TranscriptProjection {
        items: vec![
            TranscriptItem::ActivityGroup(ActivityGroup {
                source: group_source,
                turn: 1,
                open: false,
                activities: vec![read, command, edit, failure, parent],
            }),
            TranscriptItem::UserShell(UserShellActivity {
                source: shell_source,
                command: text.clone(),
                outcome: Some(UserShellOutcome::Observed {
                    output: json!({"stdout":text,"stderr":text,"exit_code":0}),
                    is_error: false,
                }),
                exclude_from_context: false,
            }),
        ],
    });
    let keys = crate::keybindings::KeyBindings::default();
    let theme = Theme::default();
    let mut changed = theme.clone();
    changed.set_color(
        crate::presentation_style::Role::Secondary,
        ratatui::style::Color::Red,
    );
    let expanded = RenderOptions::new(
        OutputDetail::Expanded,
        &keys,
        &theme,
        ThinkingVisibility::Hidden,
    );
    let compact = RenderOptions::new(
        OutputDetail::Compact,
        &keys,
        &changed,
        ThinkingVisibility::Hidden,
    );
    let mut presenter = BlockPresenter::default();
    let mut view = ConversationView::default();
    for (index, (source, root, field, byte)) in cases.into_iter().enumerate() {
        let point = ReadingPoint {
            root,
            point: TextPoint::Component {
                component: Component::Field(field),
                byte,
            },
        };
        let before = read_at(
            &mut presenter,
            &document,
            &mut view,
            (Key::Fact(source), point),
            60,
            &expanded,
        );
        assert!(
            before[0].to_string().contains("MARK090"),
            "{field:?}: {}",
            before[0]
        );
        assert_eq!(view.anchor.unwrap().position, Some(point));
        fixtures::append(&mut document, item(1000 + index as u64));
        presenter.layouts.clear();
        presenter.window(&document, &mut view, (20, 8), false, false, &compact);
        assert_eq!(
            view.anchor.unwrap().position,
            Some(point),
            "hidden {field:?} must not discard its byte"
        );
        for width in [100, 20, 60] {
            presenter.layouts.clear();
            let rows = presenter.window(&document, &mut view, (width, 8), false, false, &expanded);
            assert!(
                rows[0].to_string().contains("MARK090"),
                "{field:?} at {width}: {}",
                rows[0]
            );
            assert!(rows.iter().all(|row| row.width() <= width));
            assert_eq!(view.anchor.unwrap().position, Some(point));
            if width == 60 {
                assert_eq!(rows, before);
            }
        }
        view.scroll(1);
        let moved = presenter.window(&document, &mut view, (20, 8), false, false, &expanded);
        presenter.layouts.clear();
        assert_eq!(
            presenter.window(&document, &mut view, (20, 8), false, false, &expanded),
            moved
        );
    }
}

#[test]
fn idle_fullscreen_keeps_the_continuous_tail_and_repeated_notice_reading_points() {
    use ion_core::{
        ActivityGroup, ActivityState, ToolActivity, ToolActivityKind, ToolOccurrence,
        TranscriptActivity, TranscriptActivityIdentity,
    };
    let interrupted = TranscriptItem::ActivityGroup(ActivityGroup {
        source: TranscriptSource { entry: 2, part: 0 },
        turn: 1,
        open: false,
        activities: vec![TranscriptActivity {
            identity: TranscriptActivityIdentity::Tool(ToolOccurrence {
                assistant_entry: 2,
                ordinal: 0,
            }),
            call_id: "old-call".into(),
            name: "command".into(),
            activity: ToolActivity {
                kind: ToolActivityKind::Command,
                subject: Some("interrupted work".into()),
            },
            arguments: serde_json::Value::Null,
            state: ActivityState::Unknown,
            result: None,
            children: vec![],
        }],
    });
    let mut document = fixtures::document(TranscriptProjection {
        items: vec![
            assistant(1, vec![TranscriptPart::Text("history".into())]),
            interrupted,
        ],
    });
    let response = "HELP0\nHELP1\nHELP2\nHELP3\nHELP4\nunsafe\u{1b}[2J";
    document.note(response.into(), false);
    document.note(response.into(), false);
    let keys = crate::keybindings::KeyBindings::default();
    let theme = Theme::default();
    let options = RenderOptions::new(
        OutputDetail::Compact,
        &keys,
        &theme,
        ThinkingVisibility::Hidden,
    );
    let mut presenter = BlockPresenter::default();
    let mut view = ConversationView::default();
    let complete = presenter.window(&document, &mut view, (40, 64), false, false, &options);
    assert!(complete.len() > 16);
    let tail = presenter.window(&document, &mut view, (40, 16), false, false, &options);
    assert_eq!(tail, complete[complete.len() - 16..]);
    assert_eq!(
        tail.iter().filter(|row| row.to_string() == "HELP2").count(),
        2
    );
    assert!(
        !tail
            .iter()
            .any(|row| row.to_string().contains("earlier conversation"))
    );
    assert!(tail.iter().all(|row| !row.to_string().contains('\u{1b}')));

    let point = ReadingPoint {
        root: ContentRoot::Block,
        point: TextPoint::Raw(response.find("HELP2").unwrap()),
    };
    let before = read_at(
        &mut presenter,
        &document,
        &mut view,
        (Key::Local(1), point),
        40,
        &options,
    );
    assert_eq!(before[0].to_string(), "HELP2");
    for width in [3, 80, 40] {
        presenter.layouts.clear();
        let rows = presenter.window(&document, &mut view, (width, 8), false, false, &options);
        assert_eq!(view.anchor.unwrap().key, Key::Local(1));
        assert_eq!(view.anchor.unwrap().position, Some(point));
        assert!(
            rows.iter()
                .all(|row| row.width() <= width && !row.to_string().contains('\u{1b}'))
        );
        if width == 40 {
            assert_eq!(rows, before);
        }
    }
    document.note(response.into(), false);
    presenter.layouts.clear();
    let grown = presenter.window(&document, &mut view, (40, 8), false, false, &options);
    assert_eq!(&grown[..4], &before[..4]);
    assert_eq!(view.anchor.unwrap().key, Key::Local(1));
    assert_eq!(view.anchor.unwrap().position, Some(point));
}

#[test]
fn readiness_retains_offsets_in_its_semantic_message_after_remeasurement() {
    let notices = (1..=100)
        .map(|index| ion_core::CompletionNotice {
            source_session: uuid::Uuid::from_u128(index),
            run: uuid::Uuid::nil(),
            child_session: uuid::Uuid::nil(),
        })
        .collect::<Vec<_>>();
    let marker = format!("Source {}", notices[90].source_session);
    let text = crate::transcript_render::readiness_text(1, &notices);
    let byte = text.find(&marker).unwrap();
    let source = TranscriptSource { entry: 1, part: 0 };
    let document = fixtures::document(TranscriptProjection {
        items: vec![TranscriptItem::Readiness {
            source,
            turn: 1,
            notices,
        }],
    });
    let keys = crate::keybindings::KeyBindings::default();
    let theme = Theme::default();
    let mut changed = theme.clone();
    changed.set_color(
        crate::presentation_style::Role::Secondary,
        ratatui::style::Color::Red,
    );
    let options = RenderOptions::new(
        OutputDetail::Compact,
        &keys,
        &theme,
        ThinkingVisibility::Hidden,
    );
    let themed = RenderOptions::new(
        OutputDetail::Compact,
        &keys,
        &changed,
        ThinkingVisibility::Hidden,
    );
    let mut presenter = BlockPresenter::default();
    let mut view = ConversationView::default();
    let point = ReadingPoint {
        root: ContentRoot::Block,
        point: TextPoint::Component {
            component: Component::Readiness,
            byte,
        },
    };
    let before = read_at(
        &mut presenter,
        &document,
        &mut view,
        (Key::Fact(source), point),
        160,
        &options,
    );
    assert!(before[0].to_string().contains(&marker));
    assert_eq!(view.anchor.unwrap().position, Some(point));
    for (width, options) in [(40, &themed), (180, &options), (160, &options)] {
        presenter.layouts.clear();
        let rows = presenter.window(&document, &mut view, (width, 8), false, false, options);
        assert!(
            rows.iter()
                .map(ToString::to_string)
                .collect::<String>()
                .contains(&marker)
        );
        assert!(rows.iter().all(|row| row.width() <= width));
        assert_eq!(view.anchor.unwrap().position, Some(point));
        if width == 160 {
            assert_eq!(rows, before);
        }
    }
}
