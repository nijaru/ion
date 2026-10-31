//! Ion's terminal substrate.
//!
//! This crate owns terminal process state and the renderer-facing terminal
//! contract. Application state and runtime effects stay in the `ion` crate.

mod capabilities;
mod colors;
mod input;
mod program_status;
mod publication;
mod requirements;
mod session;

pub mod screen;

pub use capabilities::{CapabilitySupport, TerminalCapabilities};
pub use colors::{Appearance, TerminalColors};
pub use input::{
    InputError, InputEvent, KeyCode, KeyEvent, Modifiers, MouseEvent, MouseKind, Size,
};
pub use program_status::ProgramStatus;
pub use publication::PaintReceipt;
pub use requirements::TerminalRequirements;
pub use screen::{Frame, Screen, Surface};
pub use session::{SecretRejection, TerminalOutput, TerminalSession, TerminalWriter};
