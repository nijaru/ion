//! Internal Worker ABI for Rust journal replay and prepared append. This is
//! not a Session constructor or a second engine. The Durable Object supplies
//! a confirmed read cut and owns its writer generation.
use js_sys::{Array, Error, Object, Promise, Reflect, Uint8Array};
use wasm_bindgen::{JsCast, prelude::*};

use super::{
    HistoryProjection, SessionEntry, SessionError,
    journal::{self, AppendBasis, AppendBatch, EncodedRecord, EntryRows, JournalIdentity},
    projection::ProjectionMode,
    store::Header,
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
        typescript_type = "{ append(batch: { identity: { session: string; providerSession: string }; expectedTail: bigint; bodies: Uint8Array[] }): Promise<bigint> }"
    )]
    pub type FencedWriter;

    #[wasm_bindgen(method, catch, js_name = append)]
    fn write_batch(this: &FencedWriter, batch: &Object) -> Result<Promise, JsValue>;
}

/// An immutable projection of one committed cut. The writer is a separate
/// capability; an old cut can still be read but cannot advance a newer tail.
#[wasm_bindgen]
pub struct WorkerJournalCut {
    source: ConfirmedCut,
    identity: JournalIdentity,
    projection: HistoryProjection,
}

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
            |entry| read_entry(&source, entry),
        )
        .map_err(js_error)?;
        Ok(Self {
            source,
            identity,
            projection,
        })
    }

    #[wasm_bindgen(getter)]
    pub fn tail(&self) -> u64 {
        self.projection.state.sequence
    }

    #[wasm_bindgen(getter, js_name = unfinishedTurn)]
    pub fn unfinished_turn(&self) -> Option<u64> {
        self.projection.state.active
    }

    /// Validate and encode the batch through the normal Rust journal before
    /// invoking SQL. This cut remains immutable, including if the reply is
    /// lost. Open a new confirmed cut to observe later state.
    pub async fn append(&self, writer: FencedWriter, records: Array) -> Result<u64, JsValue> {
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
        // The host stores opaque chunks and cannot enforce the native SQLite
        // expression indexes. Check their replay invariants before writing.
        self.projection
            .keys
            .validate_extension(&records)
            .map_err(js_error)?;
        let prepared = AppendBasis {
            identity: self.identity,
            state: &self.projection.state,
            context_index: &self.projection.context_index,
            settled: &self.projection.settled,
            message_count: self.projection.messages.len(),
        }
        .prepare(
            &records,
            |entry| read_entry(&self.source, entry),
            |_| {
                Err(SessionError::Storage(
                    "the Worker bridge has no artifact store".into(),
                ))
            },
        )
        .map_err(js_error)?;
        let committed = prepared
            .commit_with(async move |batch| commit_batch(&writer, batch).await)
            .await?;
        Ok(committed.into_projection().candidate.sequence)
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
        let (sequence, body) = record_bytes(self.source, self.position)?;
        self.body = body;
        Ok(Some(EncodedRecord {
            sequence,
            body: &self.body,
        }))
    }
}

fn read_entry(source: &ConfirmedCut, entry: u64) -> Result<SessionEntry, SessionError> {
    let (sequence, body) = record_bytes(source, entry)?;
    EncodedRecord {
        sequence,
        body: &body,
    }
    .decode(entry)
}

fn record_bytes(source: &ConfirmedCut, entry: u64) -> Result<(i64, Vec<u8>), SessionError> {
    let sequence = i64::try_from(entry).map_err(|_| SessionError::InvalidHistory)?;
    let bytes = source
        .read_record(sequence)
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
        let error = Error::new("host did not acknowledge the exact journal tail");
        set(error.as_ref(), "code", &"uncertain".into())?;
        return Err(error.into());
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

fn js_error(error: impl std::fmt::Display) -> JsValue {
    Error::new(&error.to_string()).into()
}
