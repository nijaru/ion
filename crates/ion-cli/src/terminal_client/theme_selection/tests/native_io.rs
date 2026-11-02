use super::*;
use std::sync::mpsc;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap()
}

struct PoolGate {
    release: mpsc::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}
impl PoolGate {
    async fn hold() -> Self {
        let (release, receive) = mpsc::channel();
        let (ready, started) = tokio::sync::oneshot::channel();
        let task = tokio::task::spawn_blocking(move || {
            ready.send(()).unwrap();
            let _ = receive.recv();
        });
        started.await.unwrap();
        Self { release, task }
    }
    async fn release(self) {
        self.release.send(()).unwrap();
        self.task.await.unwrap();
    }
}

fn custom(root: &Root, color: &str) {
    fs::write(
        root.0.join("themes/custom.json"),
        format!(r##"{{"name":"custom","colors":{{"code":"{color}"}}}}"##),
    )
    .unwrap();
}

#[test]
fn catalogue_and_preview_wait_for_native_capacity_without_consuming_editor_input() {
    let root = Root::new();
    custom(&root, "#123abc");
    runtime().block_on(async {
        let mut ui = root.ui();
        let original = ui.themes.theme().clone();
        let gate = PoolGate::hold().await;
        ui.open_theme_picker().unwrap();
        ui.pickers.insert("custom");
        ui.sync_theme_preview();
        assert_eq!(
            ui.themes.theme(),
            &original,
            "theme files must not be read inline while native capacity is occupied"
        );
        assert_eq!(
            (
                ui.editor.draft.as_str(),
                ui.editor.cursor,
                ui.editor.images.len()
            ),
            ("unsent 🦀", 3, 1)
        );
        assert!(ui.pending_history_rows(80).is_empty());
        tokio::task::yield_now().await;
        gate.release().await;
        settle(&mut ui).await;
        assert_eq!(
            ui.pickers.active().unwrap().query,
            "custom",
            "typing during catalogue discovery must survive publication"
        );
        assert_eq!(
            ui.themes.theme().style(Role::Code).fg,
            Some(Color::Rgb(18, 58, 188))
        );
        ui.shutdown_theme_io().await.unwrap();
        assert!(!root.0.join("settings.json").exists());
    });
}

#[tokio::test]
async fn catalogue_without_typed_filter_keeps_the_selected_saved_theme() {
    let root = Root::new();
    fs::write(root.0.join("settings.json"), br#"{"theme":"dark"}"#).unwrap();
    let mut ui = root.ui();
    open(&mut ui).await;
    assert!(
        matches!(ui.pickers.selected_value(), Some(PickerValue::Theme(name)) if name == "dark")
    );
    assert_eq!(
        ui.themes.theme(),
        &crate::presentation_style::Theme::fixed(true)
    );
    ui.shutdown_theme_io().await.unwrap();
}

#[test]
fn superseded_preview_error_cannot_replace_the_latest_choice_or_status() {
    let root = Root::new();
    custom(&root, "#123abc");
    runtime().block_on(async {
        let mut ui = root.ui();
        open(&mut ui).await;
        let original = ui.themes.theme().clone();
        let gate = PoolGate::hold().await;
        ui.pickers.insert("custom");
        ui.sync_theme_preview();
        for _ in 0..6 {
            key(&mut ui, KeyCode::Backspace);
        }
        ui.pickers.insert("dark");
        ui.sync_theme_preview();
        fs::remove_file(root.0.join("themes/custom.json")).unwrap();
        ui.status = "current view".into();
        gate.release().await;
        ui.finish_theme_io().await.unwrap(); // stale custom read, followed by latest preview
        assert_eq!(ui.status, "current view");
        assert_eq!(ui.themes.theme(), &original);
        settle(&mut ui).await;
        assert_eq!(
            ui.themes.theme(),
            &crate::presentation_style::Theme::fixed(true)
        );
        key(&mut ui, KeyCode::Esc);
        assert_eq!(ui.themes.theme(), &original);
        ui.shutdown_theme_io().await.unwrap();
    });
}

#[test]
fn queued_confirmation_survives_escape_and_a_cancelled_shutdown_wait() {
    let root = Root::new();
    custom(&root, "#123abc");
    runtime().block_on(async {
        let mut ui = root.ui();
        ui.open_settings(ion_core::CompletionDelivery::Inbox);
        open(&mut ui).await;
        let gate = PoolGate::hold().await;
        ui.pickers.insert("custom");
        ui.sync_theme_preview(); // retained read occupies the one native-job slot
        ui.choose_theme("custom").unwrap(); // confirmation waits behind it
        key(&mut ui, KeyCode::Esc);
        assert!(ui.theme_picker.is_none());
        assert!(!root.0.join("settings.json").exists());
        let mut closing = Box::pin(ui.shutdown_theme_io());
        assert!(futures_util::poll!(closing.as_mut()).is_pending());
        drop(closing);
        // The actual save must patch current settings, not its admission snapshot.
        fs::write(
            root.0.join("settings.json"),
            br#"{"search":{"native":true},"theme":"system"}"#,
        )
        .unwrap();
        gate.release().await;
        ui.shutdown_theme_io().await.unwrap();
        let settings: serde_json::Value =
            serde_json::from_slice(&fs::read(root.0.join("settings.json")).unwrap()).unwrap();
        assert_eq!(settings["theme"], "custom");
        assert_eq!(settings["search"]["native"], true);
        assert_eq!(ui.themes.name(), "custom");
        assert!(ui.status.starts_with("Theme saved: custom"));
        assert!(
            ui.pickers.active().is_none(),
            "cleanup must not resurrect a dismissed menu"
        );
        assert_eq!(
            (
                ui.editor.draft.as_str(),
                ui.editor.cursor,
                ui.editor.images.len()
            ),
            ("unsent 🦀", 3, 1)
        );
        assert!(ui.pending_history_rows(80).is_empty());
    });
}

#[test]
fn native_file_poll_keeps_last_valid_palette_and_resolves_current_appearance() {
    let root = Root::new();
    custom(&root, "#123abc");
    fs::write(root.0.join("settings.json"), br#"{"theme":"custom"}"#).unwrap();
    runtime().block_on(async {
        let mut ui = root.ui();
        let original = ui.themes.theme().clone();
        fs::write(
            root.0.join("themes/custom.json"),
            br#"{"name":"custom","colors":{"unknown":"PRIVATE_INVALID_COLOR"}}"#,
        )
        .unwrap();
        ui.theme_io.poll(&ui.themes);
        ui.finish_theme_io().await.unwrap();
        assert!(ui.status.starts_with("Theme unchanged:"));
        assert!(!ui.status.contains("PRIVATE_INVALID_COLOR"));
        assert_eq!(ui.themes.theme(), &original);
        ui.status = "a newer notice".into();
        ui.theme_io.poll(&ui.themes);
        ui.finish_theme_io().await.unwrap();
        assert_eq!(ui.status, "a newer notice");
        custom(&root, "#00ff00");
        let gate = PoolGate::hold().await;
        ui.theme_io.poll(&ui.themes);
        let colors = ion_terminal::TerminalColors {
            background: Some([255; 3]),
            foreground: Some([0; 3]),
            ..Default::default()
        };
        ui.themes.observe(colors);
        gate.release().await;
        ui.finish_theme_io().await.unwrap();
        let mut expected = crate::themes::Themes::load(&root.0, None).unwrap();
        expected.observe(colors);
        assert_eq!(ui.themes.theme(), expected.theme());
        assert!(ui.status.contains("reloaded"));
        ui.shutdown_theme_io().await.unwrap();
    });
}
