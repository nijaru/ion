//! Real, isolated Session storage for tests of retained native observations.
use std::{
    path::PathBuf,
    sync::{Arc, Weak},
};

pub(crate) struct ArtifactFixture {
    pub session: Arc<ion_core::Session>,
    _directory: Directory,
}
struct Directory {
    path: PathBuf,
    session: Weak<ion_core::Session>,
}
impl Drop for Directory {
    fn drop(&mut self) {
        // Never unlink a database while a failed test still has started I/O.
        if self.session.strong_count() == 0 {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}
impl ArtifactFixture {
    pub fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("ion-artifact-fixture-{}", uuid::Uuid::new_v4()));
        let session =
            Arc::new(ion_core::Session::create(path.join("session.sqlite"), None).unwrap());
        let directory = Directory {
            path,
            session: Arc::downgrade(&session),
        };
        Self {
            session,
            _directory: directory,
        }
    }
    pub fn sink(&self) -> ion_core::ArtifactSink {
        self.session.artifact_sink()
    }
    pub fn json(&self, output: &ion_core::ToolOutput, key: &str) -> serde_json::Value {
        let index = output.value[key].as_u64().expect("typed artifact index") as usize;
        serde_json::from_slice(&self.bytes(&output.artifacts[index])).unwrap()
    }
    pub fn bytes(&self, reference: &ion_core::ArtifactRef) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut offset = 0;
        loop {
            let page = self
                .session
                .read_artifact(reference, offset, 16384)
                .unwrap();
            bytes.extend(page.bytes);
            match page.next_offset {
                Some(next) => offset = next,
                None => return bytes,
            }
        }
    }
}
