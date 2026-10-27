//! Read-only local configuration checks; never assemble or admit an application.
use std::{io, path::Path};

use crate::{CommandCatalog, CommandEnvironment, CredentialStatus, Host, ModelSelectionRequired};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigurationStatus {
    Ok,
    Warning,
    Failed,
}

pub struct ConfigurationCheck {
    pub area: &'static str,
    pub status: ConfigurationStatus,
    pub message: String,
}

impl Host {
    /// Inspect local configuration and credential presence, not remote access.
    /// Independent checks continue after failure. This executes no configured
    /// program/setup, connects no MCP/provider, writes no files and opens no Session.
    /// Error values may contain credentials or private configuration: retain only
    /// safe structural locations/categories, never arbitrary error text.
    pub fn check_configuration(&self, cwd: &Path) -> Vec<ConfigurationCheck> {
        use ConfigurationStatus::{Failed, Ok as Ready, Warning};
        let mut checks = Vec::new();
        let mut add = |area, status, message| {
            checks.push(ConfigurationCheck {
                area,
                status,
                message,
            })
        };
        match self.models().choose(None, None, self.credentials()) {
            Ok(selected) => {
                add(
                    "model",
                    Ready,
                    format!(
                        "{}/{} (local selection; remote access not tested)",
                        selected.provider, selected.model
                    ),
                );
                let (status, message) = match self
                    .credentials()
                    .status(&selected.provider, selected.api_key_env.as_deref())
                {
                    Ok(CredentialStatus::Environment) => (
                        Ready,
                        "environment credential present; not authenticated".into(),
                    ),
                    Ok(CredentialStatus::Saved) => {
                        (Ready, "saved credential present; not authenticated".into())
                    }
                    Ok(CredentialStatus::Missing) if !selected.requires_key => {
                        (Ready, "selected route requires no API key".into())
                    }
                    Ok(CredentialStatus::Missing) => (
                        Failed,
                        format!(
                            "missing credential; run ion login {} or supply the configured environment key",
                            selected.provider
                        ),
                    ),
                    Err(error) => (
                        Failed,
                        format!(
                            "cannot use the selected credential: {}; check credential permissions and value format",
                            problem(&error)
                        ),
                    ),
                };
                add("credentials", status, message);
            }
            Err(error) => {
                let message = if error.is::<ModelSelectionRequired>() {
                    "no model selected; run ion models, then ion use PROVIDER MODEL, or supply a provider key".into()
                } else {
                    format!(
                        "cannot resolve local model selection: {}; check models.json and credential files",
                        problem(&error)
                    )
                };
                add("model", Failed, message);
                add(
                    "credentials",
                    Warning,
                    "not checked for a selected route because model selection failed".into(),
                );
            }
        }
        let settings = (|| {
            let settings = crate::user_settings::UserSettings::load(self.config_root())?;
            CommandEnvironment::coding_with_settings(settings.shell()?)?;
            settings.search()?.validate()?;
            anyhow::Ok(())
        })();
        match settings {
            Ok(()) => add(
                "settings",
                Ready,
                "settings.json and shell capture valid; setup not executed".into(),
            ),
            Err(error) => add(
                "settings",
                Failed,
                format!(
                    "cannot use settings.json: {}; check shell path/setup and search settings",
                    problem(&error)
                ),
            ),
        }
        match self.resources(cwd) {
            Ok(resources) => {
                let count = resources.diagnostics().len();
                add(
                    "resources",
                    if count == 0 { Ready } else { Warning },
                    format!(
                        "{} skill(s), {} template(s), {count} omitted/unreadable resource diagnostic(s)",
                        resources.skills().count(),
                        resources.templates().count()
                    ),
                );
                for diagnostic in resources.diagnostics() {
                    // Resource parser errors may quote private frontmatter/body.
                    add(
                        "resources",
                        Warning,
                        format!(
                            "inspect omitted/unreadable resource {}",
                            diagnostic.path.display()
                        ),
                    );
                }
            }
            Err(error) => add(
                "resources",
                Failed,
                format!(
                    "cannot inspect workspace resources: {}; check paths and permissions",
                    problem(&error)
                ),
            ),
        }
        match CommandCatalog::load(cwd, self.config_root()) {
            Ok(commands) => {
                let count = commands.diagnostics().len();
                add(
                    "commands",
                    if count == 0 { Ready } else { Warning },
                    format!(
                        "{} enabled command(s), {count} skipped/invalid manifest diagnostic(s); inspect ion commands list; no program executed",
                        commands.iter().count()
                    ),
                );
            }
            Err(error) => add(
                "commands",
                Warning,
                format!(
                    "cannot inspect command manifests: {}; inspect ion commands list; no program executed",
                    problem(&error)
                ),
            ),
        }
        match self.mcp_config_for(cwd).and_then(|config| config.list()) {
            Ok(listing) => {
                let count = listing.diagnostics.len();
                add(
                    "mcp",
                    if count == 0 { Ready } else { Warning },
                    format!(
                        "{} configured server(s), {count} skipped/invalid manifest diagnostic(s); inspect ion mcp list; no server connected",
                        listing.servers.len()
                    ),
                );
            }
            Err(error) => add(
                "mcp",
                Warning,
                format!(
                    "cannot inspect MCP manifests: {}; inspect ion mcp list; no server connected",
                    problem(&error)
                ),
            ),
        }
        checks
    }
}

fn problem(error: &anyhow::Error) -> String {
    if let Some(error) = error.downcast_ref::<serde_json::Error>() {
        format!(
            "invalid JSON/field at line {}, column {}",
            error.line(),
            error.column()
        )
    } else if let Some(error) = error.downcast_ref::<crate::user_settings::InvalidSettings>() {
        error.to_string()
    } else if let Some(error) = error.downcast_ref::<io::Error>() {
        format!("I/O {:?}", error.kind())
    } else {
        "invalid configuration (values withheld)".into()
    }
}
