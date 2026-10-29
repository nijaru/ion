//! Private MCP artifacts, shared by tool and resource delivery.
use std::{
    fs::{self, OpenOptions},
    io::{BufWriter, Write},
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
};

use anyhow::Result as AnyResult;
use rmcp::model::CallToolResult;

/// The original wire response, streamed to a private artifact. Nothing is
/// cloned or base64-copied into the model context.
pub(super) fn save_raw_mcp_result(result: &CallToolResult) -> AnyResult<PathBuf> {
    write_private_artifact(
        private_artifact_path("ion-mcp-raw-result", ".json")?,
        |file| {
            let mut writer = BufWriter::new(file);
            serde_json::to_writer(&mut writer, result)?;
            writer.flush()?;
            Ok(())
        },
    )
}
pub(super) fn save_full_mcp_output(bytes: &[u8]) -> AnyResult<PathBuf> {
    write_private_artifact(private_artifact_path("ion-mcp-output", ".json")?, |file| {
        file.write_all(bytes)?;
        Ok(())
    })
}

pub(super) fn private_artifact_path(prefix: &str, extension: &str) -> AnyResult<PathBuf> {
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce)
        .map_err(|error| anyhow::anyhow!("cannot name MCP artifact: {error}"))?;
    let serial = u128::from_ne_bytes(nonce);
    Ok(std::env::temp_dir().join(format!(
        "{prefix}-{}-{serial:032x}{extension}",
        std::process::id()
    )))
}

/// A private 0600 artifact. A failed write removes its partial file instead of
/// leaving untrustworthy evidence, and reports the real error.
pub(super) fn write_private_artifact(
    path: PathBuf,
    write: impl FnOnce(&mut fs::File) -> AnyResult<()>,
) -> AnyResult<PathBuf> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    if let Err(error) = write(&mut file) {
        drop(file);
        let _ = fs::remove_file(&path);
        return Err(error);
    }
    Ok(path)
}
