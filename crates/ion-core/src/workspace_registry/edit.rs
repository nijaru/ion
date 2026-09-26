//! Durable edit evidence, not an execution backend. All attestations come from
//! trusted host code holding exclusive custody of the worker and private,
//! same-filesystem staging. Terminal attestations require proven worker quiescence.
//! No method here performs or authorizes filesystem I/O.

use super::*;

pub const MAX_EDIT_BYTES: u64 = 16 * 1024;
pub const MAX_EDIT_TARGET_BYTES: usize = 4096;
/// Registry-wide outstanding reservations, including terminal cleanup failures.
pub const MAX_EDIT_ALLOCATIONS: usize = 64;

/// Immutable vacancy certificate for the enclosing claim's exact receipt and slot.
/// Trusted host attests a no-follow absent-name check and parent durability barrier
/// under permanent worker/staging custody BEFORE exclusive creation. Provenance
/// remains valid only while the host continuously protects the namespace (0700 is
/// not confinement against same-user writers). This is NOT rename eligibility.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageAllocation {
    pub registry_incarnation: String,
    pub parent: EditPhysicalIdentity,
    /// Reserve the full per-stage bound, including interrupted writes.
    pub reserved_bytes: u64,
}

/// Independent of transcript/effect settlement; disposal never clears quarantine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StageDisposal {
    Reserved,
    /// Exclusive create observed a collision after certified vacancy: custody was
    /// violated. Never adopt/unlink this occupant or automatically retire quota.
    Blocked,
    /// Durable terminal proof has authorized cleanup; quota remains reserved.
    Authorized,
    /// Host attests unlink/absence AND a successful parent durability barrier.
    Disposed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditContent {
    pub digest: ContentDigest,
    pub bytes: u64,
}

/// Bounded, self-contained intent. Workspace/executor binding, resource scope and
/// expected revision live in the enclosing WorkspaceClaim, not in Session storage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditAction {
    pub kind: EditKind,
    pub action_digest: ContentDigest,
    /// `ContentDigest::of` the exact frozen ToolBinding, not just its display name.
    pub tool_binding: ContentDigest,
    pub target: String,
    pub expected: EditContent,
    pub replacement: EditContent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EditKind {
    Replace,
    Create,
}

impl EditAction {
    pub(super) fn validate(&self) -> Result<()> {
        if self.target.is_empty()
            || self.target.len() > MAX_EDIT_TARGET_BYTES
            || self.target.contains('\0')
            || self.target.split('/').any(|part| {
                part.is_empty() || part == "." || part == ".." || part.eq_ignore_ascii_case(".git")
            })
            || self.expected.bytes > MAX_EDIT_BYTES
            || self.replacement.bytes > MAX_EDIT_BYTES
            || (self.kind == EditKind::Replace && self.expected == self.replacement)
            || (self.kind == EditKind::Create
                && self.expected
                    != EditContent {
                        digest: ContentDigest::of_bytes(b""),
                        bytes: 0,
                    })
        {
            return Err(RegistryError::Invalid);
        }
        Ok(())
    }

    pub(super) fn validate_target(&self, descriptor: &Descriptor) -> Result<()> {
        let target = descriptor.root.path.join(&self.target);
        // The lexical guard alone misses APFS case aliases such as ADMIN/config
        // for a redirected .git directory named admin. Authentication at claim
        // admission uses the actual existing object too. A missing target still
        // needs its ordinary backend's no-follow base check before effect.
        let resolved = match fs::canonicalize(&target) {
            Ok(resolved) => Some(resolved),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidFilename
                ) =>
            {
                None
            }
            Err(error) => return Err(error.into()),
        };
        if resolved
            .as_ref()
            .is_some_and(|path| !path.starts_with(&descriptor.root.path))
            || descriptor
                .git
                .iter()
                .chain(descriptor.common.iter())
                .any(|admin| {
                    target.starts_with(&admin.path)
                        || resolved
                            .as_ref()
                            .is_some_and(|path| path.starts_with(&admin.path))
                })
        {
            return Err(RegistryError::Invalid);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditManifest {
    pub action: EditAction,
    /// Registry-minted opaque leaf, never a workspace-relative or absolute path.
    /// The future host backend must map it into its protected staging namespace.
    /// It must never fall back to creating a temporary file in the workspace.
    pub stage_slot: String,
}

/// Creation time is required: device/inode alone does not distinguish inode reuse.
/// The host must qualify these facts on its filesystem; integers are not custody.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditPhysicalIdentity {
    pub device: u64,
    pub inode: u64,
    pub birth_seconds: u64,
    pub birth_nanos: u32,
}

impl EditPhysicalIdentity {
    fn validate(self) -> Result<()> {
        if self.birth_nanos >= 1_000_000_000 {
            return Err(RegistryError::Invalid);
        }
        Ok(())
    }

    // Largest encoded identity for settlement-capacity reservation.
    const MAX: Self = Self {
        device: u64::MAX,
        inode: u64::MAX,
        birth_seconds: u64::MAX,
        birth_nanos: 999_999_999,
    };
}

/// Host attests exact replacement bytes, physical identity and completed staging
/// barriers under private custody. Partial/failed staging is not a Staged fact.
/// File and parent identities must be distinct; rename targets must have a
/// different parent from this host-private staging directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditStaged {
    pub file: EditPhysicalIdentity,
    pub parent: EditPhysicalIdentity,
    pub content: EditContent,
    pub file_synced: bool,
    pub parent_synced: bool,
}

/// Persist before the first rename invocation. The host attests it rechecked the
/// exact expected base and pinned target identities, custody and live authority.
/// This is eligibility evidence, NOT evidence that rename happened or a replay
/// permit. Only the original, exclusively owned worker may invoke rename once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditRenameArmed {
    pub staged_file: EditPhysicalIdentity,
    /// Existing base for replacement; absent base for creation.
    pub target_file: Option<EditPhysicalIdentity>,
    pub target_parent: EditPhysicalIdentity,
}

/// Terminal host attestations are deliberately separate from effect summaries.
/// Neither an absent replacement nor an error reply supplies any of these facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EditTermination {
    /// Owning worker is joined, never invoked rename, and every staging effect was
    /// host-internal. This explicit proof can settle even an armed-but-unused edit;
    /// recovery must never infer it from missing files or an unchanged target.
    JoinedWithoutRename,
    /// Host verified the exact staged object and replacement bytes at the target.
    /// Both cross-directory barriers must complete before recording KnownChanges.
    Replaced {
        destination_parent_synced: bool,
        staging_parent_synced: bool,
    },
    /// Host proved termination after arm, but cannot enumerate the effect exactly.
    /// No possibly-live worker may be settled using this attestation.
    StoppedUnknownEffect,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditClaim {
    pub manifest: EditManifest,
    pub allocation: Option<StageAllocation>,
    pub disposal: Option<StageDisposal>,
    pub staged: Option<EditStaged>,
    pub rename_armed: Option<EditRenameArmed>,
    /// Proof supporting WorkspaceClaim::terminal, written atomically with it.
    pub termination: Option<EditTermination>,
}

impl WorkspaceRegistry {
    /// Atomically mint the immutable start receipt, stage slot and manifest with
    /// resource quarantine. Exact duplicate admission returns the retained claim,
    /// including later evidence; it is observation, never permission to execute.
    /// A conflicting reuse of the key is rejected. No staging file is created.
    /// On an ambiguous write error inspect `claim(key)` before proceeding.
    pub fn admit_edit(
        &mut self,
        binding: &WorkspaceBinding,
        key: ClaimKey,
        resources: WorkspaceResources,
        expected: WorkspaceRevision,
        action: EditAction,
    ) -> Result<WorkspaceClaim> {
        self.admit_claim(binding, key, resources, expected, Some(action))
    }

    /// Persist the host's vacancy certificate before O_EXCL creation. An uncertain
    /// reply MUST be authoritatively read back before creation. Duplicate facts
    /// are observations, never permission for another create or replay.
    pub fn allocate_edit_stage(
        &mut self,
        key: ClaimKey,
        receipt: &RegistryReceipt,
        allocation: StageAllocation,
    ) -> Result<()> {
        self.record_edit_fact(key, receipt, EditFact::Allocation(allocation))
    }

    /// At most 64 outstanding allocations, discoverable without Session or workspace
    /// access, including crashed workers and terminal cleanup-pending claims.
    pub fn outstanding_edit_allocations(&self) -> Result<Vec<WorkspaceClaim>> {
        let mut stmt = self.connection.prepare(
            "SELECT claims.record FROM edit_allocations JOIN claims USING(key) ORDER BY key LIMIT ?1",
        )?;
        let rows = stmt.query_map([MAX_EDIT_ALLOCATIONS as i64 + 1], |r| r.get::<_, String>(0))?;
        let claims: Vec<WorkspaceClaim> = rows.map(|r| decode(&r?)).collect::<Result<_>>()?;
        if claims.len() > MAX_EDIT_ALLOCATIONS {
            return Err(RegistryError::Capacity);
        }
        Ok(claims)
    }

    /// Record disposal authority before unlink, or completed durable disposal.
    /// `Blocked` records a witnessed post-allocation custody violation and is
    /// irreversible. It never grants disposal authority, even after settlement.
    /// These trusted host attestations cannot settle effects or authorize cleanup
    /// of an unresolved armed attempt. A failed reply requires readback.
    pub fn record_edit_disposal(
        &mut self,
        key: ClaimKey,
        receipt: &RegistryReceipt,
        disposition: StageDisposal,
    ) -> Result<()> {
        self.record_edit_fact(key, receipt, EditFact::Disposal(disposition))
    }

    /// Append one immutable Staged fact. Exact duplicates remain observable after
    /// arm/settlement; mismatches and new facts after settlement are rejected.
    pub fn record_edit_staged(
        &mut self,
        key: ClaimKey,
        receipt: &RegistryReceipt,
        staged: EditStaged,
    ) -> Result<()> {
        self.record_edit_fact(key, receipt, EditFact::Staged(staged))
    }

    /// Append RenameArmed after Staged. Rechecks registry binding/revision, but
    /// cannot enforce host custody, authority, or filesystem compare-and-swap.
    /// A duplicate or recovered fact must never trigger a second rename.
    pub fn record_edit_rename_armed(
        &mut self,
        key: ClaimKey,
        receipt: &RegistryReceipt,
        armed: EditRenameArmed,
    ) -> Result<()> {
        self.record_edit_fact(key, receipt, EditFact::RenameArmed(armed))
    }

    fn record_edit_fact(
        &mut self,
        key: ClaimKey,
        receipt: &RegistryReceipt,
        fact: EditFact,
    ) -> Result<()> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut claim = load_claim(&tx, key)?;
        if claim.start.as_ref() != Some(receipt) {
            return Err(RegistryError::EvidenceConflict);
        }
        let edit = claim.edit.as_mut().ok_or(RegistryError::EvidenceConflict)?;
        let terminal = claim.terminal.is_some();
        let arming = matches!(fact, EditFact::RenameArmed(_));
        let changed = match fact {
            EditFact::Allocation(allocation) => {
                allocation.parent.validate()?;
                if allocation.registry_incarnation != self.incarnation
                    || allocation.reserved_bytes != MAX_EDIT_BYTES
                {
                    return Err(RegistryError::EvidenceConflict);
                }
                let changed = append(&mut edit.allocation, allocation, terminal)?;
                if changed {
                    let count: i64 =
                        tx.query_row("SELECT count(*) FROM edit_allocations", [], |r| r.get(0))?;
                    if count >= MAX_EDIT_ALLOCATIONS as i64 {
                        return Err(RegistryError::Capacity);
                    }
                    tx.execute(
                        "INSERT INTO edit_allocations(key) VALUES(?1)",
                        [encode(&key)?],
                    )?;
                    edit.disposal = Some(StageDisposal::Reserved);
                }
                changed
            }
            EditFact::Disposal(disposition) => {
                if edit
                    .allocation
                    .as_ref()
                    .is_none_or(|a| a.registry_incarnation != self.incarnation)
                    || (disposition != StageDisposal::Blocked
                        && (!terminal
                            || !matches!(
                                edit.termination,
                                Some(
                                    EditTermination::JoinedWithoutRename
                                        | EditTermination::Replaced {
                                            destination_parent_synced: true,
                                            staging_parent_synced: true
                                        }
                                )
                            )))
                {
                    return Err(RegistryError::EvidenceConflict);
                }
                if edit.disposal == Some(disposition) {
                    false
                } else {
                    match (edit.disposal, disposition) {
                        (Some(StageDisposal::Reserved), StageDisposal::Blocked)
                            if edit.staged.is_none() && edit.rename_armed.is_none() => {}
                        (Some(StageDisposal::Reserved), StageDisposal::Authorized) => {}
                        (Some(StageDisposal::Authorized), StageDisposal::Disposed) => {
                            tx.execute(
                                "DELETE FROM edit_allocations WHERE key=?1",
                                [encode(&key)?],
                            )?;
                        }
                        _ => return Err(RegistryError::EvidenceConflict),
                    }
                    edit.disposal = Some(disposition);
                    true
                }
            }
            EditFact::Staged(staged) => {
                if edit.disposal == Some(StageDisposal::Blocked) {
                    return Err(RegistryError::EvidenceConflict);
                }
                staged.file.validate()?;
                staged.parent.validate()?;
                if edit
                    .allocation
                    .as_ref()
                    .is_none_or(|a| a.parent != staged.parent)
                    || staged.content != edit.manifest.action.replacement
                    || !staged.file_synced
                    || !staged.parent_synced
                    || staged.file.device != staged.parent.device
                    || staged.file == staged.parent
                {
                    return Err(RegistryError::EvidenceConflict);
                }
                append(&mut edit.staged, staged, terminal)?
            }
            EditFact::RenameArmed(armed) => {
                let staged = edit
                    .staged
                    .as_ref()
                    .ok_or(RegistryError::EvidenceConflict)?;
                if let Some(file) = armed.target_file {
                    file.validate()?;
                }
                armed.target_parent.validate()?;
                if armed.staged_file != staged.file
                    || armed.target_file.is_some()
                        != (edit.manifest.action.kind == EditKind::Replace)
                    || armed
                        .target_file
                        .is_some_and(|file| file.device != staged.file.device)
                    || armed.target_parent.device != staged.file.device
                    || armed.target_file == Some(staged.file)
                    || armed.target_file == Some(staged.parent)
                    || armed.target_file == Some(armed.target_parent)
                    || armed.target_parent == staged.file
                    || armed.target_parent == staged.parent
                {
                    return Err(RegistryError::EvidenceConflict);
                }
                append(&mut edit.rename_armed, armed, terminal)?
            }
        };
        if !changed {
            return Ok(());
        }
        if arming {
            let record = checked_binding(&tx, &claim.binding)?;
            outside(&self.directory, &record.descriptor)?;
            if describe(Path::new(&claim.binding.canonical_root))
                .ok()
                .as_ref()
                != Some(&record.descriptor)
            {
                return Err(RegistryError::BindingChanged);
            }
            if revision(&tx, &record)? != claim.base {
                return Err(RegistryError::StaleRevision);
            }
        }
        tx.execute(
            "UPDATE claims SET record=?2 WHERE key=?1",
            params![encode(&key)?, encode(&claim)?],
        )?;
        commit_claim_write(
            tx,
            #[cfg(test)]
            &mut self.lose_next_commit_ack,
        )
    }

    /// Resolve only with the exact minted receipt and phase-compatible terminal
    /// attestation/effect. Generic `resolve` cannot bypass this check. Proof,
    /// release and at most one revision advance commit together. No filesystem
    /// observation is inferred here; the host must authenticate the proof.
    pub fn resolve_edit(
        &mut self,
        key: ClaimKey,
        evidence: TerminalEvidence,
        termination: EditTermination,
    ) -> Result<()> {
        self.resolve_claim(key, evidence, Some(termination))
    }
}

enum EditFact {
    Allocation(StageAllocation),
    Disposal(StageDisposal),
    Staged(EditStaged),
    RenameArmed(EditRenameArmed),
}

fn append<T: PartialEq>(slot: &mut Option<T>, value: T, terminal: bool) -> Result<bool> {
    if slot.as_ref() == Some(&value) {
        return Ok(false);
    }
    if slot.is_some() || terminal {
        return Err(RegistryError::EvidenceConflict);
    }
    *slot = Some(value);
    Ok(true)
}

pub(super) fn initialize(
    claim: &mut WorkspaceClaim,
    action: EditAction,
    incarnation: &str,
) -> Result<()> {
    let identity = ContentDigest::of_bytes(
        encode(&(
            "registry-edit-attempt-v2",
            incarnation,
            claim.key,
            &claim.binding,
            claim.resources,
            claim.base,
            &action,
        ))?
        .as_bytes(),
    );
    claim.start = Some(RegistryReceipt {
        backend: claim.binding.backend.clone(),
        identity: format!(
            "{}:{identity}",
            if action.kind == EditKind::Create {
                "create-attempt-v1"
            } else {
                "edit-attempt-v2"
            }
        ),
    });
    claim.edit = Some(EditClaim {
        manifest: EditManifest {
            action,
            stage_slot: format!("edit-{identity}"),
        },
        allocation: None,
        disposal: None,
        staged: None,
        rename_armed: None,
        termination: None,
    });
    reserve_terminal_capacity(claim)
}

/// Reserve an encoded upper bound on a fully populated terminal record at
/// admission, so manifests cannot consume space needed for phases and settlement.
fn reserve_terminal_capacity(claim: &WorkspaceClaim) -> Result<()> {
    let mut largest = claim.clone();
    let edit = largest.edit.as_mut().ok_or(RegistryError::Invalid)?;
    edit.allocation = Some(StageAllocation {
        registry_incarnation: "f".repeat(64),
        parent: EditPhysicalIdentity::MAX,
        reserved_bytes: MAX_EDIT_BYTES,
    });
    edit.disposal = Some(StageDisposal::Authorized);
    edit.staged = Some(EditStaged {
        file: EditPhysicalIdentity::MAX,
        parent: EditPhysicalIdentity::MAX,
        content: edit.manifest.action.replacement,
        file_synced: true,
        parent_synced: true,
    });
    edit.rename_armed = Some(EditRenameArmed {
        staged_file: EditPhysicalIdentity::MAX,
        target_file: Some(EditPhysicalIdentity::MAX),
        target_parent: EditPhysicalIdentity::MAX,
    });
    edit.termination = Some(EditTermination::Replaced {
        destination_parent_synced: true,
        staging_parent_synced: true,
    });
    largest.terminal = Some(TerminalEvidence {
        receipt: largest.start.clone().ok_or(RegistryError::Invalid)?,
        effect: EffectSummary::KnownChanges {
            paths: vec![edit.manifest.action.target.clone()],
        },
    });
    encode(&largest)?;
    Ok(())
}

pub(super) fn apply_termination(
    claim: &mut WorkspaceClaim,
    evidence: &TerminalEvidence,
    termination: Option<EditTermination>,
) -> Result<()> {
    let Some(edit) = &mut claim.edit else {
        return if termination.is_none() {
            Ok(())
        } else {
            Err(RegistryError::EvidenceConflict)
        };
    };
    let termination = termination.ok_or(RegistryError::EvidenceConflict)?;
    if claim.start.as_ref() != Some(&evidence.receipt) {
        return Err(RegistryError::EvidenceConflict);
    }
    let valid = match (&termination, &evidence.effect) {
        (EditTermination::JoinedWithoutRename, EffectSummary::NoMutation) => true,
        (
            EditTermination::Replaced {
                destination_parent_synced: true,
                staging_parent_synced: true,
            },
            EffectSummary::KnownChanges { paths },
        ) => {
            edit.rename_armed.is_some()
                && edit.staged.is_some()
                && paths.len() == 1
                && paths[0] == edit.manifest.action.target
        }
        (EditTermination::StoppedUnknownEffect, EffectSummary::MayHaveMutated) => {
            edit.rename_armed.is_some()
        }
        _ => false,
    };
    if !valid {
        return Err(RegistryError::EvidenceConflict);
    }
    append(&mut edit.termination, termination, claim.terminal.is_some())?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests;
