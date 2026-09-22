#![cfg(unix)]

use ion_core::{
    AttemptId, EffectSummary, InvocationId, SessionId, WorkspaceBinding, workspace_registry::*,
};
use std::{
    fs,
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Fixture {
    home: PathBuf,
    root: PathBuf,
    registry: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let home = std::env::temp_dir().join(format!("ion-registry-{}", SessionId::new()));
        let root = home.join("checkout");
        let registry = home.join("host");
        fs::create_dir_all(&root).unwrap();
        Self {
            home,
            root,
            registry,
        }
    }
    fn open(&self) -> WorkspaceRegistry {
        WorkspaceRegistry::open(&self.registry).unwrap()
    }
    fn bind(&self, registry: &mut WorkspaceRegistry) -> WorkspaceBinding {
        registry.bind("workspace", &self.root, "local-v1").unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.home).unwrap();
    }
}
fn key() -> ClaimKey {
    ClaimKey {
        session: SessionId::new(),
        invocation: InvocationId::new(1).unwrap(),
        attempt: AttemptId::new(2).unwrap(),
    }
}
fn receipt(id: &str) -> RegistryReceipt {
    RegistryReceipt {
        backend: "local-v1".into(),
        identity: id.into(),
    }
}
fn evidence(effect: EffectSummary) -> TerminalEvidence {
    TerminalEvidence {
        receipt: receipt("terminal-receipt"),
        effect,
    }
}
fn admit(
    registry: &mut WorkspaceRegistry,
    binding: &WorkspaceBinding,
    key: ClaimKey,
) -> WorkspaceClaim {
    registry
        .admit(
            binding,
            key,
            WorkspaceResources::Files,
            registry.revision(binding).unwrap(),
        )
        .unwrap()
}

#[test]
fn terminal_evidence_is_atomic_idempotent_and_revisioned() {
    let f = Fixture::new();
    let mut r = f.open();
    let b = f.bind(&mut r);
    for (effect, revision) in [
        (EffectSummary::NoMutation, 0),
        (
            EffectSummary::KnownChanges {
                paths: vec!["a".into()],
            },
            1,
        ),
        (EffectSummary::MayHaveMutated, 2),
    ] {
        let k = key();
        let claim = admit(&mut r, &b, k);
        r.record_start(k, receipt("started")).unwrap();
        assert!(matches!(
            r.admit(&b, key(), WorkspaceResources::Files, claim.base),
            Err(RegistryError::Conflict)
        ));
        let terminal = evidence(effect);
        r.resolve(k, terminal.clone()).unwrap();
        r.resolve(k, terminal.clone()).unwrap();
        assert_eq!(r.revision(&b).unwrap().files, revision);
        assert_eq!(r.claim(k).unwrap().terminal, Some(terminal));
        assert!(matches!(
            r.resolve(
                k,
                TerminalEvidence {
                    receipt: receipt("different"),
                    effect: EffectSummary::NoMutation
                }
            ),
            Err(RegistryError::EvidenceConflict)
        ));
    }
    assert!(r.unresolved(None, 10).unwrap().is_empty());
    assert!(matches!(
        r.admit(
            &b,
            key(),
            WorkspaceResources::Files,
            WorkspaceRevision {
                files: 0,
                repository: 0
            }
        ),
        Err(RegistryError::StaleRevision)
    ));
}

#[test]
fn replacement_rejects_admission_and_retains_old_quarantine() {
    let f = Fixture::new();
    let mut r = f.open();
    let b = f.bind(&mut r);
    let k = key();
    let claim = admit(&mut r, &b, k);
    fs::rename(&f.root, f.home.join("old")).unwrap();
    fs::create_dir(&f.root).unwrap();
    assert!(matches!(
        r.admit(&b, key(), WorkspaceResources::Files, claim.base),
        Err(RegistryError::BindingChanged)
    ));
    assert!(matches!(
        f.open().bind("workspace", &f.root, "local-v1"),
        Err(RegistryError::BindingChanged)
    ));
    assert_eq!(r.claim(k).unwrap(), claim);
    let replacement = r.bind("replacement", &f.root, "local-v1").unwrap();
    assert_eq!(r.revision(&replacement).unwrap().files, 0);
    // A possibly-live old process may still reach the reused path.
    assert!(matches!(
        r.admit(&replacement, key(), WorkspaceResources::Files, claim.base),
        Err(RegistryError::Conflict)
    ));
    r.resolve(k, evidence(EffectSummary::MayHaveMutated))
        .unwrap();
    assert_eq!(r.revision(&b).unwrap().files, 1);
    assert_eq!(r.revision(&replacement).unwrap().files, 0);
}

#[test]
fn late_resolution_does_not_transfer_revision_into_replacement_subtree() {
    let f = Fixture::new();
    let mut r = f.open();
    let b = f.bind(&mut r);
    let k = key();
    admit(&mut r, &b, k);
    fs::rename(&f.root, f.home.join("old-checkout")).unwrap();
    fs::create_dir_all(f.root.join("nested")).unwrap();
    let replacement = r
        .bind("replacement-child", f.root.join("nested"), "local-v1")
        .unwrap();
    assert_eq!(r.revision(&replacement).unwrap().files, 0);
    r.resolve(k, evidence(EffectSummary::MayHaveMutated))
        .unwrap();
    assert_eq!(r.revision(&b).unwrap().files, 1);
    assert_eq!(
        r.revision(&replacement).unwrap().files,
        0,
        "path overlap does not make the new subtree part of the old object"
    );
}

#[test]
fn session_and_blob_loss_do_not_erase_orphan_or_receipts() {
    let f = Fixture::new();
    let mut r = f.open();
    let b = f.bind(&mut r);
    let k = key();
    fs::create_dir_all(f.home.join("session/blobs")).unwrap();
    fs::write(f.home.join("session/session.sqlite"), "independent session").unwrap();
    fs::write(f.home.join("session/blobs/output"), "large output").unwrap();
    admit(&mut r, &b, k);
    r.record_start(k, receipt("durable backend start")).unwrap();
    drop(r);
    fs::remove_dir_all(f.home.join("session")).unwrap();
    let mut r = f.open();
    let orphan = r.unresolved(None, 1).unwrap().remove(0);
    assert_eq!(orphan.key, k);
    assert_eq!(orphan.start, Some(receipt("durable backend start")));
    assert!(matches!(
        r.admit(&b, key(), WorkspaceResources::Files, orphan.base),
        Err(RegistryError::Conflict)
    ));
    r.resolve(k, evidence(EffectSummary::MayHaveMutated))
        .unwrap();
    assert_eq!(r.revision(&b).unwrap().files, 1);
}

#[test]
fn registry_placement_and_overlapping_roots_are_checked() {
    let f = Fixture::new();
    let mut inside = WorkspaceRegistry::open(f.root.join(".ion")).unwrap();
    assert!(matches!(
        inside.bind("bad", &f.root, "local-v1"),
        Err(RegistryError::RegistryInWorkspace)
    ));
    let mut r = f.open();
    let b = f.bind(&mut r);
    fs::create_dir(f.root.join("nested")).unwrap();
    let nested = r.bind("nested", f.root.join("nested"), "local-v1").unwrap();
    let k = key();
    admit(&mut r, &nested, k);
    assert!(matches!(
        r.admit(
            &b,
            key(),
            WorkspaceResources::Files,
            r.revision(&b).unwrap()
        ),
        Err(RegistryError::Conflict)
    ));
    r.resolve(
        k,
        evidence(EffectSummary::KnownChanges {
            paths: vec!["file".into()],
        }),
    )
    .unwrap();
    assert_eq!(r.revision(&b).unwrap().files, 1);
}

#[test]
fn shared_git_common_directory_conflicts_and_invalidates_other_worktree() {
    let f = Fixture::new();
    let common = f.home.join("common");
    let second = f.home.join("second");
    fs::create_dir_all(common.join("worktrees/one")).unwrap();
    fs::create_dir_all(common.join("worktrees/two")).unwrap();
    fs::create_dir(&second).unwrap();
    for (root, name) in [(&f.root, "one"), (&second, "two")] {
        fs::write(
            root.join(".git"),
            format!(
                "gitdir: {}\n",
                common.join("worktrees").join(name).display()
            ),
        )
        .unwrap();
        fs::write(
            common.join("worktrees").join(name).join("commondir"),
            "../..\n",
        )
        .unwrap();
    }
    let mut r = f.open();
    let b = f.bind(&mut r);
    let other = r.bind("other", &second, "local-v1").unwrap();
    let base = r.revision(&other).unwrap();
    let k = key();
    r.admit(
        &b,
        k,
        WorkspaceResources::FilesAndRepository,
        r.revision(&b).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        r.admit(&other, key(), WorkspaceResources::FilesAndRepository, base),
        Err(RegistryError::Conflict)
    ));
    // File-only claims in independent worktrees may coexist.
    let file_key = key();
    admit(&mut r, &other, file_key);
    r.resolve(k, evidence(EffectSummary::MayHaveMutated))
        .unwrap();
    assert_eq!(
        r.revision(&other).unwrap(),
        WorkspaceRevision {
            files: 0,
            repository: 1
        }
    );
    r.resolve(file_key, evidence(EffectSummary::NoMutation))
        .unwrap();
    assert!(matches!(
        r.admit(&other, key(), WorkspaceResources::FilesAndRepository, base),
        Err(RegistryError::StaleRevision)
    ));
    fs::rename(&common, f.home.join("old-common")).unwrap();
    fs::create_dir_all(common.join("worktrees/one")).unwrap();
    fs::write(common.join("worktrees/one/commondir"), "../..").unwrap();
    assert!(matches!(
        r.admit(
            &b,
            key(),
            WorkspaceResources::Files,
            r.revision(&b).unwrap()
        ),
        Err(RegistryError::BindingChanged)
    ));
}

#[test]
fn symlink_retarget_backend_change_and_checkout_registry_are_refused() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let mut r = f.open();
    let b = f.bind(&mut r);
    assert!(matches!(
        r.bind("workspace", &f.root, "different-backend"),
        Err(RegistryError::BindingChanged)
    ));
    let other = f.home.join("other");
    fs::create_dir(&other).unwrap();
    fs::rename(&f.root, f.home.join("original")).unwrap();
    symlink(&other, &f.root).unwrap();
    assert!(matches!(
        r.admit(
            &b,
            key(),
            WorkspaceResources::Files,
            r.revision(&b).unwrap()
        ),
        Err(RegistryError::BindingChanged)
    ));
    fs::create_dir(other.join(".git")).unwrap();
    assert!(matches!(
        WorkspaceRegistry::open(other.join(".ion")),
        Err(RegistryError::RegistryInWorkspace)
    ));
}

#[test]
fn oversized_evidence_and_wrong_backend_cannot_release_claim() {
    let f = Fixture::new();
    let mut r = f.open();
    let b = f.bind(&mut r);
    let k = key();
    admit(&mut r, &b, k);
    assert!(matches!(
        r.resolve(
            k,
            evidence(EffectSummary::KnownChanges {
                paths: vec!["x".repeat(20_000)]
            })
        ),
        Err(RegistryError::Capacity)
    ));
    assert!(matches!(
        r.resolve(
            k,
            TerminalEvidence {
                receipt: RegistryReceipt {
                    backend: "other".into(),
                    identity: "done".into()
                },
                effect: EffectSummary::NoMutation
            }
        ),
        Err(RegistryError::EvidenceConflict)
    ));
    assert!(r.claim(k).unwrap().terminal.is_none());
    assert_eq!(r.revision(&b).unwrap().files, 0);
}

#[test]
fn process_child() {
    let Ok(home) = std::env::var("ION_REGISTRY_CHILD") else {
        return;
    };
    let home = PathBuf::from(home);
    let mut r = WorkspaceRegistry::open(home.join("host")).unwrap();
    let b = r
        .bind("workspace", home.join("checkout"), "local-v1")
        .unwrap();
    let k: ClaimKey = serde_json::from_slice(&fs::read(home.join("key")).unwrap()).unwrap();
    admit(&mut r, &b, k);
    r.record_start(k, receipt("child-started")).unwrap();
    fs::write(
        home.join("checkout/mutated"),
        "effect before Session commit",
    )
    .unwrap();
    fs::write(home.join("ready"), "ready").unwrap();
    loop {
        thread::sleep(Duration::from_secs(1));
    }
}

struct OwnerProcess(std::process::Child);

impl Drop for OwnerProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn actual_process_death_retains_claim_and_cross_process_exclusion() {
    let f = Fixture::new();
    let mut r = f.open();
    let b = f.bind(&mut r);
    let k = key();
    fs::write(f.home.join("key"), serde_json::to_vec(&k).unwrap()).unwrap();
    let mut child = OwnerProcess(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "process_child", "--nocapture"])
            .env("ION_REGISTRY_CHILD", &f.home)
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while !f.home.join("ready").exists() {
        if Instant::now() >= deadline {
            child.0.kill().unwrap();
            child.0.wait().unwrap();
            panic!("child readiness timed out");
        }
        assert!(child.0.try_wait().unwrap().is_none());
        thread::sleep(Duration::from_millis(10));
    }
    assert!(matches!(
        r.admit(
            &b,
            key(),
            WorkspaceResources::Files,
            r.revision(&b).unwrap()
        ),
        Err(RegistryError::Conflict)
    ));
    child.0.kill().unwrap();
    assert!(!child.0.wait().unwrap().success());
    drop(r);
    let mut r = f.open();
    assert!(f.root.join("mutated").exists());
    assert_eq!(r.claim(k).unwrap().start, Some(receipt("child-started")));
    assert!(matches!(
        r.admit(
            &b,
            key(),
            WorkspaceResources::Files,
            r.revision(&b).unwrap()
        ),
        Err(RegistryError::Conflict)
    ));
    r.resolve(k, evidence(EffectSummary::MayHaveMutated))
        .unwrap();
    admit(&mut r, &b, key());
}
