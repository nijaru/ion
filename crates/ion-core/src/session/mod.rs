//! One session: a supervised, durably owned group of conversations.

mod command;
mod handle;
mod owner;
mod supervisor;

#[cfg(test)]
mod tests;

pub use command::{
    AdmissionReceipt, CancelReceipt, ConfigureRequest, EntryQuery, ResolveRequest, SessionEvent,
    SubmitRequest,
};
pub use handle::{SessionHandle, SessionWatch, WatchError};
pub use owner::{Session, SessionSpec};
pub use supervisor::{CloseOutcome, Services};

pub(crate) use command::Request;
