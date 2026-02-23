//! TUI entry point.

use crate::cli::PermissionSettings;
use anyhow::Result;
use crossterm::{cursor::Show, execute, terminal::disable_raw_mode};
use std::io;

/// Resume option for TUI mode.
#[derive(Debug, Clone)]
pub enum ResumeOption {
    None,
    Latest,
    ById(String),
    Selector,
}

/// Guard that restores the original panic hook on drop.
struct PanicHookGuard {
    original_hook: std::sync::Arc<dyn Fn(&std::panic::PanicHookInfo) + Send + Sync + 'static>,
}

impl Drop for PanicHookGuard {
    fn drop(&mut self) {
        let original_hook = std::sync::Arc::clone(&self.original_hook);
        std::panic::set_hook(Box::new(move |info| {
            (original_hook)(info);
        }));
    }
}

/// Main entry point for the TUI.
///
/// Creates an `IonApp`, applies any resume option, then hands off to
/// `AppBuilder` which owns terminal setup and the event loop.
pub async fn run(permissions: PermissionSettings, resume_option: ResumeOption) -> Result<()> {
    // Set panic hook to restore terminal on panic (guard restores original on exit).
    let original_hook: std::sync::Arc<dyn Fn(&std::panic::PanicHookInfo) + Send + Sync> =
        std::sync::Arc::from(std::panic::take_hook());
    let hook_for_panic = std::sync::Arc::clone(&original_hook);
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), Show);
        (hook_for_panic)(info);
    }));
    let _panic_guard = PanicHookGuard { original_hook };

    let mut ion_app = crate::tui::ion_app::IonApp::new(permissions).await?;
    ion_app.apply_resume(resume_option);

    tui::app::AppBuilder::new(ion_app)
        .fullscreen()
        .mouse(true)
        .bracketed_paste(true)
        .focus_events(true)
        .run()
        .await
        .map(|_| ())
        .map_err(anyhow::Error::from)
}
