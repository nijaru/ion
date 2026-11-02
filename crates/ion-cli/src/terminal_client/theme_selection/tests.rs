mod native_io;
use super::*;
use crate::terminal_client::Editor;
use crate::{
    presentation_style::Role,
    terminal_client::{Action, painted_dock_rows},
};
use ion_terminal::{KeyCode, KeyEvent, Modifiers};
use ratatui::style::Color;
use std::{fs, path::PathBuf};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("ion-theme-picker-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(path.join("themes")).unwrap();
        Self(path)
    }
    fn ui(&self) -> Frontend {
        let mut ui = Frontend {
            editor: Editor {
                draft: "unsent 🦀".into(),
                cursor: 3,
                images: vec![
                    ion_ai::normalize_rgba(1, 1, vec![0, 255, 0, 255])
                        .unwrap()
                        .into(),
                ],
                ..Editor::default()
            },
            themes: crate::themes::Themes::load(&self.0, None).unwrap(),
            conversation: crate::terminal_client::conversation::fixtures::document(
                ion_core::TranscriptProjection {
                    items: vec![ion_core::TranscriptItem::User(
                        ion_core::TranscriptMessage {
                            source: Some(ion_core::TranscriptSource { entry: 1, part: 0 }),
                            turn: Some(1),
                            steering: false,
                            parts: vec![ion_core::TranscriptPart::Text("already published".into())],
                        },
                    )],
                },
            ),
            ..Frontend::default()
        };
        crate::terminal_client::conversation::fixtures::publish(&mut ui.conversation);
        ui
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn key(ui: &mut Frontend, code: KeyCode) -> Action {
    ui.key(KeyEvent::new(code, Modifiers::NONE), None, None)
}
async fn settle(ui: &mut Frontend) {
    loop {
        ui.sync_theme_preview();
        if !ui.theme_io.has_job() {
            break;
        }
        tokio::time::timeout(std::time::Duration::from_secs(5), ui.finish_theme_io())
            .await
            .unwrap()
            .unwrap();
    }
}
async fn open(ui: &mut Frontend) {
    ui.open_theme_picker().unwrap();
    settle(ui).await;
}
async fn frame(ui: &mut Frontend) -> Vec<ratatui::text::Line<'static>> {
    settle(ui).await;
    painted_dock_rows(ui, None, 100, 30).0
}

#[tokio::test]
async fn rendered_theme_preview_cancel_and_failed_save_preserve_input_and_publication() {
    let root = Root::new();
    fs::write(
        root.0.join("themes/custom.json"),
        r##"{"name":"custom","colors":{"code":"#123abc"}}"##,
    )
    .unwrap();
    let mut ui = root.ui();
    let original = ui.themes.theme().clone();
    ui.open_settings(ion_core::CompletionDelivery::Inbox);
    ui.pickers.insert("Theme");
    frame(&mut ui).await;
    assert!(matches!(
        key(&mut ui, KeyCode::Enter),
        Action::Pick(PickerValue::Themes)
    ));
    open(&mut ui).await;
    ui.pickers.insert("custom");
    assert!(
        matches!(key(&mut ui, KeyCode::Enter), Action::None),
        "unpainted choices cannot save"
    );
    let rows = frame(&mut ui).await;
    assert!(
        rows.iter()
            .flat_map(|row| &row.spans)
            .any(|span| span.content == "Code" && span.style.fg == Some(Color::Rgb(18, 58, 188)))
    );
    assert_eq!(ui.themes.name(), "system");
    assert!(!root.0.join("settings.json").exists());
    key(&mut ui, KeyCode::Esc);
    assert_eq!(ui.themes.theme(), &original);
    assert_eq!(ui.pickers.active().unwrap().query, "Theme");

    open(&mut ui).await;
    ui.pickers.insert("custom");
    frame(&mut ui).await;
    let Action::Pick(PickerValue::Theme(name)) = key(&mut ui, KeyCode::Enter) else {
        panic!("painted theme choice");
    };
    let lock = fs::File::open(&root.0).unwrap();
    lock.lock().unwrap();
    ui.choose_theme(&name).unwrap();
    settle(&mut ui).await;
    assert!(ui.status.contains("not saved"));
    assert_eq!(ui.themes.name(), "system");
    assert!(!root.0.join("settings.json").exists());
    frame(&mut ui).await;
    assert!(
        matches!(ui.pickers.selected_value(), Some(PickerValue::Theme(name)) if name == "custom")
    );
    drop(lock);
    let Action::Pick(PickerValue::Theme(name)) = key(&mut ui, KeyCode::Enter) else {
        panic!("save retry choice");
    };
    ui.choose_theme(&name).unwrap();
    settle(&mut ui).await;
    assert_eq!(
        crate::themes::Themes::load(&root.0, None).unwrap().name(),
        "custom"
    );
    assert_eq!(
        ui.themes.theme().style(Role::Code).fg,
        Some(Color::Rgb(18, 58, 188))
    );
    assert_eq!(ui.pickers.active().unwrap().query, "Theme");
    assert_eq!(
        (
            ui.editor.draft.as_str(),
            ui.editor.cursor,
            ui.editor.images.len()
        ),
        ("unsent 🦀", 3, 1)
    );
    assert!(ui.conversation.has_published_prefix());
    assert!(ui.pending_history_rows(80).is_empty());
}

#[tokio::test]
async fn replacing_a_theme_view_drops_its_preview_and_back_menu() {
    let root = Root::new();
    let mut ui = root.ui();
    ui.open_settings(ion_core::CompletionDelivery::Inbox);
    open(&mut ui).await;
    ui.pickers.insert("dark");
    frame(&mut ui).await;
    assert!(ui.theme_picker.is_some());
    ui.open_settings(ion_core::CompletionDelivery::Inbox);
    ui.sync_theme_preview();
    assert!(ui.theme_picker.is_none());
    assert_eq!(
        ui.themes.theme(),
        &crate::presentation_style::Theme::default()
    );
    key(&mut ui, KeyCode::Esc);
    assert!(
        ui.pickers.active().is_none(),
        "the old back-menu must not resurrect"
    );
}

#[tokio::test]
async fn automatic_theme_slots_are_unsaved_drafts_until_apply() {
    let root = Root::new();
    let mut ui = root.ui();
    open(&mut ui).await;
    ui.theme_action(ThemeAction::Automatic).unwrap();
    ui.theme_action(ThemeAction::Light).unwrap();
    ui.choose_theme("light").unwrap();
    settle(&mut ui).await;
    ui.theme_action(ThemeAction::Dark).unwrap();
    ui.pickers.insert("dark");
    frame(&mut ui).await;
    assert_eq!(
        ui.themes.theme(),
        &crate::presentation_style::Theme::fixed(true)
    );
    key(&mut ui, KeyCode::Esc); // Cancel only the dark slot.
    assert_eq!(ui.pair(), "light/system");
    assert!(!root.0.join("settings.json").exists());
    key(&mut ui, KeyCode::Esc); // Cancel the entire automatic draft.
    assert!(ui.theme_picker.is_none());
    assert_eq!(ui.themes.name(), "system");
    assert_eq!(
        ui.themes.theme(),
        &crate::presentation_style::Theme::default()
    );

    open(&mut ui).await;
    ui.theme_action(ThemeAction::Automatic).unwrap();
    ui.theme_action(ThemeAction::Light).unwrap();
    ui.choose_theme("light").unwrap();
    settle(&mut ui).await;
    ui.theme_action(ThemeAction::Dark).unwrap();
    ui.choose_theme("dark").unwrap();
    settle(&mut ui).await;
    assert_eq!(ui.themes.name(), "system");
    assert!(!root.0.join("settings.json").exists());
    ui.theme_action(ThemeAction::Apply).unwrap();
    settle(&mut ui).await;
    assert_eq!(
        crate::themes::Themes::load(&root.0, None).unwrap().name(),
        "light/dark"
    );
    assert_eq!(
        (
            ui.editor.draft.as_str(),
            ui.editor.cursor,
            ui.editor.images.len()
        ),
        ("unsent 🦀", 3, 1)
    );
}
