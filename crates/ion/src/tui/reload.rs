//! Live configuration preparation and application for the interactive host.
//!
//! Preparation is read-only. Application is ordered and can partially apply
//! if a runtime command fails; only a completely applied candidate is reported
//! as successful. Durable authority changes precede peer replacement.

use super::{KeyMap, Theme, UiState, notice};
use crate::settings::Settings;
use ion_core::{ExtensionDef, ServerDef, SessionHandle, TrustedResource};
use std::collections::BTreeMap;
use std::path::Path;

pub(super) struct ReloadHost<'a> {
    pub store: Option<&'a ion_core::SessionStore>,
    pub catalog: Option<&'a ion_core::ToolCatalog>,
    pub mcp_service: Option<&'a std::sync::Arc<ion_core::McpService>>,
    pub extension_service: Option<&'a ion_core::ExtensionService>,
    pub provider: Option<&'a std::sync::Arc<ion_core::SwitchingProvider<crate::CliProvider>>>,
}

struct Candidate {
    settings: Settings,
    keymap: KeyMap,
    model_catalog: Vec<String>,
    default_model_reference: Option<String>,
    resources: Vec<TrustedResource>,
    extensions: Vec<ExtensionDef>,
    mcp: Vec<ServerDef>,
    authority: BTreeMap<String, String>,
}

impl Candidate {
    fn prepare(settings: Settings, cwd: &Path, trust_project: bool) -> Result<Self, String> {
        let keymap = KeyMap::from_settings(&settings.keybindings)?;
        let model_catalog = settings.model_catalog()?;
        let default_model_reference = settings
            .model_selection()?
            .map(|selection| format!("{}/{}", selection.provider, selection.model));
        let resources = ion_core::load_trusted_resources(cwd, trust_project)
            .map_err(|err| format!("context files: {err}"))?;
        let extensions = crate::settings::load_extension_defs(&settings, Some(cwd), trust_project)?;
        let mcp: Vec<ServerDef> = settings
            .mcp_servers
            .iter()
            .cloned()
            .map(Into::into)
            .collect();
        let authority = crate::settings::peer_authority_definitions(
            &mcp,
            &extensions,
            &settings.active_mcp_servers,
        )?;
        Ok(Self {
            settings,
            keymap,
            model_catalog,
            default_model_reference,
            resources,
            extensions,
            mcp,
            authority,
        })
    }

    fn validate_host(&self, host: &ReloadHost<'_>) -> Result<(), String> {
        if host.catalog.is_none() && (!self.extensions.is_empty() || !self.mcp.is_empty()) {
            return Err("this host has no tool catalog for configured peers".to_owned());
        }
        if !self.extensions.is_empty() && host.extension_service.is_none() {
            return Err(
                "this host has no extension service; restart to configure extensions".to_owned(),
            );
        }
        if !self.mcp.is_empty() && host.mcp_service.is_none() {
            return Err("this host has no MCP service; restart to configure servers".to_owned());
        }
        Ok(())
    }
}

pub(super) async fn reload_config(
    session: &SessionHandle,
    state: &mut UiState,
    keymap: &mut KeyMap,
    theme: &mut Theme,
    host: ReloadHost<'_>,
    trust_project: bool,
) {
    let prepared = (|| {
        let settings = Settings::load()?;
        let cwd = std::env::current_dir().map_err(|err| err.to_string())?;
        let candidate = Candidate::prepare(settings, &cwd, trust_project)?;
        candidate.validate_host(&host)?;
        Ok::<_, String>(candidate)
    })();
    let candidate = match prepared {
        Ok(candidate) => candidate,
        Err(err) => {
            notice(
                state,
                &format!("reload rejected before applying changes: {err}"),
            );
            return;
        }
    };
    let count = candidate.resources.len();
    let applied = apply_runtime(session, &candidate, &host).await;
    let (started, stopped) = match applied {
        Ok(counts) => counts,
        Err(err) => {
            notice(
                state,
                &format!("reload did not complete; runtime changes may have applied: {err}"),
            );
            return;
        }
    };
    *keymap = candidate.keymap.clone();
    state.set_keymap(candidate.keymap);
    *theme = candidate.settings.theme();
    state.theme = candidate.settings.theme();
    state.thinking_visible = !candidate.settings.hide_thinking_block;
    state.show_cache_miss_notices = candidate.settings.show_cache_miss_notices();
    state.model_catalog = candidate.model_catalog;
    state.default_model_reference = candidate.default_model_reference;
    if let Some(provider) = host.provider {
        provider.invalidate();
    }
    notice(
        state,
        &format!(
            "reloaded keybindings, theme, catalog and {count} trusted resources; peer supervisors: {started} started, {stopped} stopped (connection readiness is separate); peer authority updated"
        ),
    );
    if stopped > 0 {
        notice(
            state,
            "removed or replaced peer grants were revoked; existing lanes need explicit admission for new authority",
        );
    }
}

async fn apply_runtime(
    session: &SessionHandle,
    candidate: &Candidate,
    host: &ReloadHost<'_>,
) -> Result<(usize, usize), String> {
    let mut update = None;
    if let Some(catalog) = host.catalog {
        // Resolve the ledger before taking the write guard so a host that
        // cannot reconcile authority is never fenced by a rejected reload.
        let store = host
            .store
            .ok_or_else(|| "host configuration ledger unavailable".to_owned())?;
        let guard = catalog
            .configuration()
            .try_update()
            .map_err(|err| err.to_string())?;
        if let Err(err) = catalog
            .reconcile_peer_authority(store, candidate.authority.clone(), &guard)
            .await
        {
            // The transaction failed before any host effect or authority
            // change. Keep a previously healthy configuration usable.
            guard.unchanged();
            return Err(format!("authority unchanged: {err}"));
        }
        catalog.set_active_mcp_servers(&candidate.settings.active_mcp_servers);
        update = Some(guard);
    }
    session
        .set_trusted_resources(candidate.resources.clone())
        .await
        .map_err(|err| format!("context: {err}"))?;
    let mut counts = (0, 0);
    if let Some(catalog) = host.catalog {
        if let Some(service) = host.extension_service {
            let (started, stopped) = service
                .ensure(&candidate.extensions, catalog)
                .await
                .map_err(|err| format!("extensions: {err}"))?;
            counts.0 += started;
            counts.1 += stopped;
        }
        if let Some(service) = host.mcp_service {
            let (started, stopped) = service
                .ensure(&candidate.mcp, catalog)
                .await
                .map_err(|err| format!("MCP: {err}"))?;
            counts.0 += started;
            counts.1 += stopped;
        }
        // /reload explicitly adopts the chosen peers for the active main lane.
        // Other lanes keep their revisioned grants and tool-name restrictions.
        for scope in candidate.authority.keys() {
            session
                .admit_structural_scope(scope.clone())
                .await
                .map_err(|err| format!("admit {scope}: {err}"))?;
        }
    }
    if let Some(update) = update {
        update.finish();
    }
    Ok(counts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runtime_rejection_stops_application() {
        let root = tempfile::tempdir().expect("workspace");
        let candidate =
            Candidate::prepare(Settings::default(), root.path(), false).expect("candidate");
        let catalog = ion_core::ToolCatalog::with_cwd(root.path());
        let store = ion_core::SessionStore::open_in_memory().expect("store");
        let runtime = ion_core::Runtime::start_with_policy(
            ion_core::ScriptedProvider::new(vec![]),
            catalog.clone(),
            store.clone(),
            std::sync::Arc::new(ion_core::AllowlistPolicy::new(Vec::<String>::new())),
        );
        let session = runtime.session();
        session.close().await.expect("close");
        runtime.join().await.expect("join");
        let host = ReloadHost {
            store: Some(&store),
            catalog: Some(&catalog),
            mcp_service: None,
            extension_service: None,
            provider: None,
        };
        assert!(
            apply_runtime(&session, &candidate, &host)
                .await
                .expect_err("closed runtime")
                .starts_with("context:")
        );
        // A partially applied reload leaves the host fenced closed until a
        // successful reconciliation reports it (DESIGN.md §19).
        assert!(matches!(
            catalog.configuration().try_enter(),
            Err(ion_core::CommandError::ConfigurationFailed)
        ));
        catalog.close().await.expect("catalog close");
    }

    #[test]
    fn invalid_candidate_is_rejected_before_application() {
        let root = tempfile::tempdir().expect("workspace");
        std::fs::create_dir(root.path().join(".ion")).expect("project directory");
        std::fs::write(root.path().join(".ion/extensions.toml"), "invalid[").expect("fixture");
        assert!(Candidate::prepare(Settings::default(), root.path(), true).is_err());
        assert!(Candidate::prepare(Settings::default(), root.path(), false).is_ok());
    }

    #[test]
    fn duplicate_peer_identity_is_rejected() {
        let root = tempfile::tempdir().expect("workspace");
        let settings: Settings = toml::from_str(
            r#"
            [[mcpServers]]
            name = "duplicate"
            command = "first"
            [[mcpServers]]
            name = "duplicate"
            command = "second"
        "#,
        )
        .expect("settings");
        assert!(Candidate::prepare(settings, root.path(), false).is_err());
    }

    #[test]
    fn configured_peers_require_a_retained_service_owner() {
        let root = tempfile::tempdir().expect("workspace");
        let mut candidate =
            Candidate::prepare(Settings::default(), root.path(), false).expect("candidate");
        candidate.mcp.push(ServerDef {
            name: "docs".into(),
            command: "unused".into(),
            args: vec![],
        });
        let catalog = ion_core::ToolCatalog::with_cwd(root.path());
        let host = ReloadHost {
            store: None,
            catalog: Some(&catalog),
            mcp_service: None,
            extension_service: None,
            provider: None,
        };
        assert!(candidate.validate_host(&host).is_err());
    }
}
