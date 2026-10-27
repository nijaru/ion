//! Offline command over Host checks plus App-owned keybinding validation.
use std::{
    io::{self, Write},
    path::Path,
};

use anyhow::{Result, ensure};
use ion_host::{ConfigurationCheck, ConfigurationStatus, Host};

pub(crate) fn run(host: &Host, cwd: &Path) -> Result<()> {
    let mut checks = host.check_configuration(cwd);
    let (status, message) = match crate::keybindings::KeyBindings::load(host.config_root()) {
        Ok(_) => (
            ConfigurationStatus::Ok,
            "keybindings.json valid (defaults when absent)".into(),
        ),
        // The keybinding parser already withholds input values: these errors
        // contain only safe locations and fixed conflicting action identifiers.
        Err(error) => (ConfigurationStatus::Failed, format!("{error:#}")),
    };
    checks.push(ConfigurationCheck {
        area: "keybindings",
        status,
        message,
    });
    let mut output = io::stdout().lock();
    for check in &checks {
        let status = match check.status {
            ConfigurationStatus::Ok => "ok",
            ConfigurationStatus::Warning => "warn",
            ConfigurationStatus::Failed => "fail",
        };
        writeln!(
            output,
            "{status}\t{}\t{}",
            check.area,
            crate::metadata_field(&check.message)
        )?;
    }
    writeln!(
        output,
        "Offline configuration checks only; no network requests, MCP/tool execution, writes or Session integrity checks."
    )?;
    ensure!(
        !checks
            .iter()
            .any(|check| check.status == ConfigurationStatus::Failed),
        "local configuration checks failed"
    );
    Ok(())
}
