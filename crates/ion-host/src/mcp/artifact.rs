//! Typed capture collection for one observed MCP response. Runs in the joined
//! conversion worker; opaque server JSON never owns these references.
use std::io::Write;

use anyhow::{Result, anyhow};
use ion_core::{ArtifactRef, ArtifactSink};
use serde::Serialize;

pub(super) struct ResultArtifacts {
    sink: Option<ArtifactSink>,
    pub references: Vec<ArtifactRef>,
}

impl ResultArtifacts {
    pub fn new(sink: Option<ArtifactSink>) -> Self {
        Self {
            sink,
            references: Vec::new(),
        }
    }

    pub fn bytes(&mut self, label: &str, bytes: &[u8]) -> Result<usize> {
        self.save(label, |writer| {
            writer.write_all(bytes)?;
            Ok(())
        })
    }

    pub fn json(&mut self, label: &str, value: &impl Serialize) -> Result<usize> {
        self.save(label, |writer| {
            serde_json::to_writer(writer, value)?;
            Ok(())
        })
    }

    fn save(
        &mut self,
        label: &str,
        write: impl FnOnce(&mut ion_core::ArtifactWriter) -> Result<()>,
    ) -> Result<usize> {
        let sink = self
            .sink
            .as_ref()
            .ok_or_else(|| anyhow!("raw capture requires an explicit Session artifact sink"))?;
        let mut writer = sink.start(label)?;
        write(&mut writer)?;
        let reference = writer.finish(true)?;
        let index = self.references.len();
        self.references.push(reference);
        Ok(index)
    }
}
