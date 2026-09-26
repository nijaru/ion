use super::*;

struct Fixture {
    home: PathBuf,
    registry: WorkspaceRegistry,
    binding: WorkspaceBinding,
}

impl Fixture {
    fn new() -> Self {
        let home = std::env::temp_dir().join(format!("ion-edit-phases-{}", SessionId::new()));
        let root = home.join("workspace");
        fs::create_dir_all(root.join(".git")).unwrap();
        let mut registry = WorkspaceRegistry::open(home.join("host")).unwrap();
        let binding = registry.bind("workspace", root, "host-edit-v1").unwrap();
        Self {
            home,
            registry,
            binding,
        }
    }

    fn admit(&mut self, key: ClaimKey) -> WorkspaceClaim {
        self.registry
            .admit_edit(
                &self.binding,
                key,
                WorkspaceResources::FilesAndRepository,
                self.registry.revision(&self.binding).unwrap(),
                action(),
            )
            .unwrap()
    }

    fn reopen(&mut self) {
        self.registry = WorkspaceRegistry::open(self.home.join("host")).unwrap();
    }

    fn arm(&mut self, claim: &WorkspaceClaim) {
        let receipt = claim.start.as_ref().unwrap();
        self.registry
            .record_edit_staged(claim.key, receipt, staged())
            .unwrap();
        self.registry
            .record_edit_rename_armed(claim.key, receipt, armed())
            .unwrap();
    }

    fn unchanged(&self, key: ClaimKey, claim: &WorkspaceClaim, revision: WorkspaceRevision) {
        assert_eq!(self.registry.claim(key).unwrap(), *claim);
        assert_eq!(self.registry.revision(&self.binding).unwrap(), revision);
        assert_eq!(
            self.registry.unresolved(None, 10).unwrap(),
            vec![claim.clone()]
        );
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

fn content(bytes: &[u8]) -> EditContent {
    EditContent {
        digest: ContentDigest::of_bytes(bytes),
        bytes: bytes.len() as u64,
    }
}

fn action() -> EditAction {
    EditAction {
        action_digest: ContentDigest::of_bytes(b"prepared action"),
        tool_binding: ContentDigest::of_bytes(b"exact frozen binding"),
        target: "src/file.txt".into(),
        expected: content(b"before"),
        replacement: content(b"after"),
    }
}

fn identity(inode: u64) -> EditPhysicalIdentity {
    EditPhysicalIdentity {
        device: 7,
        inode,
        birth_seconds: 123,
        birth_nanos: 456,
    }
}

fn staged() -> EditStaged {
    EditStaged {
        file: identity(1),
        parent: identity(2),
        content: action().replacement,
        file_synced: true,
        parent_synced: true,
    }
}

fn armed() -> EditRenameArmed {
    EditRenameArmed {
        staged_file: staged().file,
        target_file: identity(3),
        target_parent: identity(4),
    }
}

fn replaced() -> EditTermination {
    EditTermination::Replaced {
        destination_parent_synced: true,
        staging_parent_synced: true,
    }
}

fn evidence(claim: &WorkspaceClaim, effect: EffectSummary) -> TerminalEvidence {
    TerminalEvidence {
        receipt: claim.start.clone().unwrap(),
        effect,
    }
}

fn success(claim: &WorkspaceClaim) -> TerminalEvidence {
    evidence(
        claim,
        EffectSummary::KnownChanges {
            paths: vec![claim.edit.as_ref().unwrap().manifest.action.target.clone()],
        },
    )
}

#[test]
fn admission_atomically_mints_one_receipt_and_bounded_manifest_without_staging() {
    let mut f = Fixture::new();
    let k = key();
    let admitted = f.admit(k);
    let receipt = admitted.start.as_ref().unwrap();
    assert_eq!(receipt.backend, f.binding.backend);
    assert!(receipt.identity.starts_with("edit-attempt-v1:"));
    assert!(receipt.identity.len() <= 160);
    let edit = admitted.edit.as_ref().unwrap();
    assert_eq!(edit.manifest.action, action());
    assert!(edit.manifest.stage_slot.starts_with("edit-"));
    assert!(!edit.manifest.stage_slot.contains('/'));
    assert!(edit.staged.is_none() && edit.rename_armed.is_none() && edit.termination.is_none());
    assert_eq!(fs::read_dir(f.home.join("workspace")).unwrap().count(), 1);
    f.reopen();
    assert_eq!(f.admit(k), admitted);
    f.registry.record_start(k, receipt.clone()).unwrap();
    assert_eq!(f.registry.claim(k).unwrap(), admitted);
    let mut wrong = receipt.clone();
    wrong.identity.push('x');
    assert!(matches!(
        f.registry.record_start(k, wrong),
        Err(RegistryError::EvidenceConflict)
    ));
    f.registry
        .resolve_edit(
            k,
            evidence(&admitted, EffectSummary::NoMutation),
            EditTermination::JoinedWithoutRename,
        )
        .unwrap();
    let settled = f.registry.claim(k).unwrap();
    assert_eq!(
        f.admit(k),
        settled,
        "duplicate admission observes even terminal evidence"
    );
    let next = f.admit(key());
    assert_ne!(next.start, admitted.start);
    assert_ne!(
        next.edit.unwrap().manifest.stage_slot,
        edit.manifest.stage_slot
    );
}

#[test]
fn pre_arm_abort_preserves_exact_receipt_and_does_not_advance_revision() {
    for stage in [false, true] {
        let mut f = Fixture::new();
        let k = key();
        let claim = f.admit(k);
        if stage {
            f.registry
                .record_edit_staged(k, claim.start.as_ref().unwrap(), staged())
                .unwrap();
        }
        let before = f.registry.claim(k).unwrap();
        let terminal = evidence(&claim, EffectSummary::NoMutation);
        f.registry
            .resolve_edit(k, terminal.clone(), EditTermination::JoinedWithoutRename)
            .unwrap();
        f.reopen();
        f.registry
            .resolve_edit(k, terminal.clone(), EditTermination::JoinedWithoutRename)
            .unwrap();
        let after = f.registry.claim(k).unwrap();
        assert_eq!(after.start, before.start);
        assert_eq!(
            after.edit.as_ref().unwrap().staged,
            before.edit.as_ref().unwrap().staged
        );
        assert_eq!(after.terminal, Some(terminal));
        assert_eq!(f.registry.revision(&f.binding).unwrap(), claim.base);
        assert!(f.registry.unresolved(None, 10).unwrap().is_empty());
        assert!(matches!(
            f.registry
                .record_edit_rename_armed(k, claim.start.as_ref().unwrap(), armed()),
            Err(RegistryError::EvidenceConflict)
        ));
        if !stage {
            assert!(matches!(
                f.registry
                    .record_edit_staged(k, claim.start.as_ref().unwrap(), staged()),
                Err(RegistryError::EvidenceConflict)
            ));
        }
    }
}

#[test]
fn success_requires_arm_exact_effect_and_both_post_rename_barriers() {
    let mut f = Fixture::new();
    let k = key();
    let claim = f.admit(k);
    let receipt = claim.start.as_ref().unwrap();
    for stage in [false, true] {
        if stage {
            f.registry.record_edit_staged(k, receipt, staged()).unwrap();
        }
        let before = f.registry.claim(k).unwrap();
        assert!(matches!(
            f.registry.resolve_edit(k, success(&claim), replaced()),
            Err(RegistryError::EvidenceConflict)
        ));
        assert!(matches!(
            f.registry.resolve_edit(
                k,
                evidence(&claim, EffectSummary::MayHaveMutated),
                EditTermination::StoppedUnknownEffect
            ),
            Err(RegistryError::EvidenceConflict)
        ));
        // The generic API cannot bypass phase or proof checks, even for NoMutation.
        for effect in [
            EffectSummary::NoMutation,
            success(&claim).effect,
            EffectSummary::MayHaveMutated,
        ] {
            assert!(matches!(
                f.registry.resolve(k, evidence(&claim, effect)),
                Err(RegistryError::EvidenceConflict)
            ));
        }
        f.unchanged(k, &before, claim.base);
    }
    f.registry
        .record_edit_rename_armed(k, receipt, armed())
        .unwrap();
    let before = f.registry.claim(k).unwrap();
    for (destination_parent_synced, staging_parent_synced) in
        [(false, false), (false, true), (true, false)]
    {
        assert!(matches!(
            f.registry.resolve_edit(
                k,
                success(&claim),
                EditTermination::Replaced {
                    destination_parent_synced,
                    staging_parent_synced
                }
            ),
            Err(RegistryError::EvidenceConflict)
        ));
    }
    for effect in [
        EffectSummary::NoMutation,
        EffectSummary::MayHaveMutated,
        EffectSummary::KnownChanges { paths: vec![] },
        EffectSummary::KnownChanges {
            paths: vec!["other".into()],
        },
        EffectSummary::KnownChanges {
            paths: vec![action().target.clone(), action().target],
        },
        EffectSummary::Receipt {
            kind: "bypass".into(),
            data: serde_json::Value::Null,
        },
    ] {
        assert!(matches!(
            f.registry
                .resolve_edit(k, evidence(&claim, effect), replaced()),
            Err(RegistryError::EvidenceConflict)
        ));
    }
    f.unchanged(k, &before, claim.base);
    f.registry
        .resolve_edit(k, success(&claim), replaced())
        .unwrap();
    f.reopen();
    f.registry
        .resolve_edit(k, success(&claim), replaced())
        .unwrap();
    assert_eq!(
        f.registry.revision(&f.binding).unwrap(),
        WorkspaceRevision {
            files: 1,
            repository: 1
        }
    );
    assert_eq!(f.registry.claim(k).unwrap().start, claim.start);
    f.registry.record_edit_staged(k, receipt, staged()).unwrap();
    f.registry
        .record_edit_rename_armed(k, receipt, armed())
        .unwrap();
    assert!(matches!(
        f.registry.resolve_edit(
            k,
            evidence(&claim, EffectSummary::NoMutation),
            EditTermination::JoinedWithoutRename
        ),
        Err(RegistryError::EvidenceConflict)
    ));
}

#[test]
fn phase_facts_are_append_only_and_receipt_bound() {
    let mut f = Fixture::new();
    let k = key();
    let claim = f.admit(k);
    let receipt = claim.start.as_ref().unwrap();
    assert!(matches!(
        f.registry.record_edit_rename_armed(k, receipt, armed()),
        Err(RegistryError::EvidenceConflict)
    ));
    for wrong in [
        RegistryReceipt {
            backend: "wrong".into(),
            ..receipt.clone()
        },
        RegistryReceipt {
            identity: "wrong".into(),
            ..receipt.clone()
        },
    ] {
        assert!(matches!(
            f.registry.record_edit_staged(k, &wrong, staged()),
            Err(RegistryError::EvidenceConflict)
        ));
        assert!(matches!(
            f.registry.record_edit_rename_armed(k, &wrong, armed()),
            Err(RegistryError::EvidenceConflict)
        ));
        assert!(matches!(
            f.registry.resolve_edit(
                k,
                TerminalEvidence {
                    receipt: wrong,
                    effect: EffectSummary::NoMutation
                },
                EditTermination::JoinedWithoutRename
            ),
            Err(RegistryError::EvidenceConflict)
        ));
    }
    for bad in [
        EditStaged {
            content: action().expected,
            ..staged()
        },
        EditStaged {
            content: EditContent {
                bytes: u64::MAX,
                ..action().replacement
            },
            ..staged()
        },
        EditStaged {
            file_synced: false,
            ..staged()
        },
        EditStaged {
            parent_synced: false,
            ..staged()
        },
        EditStaged {
            parent: staged().file,
            ..staged()
        },
        EditStaged {
            parent: EditPhysicalIdentity {
                device: 8,
                ..identity(2)
            },
            ..staged()
        },
    ] {
        assert!(matches!(
            f.registry.record_edit_staged(k, receipt, bad),
            Err(RegistryError::EvidenceConflict)
        ));
    }
    f.unchanged(k, &claim, claim.base);
    f.registry.record_edit_staged(k, receipt, staged()).unwrap();
    f.registry.record_edit_staged(k, receipt, staged()).unwrap();
    assert!(matches!(
        f.registry.record_edit_staged(
            k,
            receipt,
            EditStaged {
                file: identity(99),
                ..staged()
            }
        ),
        Err(RegistryError::EvidenceConflict)
    ));
    for bad in [
        EditRenameArmed {
            staged_file: identity(99),
            ..armed()
        },
        EditRenameArmed {
            target_file: staged().file,
            ..armed()
        },
        EditRenameArmed {
            target_file: staged().parent,
            ..armed()
        },
        EditRenameArmed {
            target_file: armed().target_parent,
            ..armed()
        },
        EditRenameArmed {
            target_parent: staged().file,
            ..armed()
        },
        EditRenameArmed {
            target_parent: staged().parent,
            ..armed()
        },
        EditRenameArmed {
            target_parent: EditPhysicalIdentity {
                device: 8,
                ..identity(4)
            },
            ..armed()
        },
    ] {
        assert!(matches!(
            f.registry.record_edit_rename_armed(k, receipt, bad),
            Err(RegistryError::EvidenceConflict)
        ));
    }
    assert!(matches!(
        f.registry.record_edit_rename_armed(
            k,
            receipt,
            EditRenameArmed {
                target_file: EditPhysicalIdentity {
                    birth_nanos: 1_000_000_000,
                    ..identity(3)
                },
                ..armed()
            }
        ),
        Err(RegistryError::Invalid)
    ));
    f.registry
        .record_edit_rename_armed(k, receipt, armed())
        .unwrap();
    let before = f.registry.claim(k).unwrap();
    f.registry
        .record_edit_rename_armed(k, receipt, armed())
        .unwrap();
    assert!(matches!(
        f.registry.record_edit_rename_armed(
            k,
            receipt,
            EditRenameArmed {
                target_file: identity(99),
                ..armed()
            }
        ),
        Err(RegistryError::EvidenceConflict)
    ));
    f.unchanged(k, &before, claim.base);
}

#[test]
fn armed_claim_needs_explicit_joined_proof_not_inference_from_missing_target() {
    for no_rename in [false, true] {
        let mut f = Fixture::new();
        let k = key();
        let claim = f.admit(k);
        f.arm(&claim);
        fs::remove_dir_all(f.home.join("workspace")).unwrap();
        f.reopen();
        assert!(f.registry.claim(k).unwrap().terminal.is_none());
        // Duplicate arm remains observation even when the workspace is gone.
        f.registry
            .record_edit_rename_armed(k, claim.start.as_ref().unwrap(), armed())
            .unwrap();
        let (effect, proof, advance) = if no_rename {
            (
                EffectSummary::NoMutation,
                EditTermination::JoinedWithoutRename,
                0,
            )
        } else {
            (
                EffectSummary::MayHaveMutated,
                EditTermination::StoppedUnknownEffect,
                1,
            )
        };
        f.registry
            .resolve_edit(k, evidence(&claim, effect), proof)
            .unwrap();
        assert_eq!(
            f.registry.revision(&f.binding).unwrap(),
            WorkspaceRevision {
                files: advance,
                repository: advance
            }
        );
    }
}

#[test]
fn conflicting_keys_stale_revisions_and_generic_claims_keep_their_contracts() {
    let mut f = Fixture::new();
    let k = key();
    let claim = f.admit(k);
    for changed in [
        EditAction {
            action_digest: ContentDigest::of_bytes(b"different"),
            ..action()
        },
        EditAction {
            tool_binding: ContentDigest::of_bytes(b"different"),
            ..action()
        },
        EditAction {
            target: "elsewhere".into(),
            ..action()
        },
        EditAction {
            replacement: content(b"different"),
            ..action()
        },
    ] {
        assert!(matches!(
            f.registry
                .admit_edit(&f.binding, k, claim.resources, claim.base, changed),
            Err(RegistryError::EvidenceConflict)
        ));
    }
    assert!(matches!(
        f.registry.admit_edit(
            &f.binding,
            k,
            WorkspaceResources::Files,
            claim.base,
            action()
        ),
        Err(RegistryError::EvidenceConflict)
    ));
    assert!(matches!(
        f.registry.admit_edit(
            &f.binding,
            k,
            claim.resources,
            WorkspaceRevision {
                files: 1,
                repository: 0
            },
            action()
        ),
        Err(RegistryError::EvidenceConflict)
    ));
    assert!(matches!(
        f.registry
            .admit_edit(&f.binding, key(), claim.resources, claim.base, action()),
        Err(RegistryError::Conflict)
    ));
    assert!(matches!(
        f.registry
            .admit(&f.binding, key(), claim.resources, claim.base),
        Err(RegistryError::Conflict)
    ));
    f.arm(&claim);
    f.registry
        .resolve_edit(k, success(&claim), replaced())
        .unwrap();
    assert!(matches!(
        f.registry
            .admit_edit(&f.binding, key(), claim.resources, claim.base, action()),
        Err(RegistryError::StaleRevision)
    ));
    let generic_key = key();
    let current = f.registry.revision(&f.binding).unwrap();
    let generic = f
        .registry
        .admit(&f.binding, generic_key, claim.resources, current)
        .unwrap();
    assert!(generic.edit.is_none());
    assert!(matches!(
        f.registry
            .admit_edit(&f.binding, generic_key, claim.resources, current, action()),
        Err(RegistryError::EvidenceConflict)
    ));
    let terminal = TerminalEvidence {
        receipt: RegistryReceipt {
            backend: f.binding.backend.clone(),
            identity: "generic".into(),
        },
        effect: EffectSummary::NoMutation,
    };
    assert!(matches!(
        f.registry.resolve_edit(
            generic_key,
            terminal.clone(),
            EditTermination::JoinedWithoutRename
        ),
        Err(RegistryError::EvidenceConflict)
    ));
    f.registry.resolve(generic_key, terminal).unwrap();
}

#[test]
fn arm_revalidates_binding_and_revision_without_erasing_staged_fact() {
    for changed_binding in [false, true] {
        let mut f = Fixture::new();
        let k = key();
        let claim = f.admit(k);
        f.registry
            .record_edit_staged(k, claim.start.as_ref().unwrap(), staged())
            .unwrap();
        let before = f.registry.claim(k).unwrap();
        if changed_binding {
            fs::rename(f.home.join("workspace"), f.home.join("old-workspace")).unwrap();
            fs::create_dir(f.home.join("workspace")).unwrap();
            assert!(matches!(
                f.registry
                    .record_edit_rename_armed(k, claim.start.as_ref().unwrap(), armed()),
                Err(RegistryError::BindingChanged)
            ));
        } else {
            f.registry
                .connection
                .execute_batch("UPDATE bindings SET revision=1;")
                .unwrap();
            assert!(matches!(
                f.registry
                    .record_edit_rename_armed(k, claim.start.as_ref().unwrap(), armed()),
                Err(RegistryError::StaleRevision)
            ));
        }
        assert_eq!(f.registry.claim(k).unwrap(), before);
    }
}

#[test]
fn manifest_bounds_and_terminal_capacity_are_checked_before_admission() {
    let mut f = Fixture::new();
    let base = f.registry.revision(&f.binding).unwrap();
    for target in [
        "",
        "/absolute",
        "a//b",
        "a/../b",
        "a/./b",
        ".GiT/config",
        "a\0b",
    ] {
        let bad = EditAction {
            target: target.into(),
            ..action()
        };
        assert!(matches!(
            f.registry
                .admit_edit(&f.binding, key(), WorkspaceResources::Files, base, bad),
            Err(RegistryError::Invalid)
        ));
    }
    for bytes in [MAX_EDIT_BYTES + 1, u64::MAX] {
        for expected in [false, true] {
            let mut bad = action();
            if expected {
                bad.expected.bytes = bytes;
            } else {
                bad.replacement.bytes = bytes;
            }
            assert!(matches!(
                f.registry
                    .admit_edit(&f.binding, key(), WorkspaceResources::Files, base, bad),
                Err(RegistryError::Invalid)
            ));
        }
    }
    let bad = EditAction {
        target: "x".repeat(MAX_EDIT_TARGET_BYTES + 1),
        ..action()
    };
    assert!(matches!(
        f.registry
            .admit_edit(&f.binding, key(), WorkspaceResources::Files, base, bad),
        Err(RegistryError::Invalid)
    ));
    // Fits raw path bounds, but escaped path copies cannot fit a terminal record.
    let too_large = EditAction {
        target: "\\".repeat(MAX_EDIT_TARGET_BYTES),
        ..action()
    };
    assert!(matches!(
        f.registry.admit_edit(
            &f.binding,
            key(),
            WorkspaceResources::Files,
            base,
            too_large
        ),
        Err(RegistryError::Capacity)
    ));
    assert!(f.registry.unresolved(None, 10).unwrap().is_empty());
    // Maximum ordinary path and content lengths still have settlement space.
    let mut maximum = action();
    maximum.target = "x".repeat(MAX_EDIT_TARGET_BYTES);
    maximum.expected.bytes = MAX_EDIT_BYTES;
    maximum.replacement.bytes = MAX_EDIT_BYTES;
    let k = key();
    let claim = f
        .registry
        .admit_edit(
            &f.binding,
            k,
            WorkspaceResources::Files,
            base,
            maximum.clone(),
        )
        .unwrap();
    let large_identity = |inode| EditPhysicalIdentity {
        inode,
        ..EditPhysicalIdentity::MAX
    };
    let stage = EditStaged {
        content: maximum.replacement,
        file: large_identity(u64::MAX),
        parent: large_identity(u64::MAX - 1),
        ..staged()
    };
    let arm = EditRenameArmed {
        staged_file: stage.file,
        target_file: large_identity(u64::MAX - 2),
        target_parent: large_identity(u64::MAX - 3),
    };
    f.registry
        .record_edit_staged(k, claim.start.as_ref().unwrap(), stage)
        .unwrap();
    f.registry
        .record_edit_rename_armed(k, claim.start.as_ref().unwrap(), arm)
        .unwrap();
    f.registry
        .resolve_edit(k, success(&claim), replaced())
        .unwrap();
    assert!(encode(&f.registry.claim(k).unwrap()).unwrap().len() <= MAX_RECORD);
}

#[test]
fn redirected_repository_metadata_is_not_an_ordinary_edit_target() {
    let mut f = Fixture::new();
    let root = f.home.join("redirected");
    fs::create_dir_all(root.join("admin")).unwrap();
    fs::create_dir(root.join("common")).unwrap();
    fs::write(root.join(".git"), "gitdir: admin\n").unwrap();
    fs::write(root.join("admin/commondir"), "../common\n").unwrap();
    let binding = f
        .registry
        .bind("redirected", &root, "host-edit-v1")
        .unwrap();
    let base = f.registry.revision(&binding).unwrap();
    for target in ["admin/config", "common/HEAD"] {
        assert!(matches!(
            f.registry.admit_edit(
                &binding,
                key(),
                WorkspaceResources::FilesAndRepository,
                base,
                EditAction {
                    target: target.into(),
                    ..action()
                }
            ),
            Err(RegistryError::Invalid)
        ));
    }
    assert!(f.registry.unresolved(None, 10).unwrap().is_empty());
}

#[test]
fn exact_duplicates_do_not_attempt_a_second_write() {
    let mut f = Fixture::new();
    let k = key();
    let claim = f.admit(k);
    f.arm(&claim);
    f.registry
        .resolve_edit(k, success(&claim), replaced())
        .unwrap();
    let terminal = f.registry.claim(k).unwrap();
    f.registry.connection.execute_batch("CREATE TRIGGER forbid_insert BEFORE INSERT ON claims BEGIN SELECT RAISE(ABORT, 'duplicate insert'); END;
        CREATE TRIGGER forbid_update BEFORE UPDATE ON claims BEGIN SELECT RAISE(ABORT, 'duplicate update'); END;
        CREATE TRIGGER forbid_revision BEFORE UPDATE ON bindings BEGIN SELECT RAISE(ABORT, 'duplicate revision'); END;").unwrap();
    // Reuse the original base: this is observation, not a fresh admission.
    assert_eq!(
        f.registry
            .admit_edit(&f.binding, k, claim.resources, claim.base, action())
            .unwrap(),
        terminal
    );
    f.registry
        .record_start(k, claim.start.clone().unwrap())
        .unwrap();
    for phase in 1..=3 {
        run_phase(&mut f, k, Some(&claim), phase).unwrap();
    }
    assert_eq!(f.registry.claim(k).unwrap(), terminal);
    assert_eq!(
        f.registry.revision(&f.binding).unwrap(),
        WorkspaceRevision {
            files: 1,
            repository: 1
        }
    );
}

#[test]
fn old_registry_version_is_refused_without_migration() {
    let mut f = Fixture::new();
    f.registry
        .connection
        .execute_batch("PRAGMA user_version=3;")
        .unwrap();
    assert!(matches!(
        WorkspaceRegistry::open(f.home.join("host")),
        Err(RegistryError::Unsupported)
    ));
    let version: i64 = f
        .registry
        .connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 3);
    // Restore only the test fixture, not through a production migration path.
    f.registry
        .connection
        .execute_batch("PRAGMA user_version=4;")
        .unwrap();
    f.reopen();
}

#[test]
fn revision_overflow_rolls_back_terminal_proof_release_and_every_revision() {
    for table in ["bindings", "repositories"] {
        let mut f = Fixture::new();
        let alias = f
            .registry
            .bind("alias", &f.binding.canonical_root, &f.binding.backend)
            .unwrap();
        let k = key();
        let claim = f.admit(k);
        f.arm(&claim);
        f.registry
            .connection
            .execute_batch(&format!("UPDATE {table} SET revision=9223372036854775807;"))
            .unwrap();
        let before = f.registry.claim(k).unwrap();
        let revision = f.registry.revision(&f.binding).unwrap();
        assert!(matches!(
            f.registry.resolve_edit(k, success(&claim), replaced()),
            Err(RegistryError::Capacity)
        ));
        f.reopen();
        f.unchanged(k, &before, revision);
        assert_eq!(f.registry.revision(&alias).unwrap(), revision);
        f.registry
            .connection
            .execute_batch(&format!("UPDATE {table} SET revision=0;"))
            .unwrap();
        f.registry
            .resolve_edit(k, success(&claim), replaced())
            .unwrap();
        f.registry
            .resolve_edit(k, success(&claim), replaced())
            .unwrap();
        assert_eq!(
            f.registry.revision(&f.binding).unwrap(),
            WorkspaceRevision {
                files: 1,
                repository: 1
            }
        );
        assert_eq!(
            f.registry.revision(&alias).unwrap(),
            WorkspaceRevision {
                files: 1,
                repository: 1
            }
        );
    }
}

#[test]
fn statement_and_commit_faults_roll_back_each_edit_phase() {
    for commit_failure in [false, true] {
        for phase in 0..4 {
            let mut f = Fixture::new();
            let k = key();
            let claim = if phase == 0 { None } else { Some(f.admit(k)) };
            if phase >= 2 {
                f.registry
                    .record_edit_staged(
                        k,
                        claim.as_ref().unwrap().start.as_ref().unwrap(),
                        staged(),
                    )
                    .unwrap();
            }
            if phase >= 3 {
                f.registry
                    .record_edit_rename_armed(
                        k,
                        claim.as_ref().unwrap().start.as_ref().unwrap(),
                        armed(),
                    )
                    .unwrap();
            }
            let before = find_claim(&f.registry.connection, k).unwrap();
            let revision = f.registry.revision(&f.binding).unwrap();
            let operation = if phase == 0 { "INSERT" } else { "UPDATE" };
            let body = if commit_failure {
                // Statement succeeds; deferred FK verification fails at COMMIT.
                f.registry.connection.execute_batch("CREATE TABLE fault_parent(id INTEGER PRIMARY KEY); CREATE TABLE fault_child(id INTEGER REFERENCES fault_parent(id) DEFERRABLE INITIALLY DEFERRED);").unwrap();
                "INSERT INTO fault_child VALUES(1);"
            } else {
                "SELECT RAISE(ABORT, 'injected statement failure');"
            };
            f.registry
                .connection
                .execute_batch(&format!(
                    "CREATE TRIGGER fault AFTER {operation} ON claims BEGIN {body} END;"
                ))
                .unwrap();
            let result = run_phase(&mut f, k, claim.as_ref(), phase);
            assert!(
                matches!(result, Err(RegistryError::Sql(_))),
                "phase {phase}: {result:?}"
            );
            f.reopen();
            assert_eq!(find_claim(&f.registry.connection, k).unwrap(), before);
            assert_eq!(f.registry.revision(&f.binding).unwrap(), revision);
            f.registry
                .connection
                .execute_batch("DROP TRIGGER fault;")
                .unwrap();
            run_phase(&mut f, k, claim.as_ref(), phase).unwrap();
        }
    }
}

fn run_phase(
    f: &mut Fixture,
    k: ClaimKey,
    claim: Option<&WorkspaceClaim>,
    phase: u8,
) -> Result<()> {
    if phase == 0 {
        return f
            .registry
            .admit_edit(
                &f.binding,
                k,
                WorkspaceResources::FilesAndRepository,
                f.registry.revision(&f.binding)?,
                action(),
            )
            .map(|_| ());
    }
    let claim = claim.unwrap();
    let receipt = claim.start.as_ref().unwrap();
    match phase {
        1 => f.registry.record_edit_staged(k, receipt, staged()),
        2 => f.registry.record_edit_rename_armed(k, receipt, armed()),
        3 => f.registry.resolve_edit(k, success(claim), replaced()),
        _ => unreachable!(),
    }
}

#[test]
fn lost_write_acknowledgements_require_durable_inspection_not_noop_inference() {
    let mut f = Fixture::new();
    let k = key();
    let base = f.registry.revision(&f.binding).unwrap();
    f.registry.lose_next_commit_ack = true;
    assert!(matches!(
        run_phase(&mut f, k, None, 0),
        Err(RegistryError::Io(_))
    ));
    f.reopen();
    let claim = f.registry.claim(k).unwrap();
    assert_eq!(f.admit(k), claim);
    for phase in 1..=3 {
        let before = f.registry.claim(k).unwrap();
        f.registry.lose_next_commit_ack = true;
        assert!(matches!(
            run_phase(&mut f, k, Some(&claim), phase),
            Err(RegistryError::Io(_))
        ));
        f.reopen();
        let after = f.registry.claim(k).unwrap();
        assert_ne!(before, after);
        assert_eq!(after.start, claim.start);
        assert_eq!(
            after.edit.as_ref().unwrap().manifest,
            claim.edit.as_ref().unwrap().manifest
        );
        run_phase(&mut f, k, Some(&claim), phase).unwrap();
        assert_eq!(f.registry.claim(k).unwrap(), after);
        if phase < 3 {
            assert_eq!(f.registry.revision(&f.binding).unwrap(), base);
        }
    }
    assert_eq!(
        f.registry.revision(&f.binding).unwrap(),
        WorkspaceRevision {
            files: 1,
            repository: 1
        }
    );
    assert!(f.registry.unresolved(None, 10).unwrap().is_empty());
}
