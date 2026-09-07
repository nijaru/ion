//! Applying settings that cross the session and host configuration boundary.
use super::{SessionHandle, UiState, notice};
use std::path::Path;

pub(super) async fn save_thinking_default(
    session: &SessionHandle,
    state: &mut UiState,
    path: &Path,
    thinking: &str,
) {
    if let Err(err) = session.switch_thinking(Some(thinking.to_owned())).await {
        notice(state, &format!("thinking unchanged: {err}"));
        return;
    }
    // The lane is authoritative for the live setting. File persistence is a
    // separate host effect; a failure must not pretend the lane rolled back.
    state.thinking_level = Some(thinking.to_owned());
    if let Some(selector) = &mut state.settings_selector
        && let Some(row) = selector
            .rows
            .iter_mut()
            .find(|row| row.id == "defaultThinkingLevel")
    {
        row.value = thinking.to_owned();
    }
    match crate::settings::Settings::write_setting(path, "defaultThinkingLevel", thinking) {
        Ok(_) => notice(state, &format!("thinking: {thinking}; default saved")),
        Err(err) => notice(
            state,
            &format!("thinking: {thinking}; default was not saved: {err}"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ion_core::{Runtime, ScriptedProvider, SessionStore, ToolRegistry};

    #[tokio::test]
    async fn default_thinking_cycles_after_acceptance_and_persists() {
        let runtime = Runtime::start_with_store(
            ScriptedProvider::echo(),
            ToolRegistry::default(),
            SessionStore::open_in_memory().unwrap(),
        );
        let session = runtime.session();
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("settings.toml");
        let mut state = UiState::new();
        state.open_settings_selector();
        save_thinking_default(&session, &mut state, &path, "high").await;
        assert_eq!(
            session.snapshot().await.unwrap().thinking.as_deref(),
            Some("high")
        );
        let settings: toml::Value =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(settings["defaultThinkingLevel"].as_str(), Some("high"));
        let selector = state.settings_selector.as_mut().unwrap();
        selector.selected = selector
            .rows
            .iter()
            .position(|row| row.id == "defaultThinkingLevel")
            .unwrap();
        assert!(
            matches!(state.cycle_settings_row(), Some(super::super::UiEffect::SaveThinkingDefault { thinking }) if thinking == "xhigh")
        );
        session.close().await.unwrap();
        runtime.join().await.unwrap();
        save_thinking_default(&session, &mut state, &path, "low").await;
        assert_eq!(state.thinking_level.as_deref(), Some("high"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "defaultThinkingLevel = \"high\"\n"
        );
    }

    #[tokio::test]
    async fn failed_default_write_keeps_the_accepted_lane_and_reports_partial_application() {
        let runtime = Runtime::start_with_store(
            ScriptedProvider::echo(),
            ToolRegistry::default(),
            SessionStore::open_in_memory().unwrap(),
        );
        let session = runtime.session();
        let root = tempfile::tempdir().unwrap();
        let mut state = UiState::new();
        save_thinking_default(&session, &mut state, root.path(), "low").await;
        assert_eq!(state.thinking_level.as_deref(), Some("low"));
        assert!(
            state
                .pending_scrollback
                .iter()
                .any(|line| line.to_string().contains("default was not saved"))
        );
        session.close().await.unwrap();
        runtime.join().await.unwrap();
    }
}
