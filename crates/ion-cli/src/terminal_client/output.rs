//! Save the raw captures behind a displayed result, never its text preview.
use std::{
    fs::{self, DirBuilder, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::Path,
    sync::Arc,
};

use anyhow::{Context, Result};
use ion_core::{ArtifactRef, Session};

use super::{Frontend, session_io::ReadOutput};

pub(super) fn start(ui: &mut Frontend, session: &Arc<Session>) -> Result<()> {
    let Some(captures) = ui
        .details
        .as_ref()
        .and_then(|details| details.view.visible_captures())
    else {
        anyhow::bail!("Scroll to the capture summary to save output");
    };
    let captures = captures.to_vec();
    let cwd = ui.cwd.clone();
    let io = session.io()?;
    ui.session_io.start_file_write(session, async move {
        io.run(move |session| Ok(save(session.path(), &cwd, &captures)))
            .await?
    })
}

fn save(session: &Path, cwd: &Path, captures: &[ArtifactRef]) -> Result<ReadOutput> {
    let name = format!(
        "ion-output-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..12]
    );
    let folder = cwd.join(&name);
    DirBuilder::new()
        .mode(0o700)
        .create(&folder)
        .with_context(|| format!("cannot create {}", folder.display()))?;
    let result = (|| {
        let mut incomplete = false;
        for (index, capture) in captures.iter().enumerate() {
            let label = match capture.label.as_str() {
                "stdout" => "stdout",
                "stderr" => "stderr",
                _ => "capture",
            };
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(folder.join(format!("{label}-{index}.bin")))?;
            let receipt = Session::export_artifact(session, capture.id, &mut file)?;
            file.flush()?;
            incomplete |= !receipt.complete;
        }
        Ok(ReadOutput::OutputSaved { name, incomplete })
    })();
    if result.is_err() {
        fs::remove_dir_all(&folder).with_context(|| {
            format!("could not remove unfinished output at {}", folder.display())
        })?;
    }
    result
}
