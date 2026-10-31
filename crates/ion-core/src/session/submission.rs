//! Request correlation lives in the same committed Turn fact, not a job ledger.
use super::*;

/// Application-authorized namespace and opaque delivery ID. These are not
/// credentials; authenticating and authorizing the namespace belongs to the caller.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "RequestKeyFields")]
pub struct RequestKey {
    scope: String,
    id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestKeyFields {
    scope: String,
    id: String,
}

impl TryFrom<RequestKeyFields> for RequestKey {
    type Error = SessionError;
    fn try_from(value: RequestKeyFields) -> Result<Self, Self::Error> {
        Self::new(value.scope, value.id)
    }
}

impl RequestKey {
    pub fn new(scope: String, id: String) -> Result<Self, SessionError> {
        if [&scope, &id].iter().any(|part| {
            part.len() > 256 || part.trim().is_empty() || part.chars().any(char::is_control)
        }) {
            return Err(SessionError::InvalidRequestKey);
        }
        Ok(Self { scope, id })
    }
    pub fn scope(&self) -> &str {
        &self.scope
    }
    pub fn id(&self) -> &str {
        &self.id
    }
}

/// Literal caller input; template expansion and local file decoding must happen
/// before constructing a durable request. Metadata is not persisted implicitly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmissionRequest {
    pub key: RequestKey,
    pub input: Message,
}

/// Committed acceptance and latest committed closure. `end: None` does not prove
/// execution is live: after process loss the unfinished work remains uncertain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestReceipt {
    pub session: uuid::Uuid,
    pub key: RequestKey,
    pub turn: u64,
    pub end: Option<TurnEndReason>,
}

#[derive(Debug)]
pub(crate) enum TurnAdmission {
    New {
        turn: u64,
        entry: u64,
        interrupted: usize,
        input: Message,
    },
    Existing(RequestReceipt),
}

impl Session {
    /// Inspect acceptance without repairing history, requiring an idle Session,
    /// or reconstructing execution services. No pending queue is promised.
    pub fn request_receipt(
        &self,
        key: &RequestKey,
    ) -> Result<Option<RequestReceipt>, SessionError> {
        let store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        request_receipt(&store.connection, self.id(), key, None)
    }

    /// Check immutable input as well as acceptance; differing reuse is an error.
    pub fn reconcile_request(
        &self,
        key: &RequestKey,
        input: &Message,
    ) -> Result<Option<RequestReceipt>, SessionError> {
        let store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        request_receipt(&store.connection, self.id(), key, Some(input))
    }
}

pub(super) fn request_receipt(
    connection: &Connection,
    session: uuid::Uuid,
    key: &RequestKey,
    input: Option<&Message>,
) -> Result<Option<RequestReceipt>, SessionError> {
    let body: Option<Vec<u8>> = connection.query_row(
        "SELECT body FROM entries WHERE json_extract(body, '$.kind') = 'turn_started' AND json_extract(body, '$.data.request') IS NOT NULL AND json_extract(body, '$.data.request.scope') = ?1 AND json_extract(body, '$.data.request.id') = ?2",
        params![key.scope(), key.id()], |row| row.get(0),
    ).optional()?;
    let Some(body) = body else { return Ok(None) };
    let SessionEntry::TurnStarted {
        turn,
        input: accepted,
        ..
    } = serde_json::from_slice(&body)?
    else {
        return Err(SessionError::InvalidHistory);
    };
    if input.is_some_and(|input| input != &accepted) {
        return Err(SessionError::RequestConflict);
    }
    let end: Option<Vec<u8>> = connection.query_row(
        "SELECT body FROM entries WHERE json_extract(body, '$.kind') = 'turn_ended' AND json_extract(body, '$.data.turn') = ?1",
        params![i64::try_from(turn).map_err(|_| SessionError::InvalidHistory)?], |row| row.get(0),
    ).optional()?;
    Ok(Some(RequestReceipt {
        session,
        key: key.clone(),
        turn,
        end: end
            .map(|body| match serde_json::from_slice(&body)? {
                SessionEntry::TurnEnded { reason, .. } => Ok(reason),
                _ => Err(SessionError::InvalidHistory),
            })
            .transpose()?,
    }))
}
