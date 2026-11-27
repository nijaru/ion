//! Internal Worker ABI for Rust journal replay and prepared append. This is
//! not a Session constructor or a second engine. The Durable Object supplies
//! a confirmed read cut and owns its writer generation.
use std::{
    cell::{Cell, RefCell},
    collections::BTreeSet,
};

use ion_ai::{Message, ModelRef};
use js_sys::{Array, Error, Object, Promise, Reflect, Uint8Array};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use wasm_bindgen::{JsCast, prelude::*};

use super::{
    AssistantRecord, ModelContextSnapshot, RecoveryBinding, RecoveryPoint, RequestKey,
    RequestReceipt, SessionEntry, SessionError, SubmissionRequest, TurnAdmission, TurnEndReason,
    journal::{self, AppendBatch, EncodedRecord, EntryRows, JournalIdentity, WorkingJournal},
    projection::ProjectionMode,
    store::Header,
    turn_write::{AdmissionPlan, TurnChange},
};

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(
        typescript_type = "{ readonly tail: bigint; readonly header: Uint8Array; readonly identity: { session: string; providerSession: string }; readRecord(seq: bigint): Uint8Array }"
    )]
    pub type ConfirmedCut;

    #[wasm_bindgen(method, getter, catch, js_name = tail)]
    fn cut_tail(this: &ConfirmedCut) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(method, getter, catch, js_name = header)]
    fn cut_header(this: &ConfirmedCut) -> Result<Uint8Array, JsValue>;
    #[wasm_bindgen(method, getter, catch, js_name = identity)]
    fn cut_identity(this: &ConfirmedCut) -> Result<Object, JsValue>;
    #[wasm_bindgen(method, catch, js_name = readRecord)]
    fn read_record(this: &ConfirmedCut, sequence: i64) -> Result<Uint8Array, JsValue>;

    #[wasm_bindgen(
        typescript_type = "{ append(batch: { identity: { session: string; providerSession: string }; expectedTail: bigint; bodies: Uint8Array[] }): Promise<bigint>; readRecord(seq: bigint, expectedTail: bigint): Uint8Array; verifyTail(expectedTail: bigint): void }"
    )]
    pub type FencedWriter;

    #[wasm_bindgen(method, catch, js_name = append)]
    fn write_batch(this: &FencedWriter, batch: &Object) -> Result<Promise, JsValue>;

    #[wasm_bindgen(method, catch, js_name = verifyTail)]
    fn verify_tail(this: &FencedWriter, expected_tail: i64) -> Result<(), JsValue>;

    #[wasm_bindgen(method, catch, js_name = readRecord)]
    fn read_owned_record(
        this: &FencedWriter,
        sequence: i64,
        expected_tail: i64,
    ) -> Result<Uint8Array, JsValue>;
}

/// An immutable projection of one committed cut. The writer is a separate
/// capability; an old cut can still be read but cannot advance a newer tail.
#[wasm_bindgen]
pub struct WorkerJournalCut(WorkingJournal);

#[wasm_bindgen]
impl WorkerJournalCut {
    #[wasm_bindgen(constructor)]
    pub fn open(source: ConfirmedCut) -> Result<WorkerJournalCut, JsValue> {
        let tail = i64::try_from(source.cut_tail()?)
            .ok()
            .and_then(|tail| u64::try_from(tail).ok())
            .ok_or_else(|| js_error(SessionError::InvalidHistory))?;
        let header = Header::decode(&bounded_bytes(&source.cut_header()?).map_err(js_error)?)
            .map_err(js_error)?;
        let identity = header.identity();
        let source_identity = source.cut_identity()?;
        identity
            .verify(JournalIdentity {
                session: identity_field(&source_identity, "session")?,
                provider_session: identity_field(&source_identity, "providerSession")?,
            })
            .map_err(js_error)?;
        let projection = journal::replay(
            CutRows {
                source: &source,
                position: 0,
                tail,
                body: Vec::new(),
            },
            ProjectionMode::Working,
            |entry| read_entry(entry, |seq| source.read_record(seq)),
        )
        .map_err(js_error)?;
        Ok(Self(WorkingJournal::open(identity, projection)))
    }

    #[wasm_bindgen(getter)]
    pub fn tail(&self) -> u64 {
        self.0.state.sequence
    }

    #[wasm_bindgen(getter, js_name = unfinishedTurn)]
    pub fn unfinished_turn(&self) -> Option<u64> {
        self.0.state.active
    }

    /// Move a confirmed projection into this isolate's claimed writer.
    #[wasm_bindgen(js_name = bindWriter)]
    pub fn bind_writer(self, writer: FencedWriter) -> WorkerJournal {
        WorkerJournal {
            working: RefCell::new(self.0),
            writer,
            phase: Cell::new(WritePhase::Idle),
        }
    }
}

/// The trusted host's journal owner, not an RPC surface or a Turn engine.
/// The host must reconstruct the binding and arrange wakeup before admission.
#[wasm_bindgen]
pub struct WorkerJournal {
    working: RefCell<WorkingJournal>,
    writer: FencedWriter,
    phase: Cell<WritePhase>,
}

#[wasm_bindgen]
impl WorkerJournal {
    #[wasm_bindgen(getter)]
    pub fn tail(&self) -> u64 {
        self.working.borrow().state.sequence
    }

    #[wasm_bindgen(getter, js_name = unfinishedTurn)]
    pub fn unfinished_turn(&self) -> Option<u64> {
        self.working.borrow().state.active
    }

    #[wasm_bindgen(getter, js_name = messageCount)]
    pub fn message_count(&self) -> usize {
        self.working.borrow().messages.len()
    }

    // The bridge carries these DTOs as JSON bytes, never via JS numbers.
    #[wasm_bindgen(js_name = requestReceipt)]
    pub fn request_receipt(&self, key: &[u8]) -> Result<Option<Vec<u8>>, JsValue> {
        let key = decode(key)?;
        self.working
            .borrow()
            .projected_request(&key)
            .map(|(_, receipt)| encode(receipt))
            .transpose()
    }

    #[wasm_bindgen(js_name = recoveryForRequest)]
    pub fn recovery_for_request(&self, key: &[u8]) -> Result<Option<Vec<u8>>, JsValue> {
        let key = decode(key)?;
        let working = self.working.borrow();
        let Some((_, receipt)) = working.projected_request(&key) else {
            return Ok(None);
        };
        working
            .recovery_for_receipt(receipt)
            .map_err(js_error)?
            .map(encode)
            .transpose()
    }

    pub async fn submit(
        &self,
        request: Vec<u8>,
        model: Vec<u8>,
        binding: Vec<u8>,
    ) -> Result<Vec<u8>, JsValue> {
        let permit = WritePermit::reserve(&self.phase)?;
        let SubmissionRequest { key, input } = decode(&request)?;
        let model: ModelRef = decode(&model)?;
        let binding: RecoveryBinding = decode(&binding)?;
        let (session, plan) = {
            let working = self.working.borrow();
            let session = working.identity().session;
            let plan = working
                .plan_admission(
                    input,
                    model,
                    Some(key.clone()),
                    Some(binding),
                    Vec::new(),
                    |key, input| {
                        reconcile_request(&working, key, input, |entry| {
                            self.read_at(entry, working.state.sequence)
                        })
                    },
                )
                .map_err(js_error)?;
            (session, plan)
        };
        let result = match plan {
            AdmissionPlan::Existing(receipt) => Submission::Existing { receipt },
            AdmissionPlan::Write(change) => {
                let TurnAdmission::New { turn, .. } = self.commit_change(permit, change).await?
                else {
                    unreachable!("a committed StartTurn accepts a new turn")
                };
                Submission::Accepted {
                    receipt: RequestReceipt {
                        session,
                        key,
                        turn,
                        end: None,
                    },
                }
            }
        };
        encode(result)
    }

    pub async fn resume(&self, point: Vec<u8>, binding: Vec<u8>) -> Result<Vec<u8>, JsValue> {
        let permit = WritePermit::reserve(&self.phase)?;
        let point: RecoveryPoint = decode(&point)?;
        let binding: RecoveryBinding = decode(&binding)?;
        let change = self
            .working
            .borrow()
            .plan_continuation(point, &binding)
            .map_err(js_error)?;
        let (point, interrupted) = self.commit_change(permit, change).await?;
        encode(Continuation { point, interrupted })
    }

    /// Used by the host that owns this active Turn. A previously constructed
    /// command cannot be applied to a later cut, even in the same Turn.
    #[wasm_bindgen(js_name = applyTurn)]
    pub async fn apply_turn(&self, expected_tail: u64, bytes: Vec<u8>) -> Result<Vec<u8>, JsValue> {
        let permit = WritePermit::reserve(&self.phase)?;
        let TurnCommand { turn, change } = decode(&bytes)?;
        let current = self.tail();
        if expected_tail != current {
            return Err(js_error(SessionError::JournalFrontierChanged {
                expected_tail,
                actual: i64::try_from(current).ok(),
            }));
        }
        macro_rules! commit {
            ($plan:ident($($arg:expr),*)) => {{
                let change = self.working.borrow().$plan(turn, $($arg),*).map_err(js_error)?;
                encode(self.commit_change(permit, change).await?)
            }};
        }
        match change {
            Command::PrepareRequest { model, context } => commit!(plan_request(model, context)),
            Command::Assistant {
                assistant,
                steering,
                completion,
            } => commit!(plan_assistant(assistant, steering, completion)),
            Command::TruncatedAssistant { assistant } => {
                commit!(plan_truncated_assistant(assistant))
            }
            Command::Inputs {
                steering,
                completion,
            } => commit!(plan_inputs(steering, completion)),
            Command::SettleTool {
                occurrence,
                result,
                context,
            } => commit!(plan_settlement(occurrence, result, context)),
            Command::Close {
                reason,
                not_dispatched,
            } => commit!(plan_turn_end(reason, &not_dispatched)),
        }
    }

    // Fixture for the other record types until the live Session owns every
    // mutation. Do not expose raw appends to requests or tools.
    pub async fn append(&self, records: Array) -> Result<u64, JsValue> {
        let permit = WritePermit::reserve(&self.phase)?;
        if records.length() == 0 {
            return Err(js_error("append requires a nonempty batch"));
        }
        let records = records
            .iter()
            .map(|value| {
                let array = value
                    .dyn_into::<Uint8Array>()
                    .map_err(|_| js_error("an encoded Session record must be a Uint8Array"))?;
                let bytes = bounded_bytes(&array).map_err(js_error)?;
                serde_json::from_slice::<SessionEntry>(&bytes).map_err(js_error)
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.commit_records(permit, &records).await
    }
}

impl WorkerJournal {
    async fn commit_change<C: TurnChange>(
        &self,
        mut permit: WritePermit<'_>,
        change: C,
    ) -> Result<C::Receipt, JsValue> {
        let tail = if change.entries().is_empty() {
            let tail = self.tail();
            let host_tail = i64::try_from(tail).map_err(js_error)?;
            permit.fence_on_drop = true;
            self.writer.verify_tail(host_tail)?;
            permit.release();
            tail
        } else {
            self.commit_records(permit, change.entries()).await?
        };
        Ok(change.committed(tail))
    }

    fn read_at(&self, entry: u64, tail: u64) -> Result<SessionEntry, SessionError> {
        let tail = i64::try_from(tail).map_err(|_| SessionError::InvalidHistory)?;
        read_entry(entry, |seq| self.writer.read_owned_record(seq, tail))
    }

    /// Keep the writer reservation, not a projection borrow or a transaction,
    /// across the host wait. Publish only the exact confirmed batch.
    async fn commit_records(
        &self,
        mut permit: WritePermit<'_>,
        records: &[SessionEntry],
    ) -> Result<u64, JsValue> {
        let prepared = {
            let working = self.working.borrow();
            let tail = working.state.sequence;
            working
                .prepare(
                    records,
                    |entry| self.read_at(entry, tail),
                    |_| {
                        Err(SessionError::Storage(
                            "the Worker bridge has no artifact store".into(),
                        ))
                    },
                )
                .map_err(js_error)?
        };
        permit.fence_on_drop = true;
        let committed = match prepared
            .commit_with(async move |batch| commit_batch(&self.writer, batch).await)
            .await
        {
            Ok(committed) => committed,
            Err(error) => {
                // This classification comes only from the private host adapter
                // after transactionSync returned the callback's rollback error.
                if code(&error).as_deref() == Some("rolled_back") {
                    permit.release();
                }
                return Err(error);
            }
        };
        let mut working = self.working.borrow_mut();
        working.publish(committed);
        let tail = working.state.sequence;
        permit.release();
        Ok(tail)
    }
}

#[derive(Deserialize)]
struct TurnCommand {
    turn: u64,
    #[serde(flatten)]
    change: Command,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Command {
    PrepareRequest {
        model: ModelRef,
        context: ModelContextSnapshot,
    },
    Assistant {
        assistant: AssistantRecord,
        #[serde(default)]
        steering: Vec<Message>,
        #[serde(default)]
        completion: Vec<crate::CompletionNotice>,
    },
    TruncatedAssistant {
        assistant: AssistantRecord,
    },
    Inputs {
        #[serde(default)]
        steering: Vec<Message>,
        #[serde(default)]
        completion: Vec<crate::CompletionNotice>,
    },
    SettleTool {
        occurrence: crate::ToolOccurrence,
        result: crate::ToolSettlement,
        context: Option<ModelContextSnapshot>,
    },
    Close {
        reason: TurnEndReason,
        #[serde(default)]
        not_dispatched: BTreeSet<String>,
    },
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Submission {
    Existing { receipt: RequestReceipt },
    Accepted { receipt: RequestReceipt },
}

#[derive(Serialize)]
struct Continuation {
    point: RecoveryPoint,
    interrupted: usize,
}

fn reconcile_request(
    working: &WorkingJournal,
    key: &RequestKey,
    input: &Message,
    read: impl FnOnce(u64) -> Result<SessionEntry, SessionError>,
) -> Result<Option<RequestReceipt>, SessionError> {
    let Some((entry, receipt)) = working.projected_request(key) else {
        return Ok(None);
    };
    let SessionEntry::TurnStarted {
        turn,
        input: accepted,
        request: Some(found_key),
        ..
    } = read(entry)?
    else {
        return Err(SessionError::InvalidHistory);
    };
    if turn != receipt.turn || &found_key != key {
        return Err(SessionError::InvalidHistory);
    }
    if &accepted != input {
        return Err(SessionError::RequestConflict);
    }
    Ok(Some(receipt))
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, JsValue> {
    if bytes.len() > journal::MAX_ENTRY_BYTES {
        return Err(js_error(SessionError::EntryTooLarge));
    }
    serde_json::from_slice(bytes).map_err(js_error)
}

fn encode(value: impl Serialize) -> Result<Vec<u8>, JsValue> {
    serde_json::to_vec(&value).map_err(js_error)
}

#[derive(Clone, Copy)]
enum WritePhase {
    Idle,
    Busy,
    Fenced,
}

/// Dropping an in-flight write continuation without a definitive outcome
/// fences this Rust owner, even if the host promise eventually resolves.
struct WritePermit<'a> {
    phase: &'a Cell<WritePhase>,
    fence_on_drop: bool,
}

impl<'a> WritePermit<'a> {
    fn reserve(phase: &'a Cell<WritePhase>) -> Result<Self, JsValue> {
        match phase.get() {
            WritePhase::Idle => {
                phase.set(WritePhase::Busy);
                Ok(Self {
                    phase,
                    fence_on_drop: false,
                })
            }
            WritePhase::Busy => Err(host_error("busy", "journal writer is busy")),
            WritePhase::Fenced => Err(host_error("fenced", "journal writer is fenced")),
        }
    }

    fn release(mut self) {
        self.fence_on_drop = false;
    }
}

impl Drop for WritePermit<'_> {
    fn drop(&mut self) {
        self.phase.set(if self.fence_on_drop {
            WritePhase::Fenced
        } else {
            WritePhase::Idle
        });
    }
}

struct CutRows<'a> {
    source: &'a ConfirmedCut,
    position: u64,
    tail: u64,
    body: Vec<u8>,
}

impl EntryRows for CutRows<'_> {
    fn next_record(&mut self) -> Result<Option<EncodedRecord<'_>>, SessionError> {
        if self.position == self.tail {
            return Ok(None);
        }
        self.position += 1;
        // Release the previous record before moving another across the ABI.
        self.body = Vec::new();
        let (sequence, body) = record_bytes(self.position, |seq| self.source.read_record(seq))?;
        self.body = body;
        Ok(Some(EncodedRecord {
            sequence,
            body: &self.body,
        }))
    }
}

fn read_entry(
    entry: u64,
    read: impl FnOnce(i64) -> Result<Uint8Array, JsValue>,
) -> Result<SessionEntry, SessionError> {
    let (sequence, body) = record_bytes(entry, read)?;
    EncodedRecord {
        sequence,
        body: &body,
    }
    .decode(entry)
}

fn record_bytes(
    entry: u64,
    read: impl FnOnce(i64) -> Result<Uint8Array, JsValue>,
) -> Result<(i64, Vec<u8>), SessionError> {
    let sequence = i64::try_from(entry).map_err(|_| SessionError::InvalidHistory)?;
    let bytes = read(sequence)
        .map_err(|error| SessionError::Storage(format!("read hosted record {entry}: {error:?}")))?;
    Ok((sequence, bounded_bytes(&bytes)?))
}

fn bounded_bytes(bytes: &Uint8Array) -> Result<Vec<u8>, SessionError> {
    if bytes.length() as usize > journal::MAX_ENTRY_BYTES {
        return Err(SessionError::EntryTooLarge);
    }
    Ok(bytes.to_vec())
}

async fn commit_batch(writer: &FencedWriter, batch: AppendBatch) -> Result<(), JsValue> {
    let expected = i64::try_from(batch.expected_tail).map_err(js_error)?;
    let count = i64::try_from(batch.encoded.len()).map_err(js_error)?;
    let tail = expected
        .checked_add(count)
        .ok_or_else(|| js_error(SessionError::InvalidHistory))?;
    let identity = Object::new();
    set(
        &identity,
        "session",
        &batch.identity.session.to_string().into(),
    )?;
    set(
        &identity,
        "providerSession",
        &batch.identity.provider_session.to_string().into(),
    )?;
    let bodies: Array = batch
        .encoded
        .into_iter()
        .map(|body| Uint8Array::from(body.as_slice()))
        .collect();
    let request = Object::new();
    set(&request, "identity", &identity)?;
    set(&request, "expectedTail", &expected.into())?;
    set(&request, "bodies", &bodies)?;
    let reply = writer.write_batch(&request)?.await?;
    if i64::try_from(reply).ok() != Some(tail) {
        return Err(host_error(
            "uncertain",
            "host did not acknowledge the exact journal tail",
        ));
    }
    Ok(())
}

fn identity_field(object: &Object, key: &str) -> Result<uuid::Uuid, JsValue> {
    Reflect::get(object, &key.into())?
        .as_string()
        .and_then(|value| uuid::Uuid::parse_str(&value).ok())
        .ok_or_else(|| js_error(SessionError::InvalidDatabase))
}

fn set(object: &JsValue, key: &str, value: &JsValue) -> Result<(), JsValue> {
    if !Reflect::set(object, &key.into(), value)? {
        return Err(js_error("could not populate the journal bridge request"));
    }
    Ok(())
}

fn code(error: &JsValue) -> Option<String> {
    Reflect::get(error, &"code".into()).ok()?.as_string()
}

fn host_error(code: &str, message: &str) -> JsValue {
    let error = Error::new(message);
    match set(error.as_ref(), "code", &code.into()) {
        Ok(()) => error.into(),
        Err(error) => error,
    }
}

fn js_error(error: impl std::fmt::Display) -> JsValue {
    Error::new(&error.to_string()).into()
}
