use super::*;
use std::{fs, sync::mpsc};

#[tokio::test]
async fn shutdown_reports_an_unobserved_failed_confirmation() {
    let root = std::env::temp_dir().join(format!("ion-theme-failed-save-{}", uuid::Uuid::now_v7()));
    fs::create_dir_all(&root).unwrap();
    let mut ui = Frontend {
        themes: Themes::load(&root, None).unwrap(),
        ..Frontend::default()
    };
    ui.theme_io
        .request(Request::Save("missing".into()), &ui.themes)
        .unwrap();
    let error = ui.shutdown_theme_io().await.unwrap_err();
    assert!(format!("{error:#}").contains("theme was not saved"));
    assert!(!root.join("settings.json").exists());
    assert_eq!(ui.themes.name(), "system");
    ui.shutdown_theme_io().await.unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn a_lost_save_result_reports_unknown_publication_without_repeating_it() {
    let root =
        std::env::temp_dir().join(format!("ion-theme-unknown-save-{}", uuid::Uuid::now_v7()));
    fs::create_dir_all(&root).unwrap();
    let mut ui = Frontend {
        themes: Themes::load(&root, None).unwrap(),
        ..Frontend::default()
    };
    let files = ui.themes.files();
    ui.theme_io.job = Some(Job {
        kind: JobKind::Save,
        task: tokio::task::spawn_blocking(move || {
            files.save("light").unwrap();
            panic!("save result lost after publication");
        }),
    });
    let error = ui.shutdown_theme_io().await.unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("publication may be unknown"));
    assert!(!message.contains("was not saved"));
    assert_eq!(
        ui.themes.name(),
        "system",
        "a lost result is not a positive save receipt"
    );
    assert_eq!(Themes::load(&root, None).unwrap().name(), "light");
    use std::os::unix::fs::MetadataExt;
    let published = fs::File::open(root.join("settings.json")).unwrap();
    let bytes = fs::read(root.join("settings.json")).unwrap();
    ui.shutdown_theme_io().await.unwrap();
    assert_eq!(fs::read(root.join("settings.json")).unwrap(), bytes);
    assert_eq!(
        fs::metadata(root.join("settings.json")).unwrap().ino(),
        published.metadata().unwrap().ino()
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn cleanup_retains_worker_failure_and_drains_confirmed_save_after_cancelled_wait() {
    let root = std::env::temp_dir().join(format!("ion-theme-custody-{}", uuid::Uuid::now_v7()));
    fs::create_dir_all(&root).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut ui = Frontend {
            themes: Themes::load(&root, None).unwrap(),
            ..Frontend::default()
        };
        ui.theme_io.job = Some(Job {
            kind: JobKind::Inspection,
            task: tokio::task::spawn_blocking(|| panic!("native theme read failed")),
        });
        while !ui.theme_io.job.as_ref().unwrap().task.is_finished() {
            tokio::task::yield_now().await;
        }
        let (release, receive) = mpsc::channel();
        let (ready, started) = tokio::sync::oneshot::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            ready.send(()).unwrap();
            let _ = receive.recv();
        });
        started.await.unwrap();
        ui.theme_io
            .request(Request::Save("light".into()), &ui.themes)
            .unwrap();
        let mut closing = Box::pin(ui.shutdown_theme_io());
        assert!(futures_util::poll!(closing.as_mut()).is_pending());
        drop(closing);
        assert!(!root.join("settings.json").exists());
        release.send(()).unwrap();
        blocker.await.unwrap();
        let error = ui.shutdown_theme_io().await.unwrap_err();
        assert!(format!("{error:#}").contains("native theme read failed"));
        assert!(!format!("{error:#}").contains("publication may be unknown"));
        assert_eq!(Themes::load(&root, None).unwrap().name(), "light");
        assert_eq!(ui.themes.name(), "light");
        assert!(!ui.theme_io.has_job());
        ui.shutdown_theme_io().await.unwrap(); // receipt consumed only after draining
    });
    fs::remove_dir_all(root).unwrap();
}
