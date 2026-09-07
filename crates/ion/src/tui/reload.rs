//! Live configuration preparation and application for the interactive host.
//!
//! Preparation is read-only. Application is ordered and can partially apply
//! if a runtime command fails; only a completely applied candidate is reported
//! as successful. Peer removal changes availability, not durable authority.

use super::{KeyMap, Theme, UiState, notice};
use crate::settings::Settings;
use ion_core::{ExtensionDef, ServerDef, SessionHandle, TrustedResource};
use std::collections::HashSet;
use std::path::Path;

pub(super) struct ReloadHost<'a> {
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
        validate_peers(
            extensions
                .iter()
                .map(|def| (&def.name, &def.command, &def.args)),
        )?;
        validate_peers(mcp.iter().map(|def| (&def.name, &def.command, &def.args)))?;
        Ok(Self {
            settings,
            keymap,
            model_catalog,
            default_model_reference,
            resources,
            extensions,
            mcp,
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

fn validate_peers<'a>(
    defs: impl Iterator<Item = (&'a String, &'a String, &'a Vec<String>)>,
) -> Result<(), String> {
    let mut names = HashSet::new();
    for (name, command, args) in defs {
        if name.trim().is_empty() || command.trim().is_empty() {
            return Err("peer names and commands must not be empty".to_owned());
        }
        if !names.insert(name) {
            return Err(format!("duplicate peer name {name}"));
        }
        if name.contains('\0')
            || command.contains('\0')
            || args.iter().any(|arg| arg.contains('\0'))
        {
            return Err(format!("peer {name} contains a NUL byte"));
        }
    }
    Ok(())
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
            "reloaded keybindings, theme, catalog and {count} trusted resources; peer supervisors: {started} started, {stopped} stopped (connection readiness is separate); MCP active selection remains launch-configured"
        ),
    );
    if stopped > 0 {
        notice(
            state,
            "stopped peer availability; existing lane scope grants are retained",
        );
    }
}

async fn apply_runtime(
    session: &SessionHandle,
    candidate: &Candidate,
    host: &ReloadHost<'_>,
) -> Result<(usize, usize), String> {
    session
        .set_trusted_resources(candidate.resources.clone())
        .await
        .map_err(|err| format!("context: {err}"))?;
    let mut counts = (0, 0);
    if let Some(catalog) = host.catalog {
        if let Some(service) = host.extension_service {
            let (started, stopped) = service.ensure(&candidate.extensions, catalog).await;
            counts.0 += started;
            counts.1 += stopped;
            for def in &candidate.extensions {
                session
                    .admit_structural_scope(format!("ext:{}", def.name))
                    .await
                    .map_err(|err| format!("admit ext:{}: {err}", def.name))?;
            }
        }
        if let Some(service) = host.mcp_service {
            let (started, stopped) = service.ensure(&candidate.mcp, catalog).await;
            counts.0 += started;
            counts.1 += stopped;
            for def in &candidate.mcp {
                session
                    .admit_structural_scope(format!("mcp:{}", def.name))
                    .await
                    .map_err(|err| format!("admit mcp:{}: {err}", def.name))?;
            }
        }
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
            store,
            std::sync::Arc::new(ion_core::AllowlistPolicy::new(Vec::<String>::new())),
        );
        let session = runtime.session();
        session.close().await.expect("close");
        runtime.join().await.expect("join");
        let host = ReloadHost {
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
            catalog: Some(&catalog),
            mcp_service: None,
            extension_service: None,
            provider: None,
        };
        assert!(candidate.validate_host(&host).is_err());
    }
}
