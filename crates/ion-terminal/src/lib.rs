//! Ion's terminal substrate.
//!
//! This crate owns terminal process state and the renderer-facing terminal
//! contract. Application state and runtime effects stay in the `ion` crate.

mod capabilities;
mod input;
mod requirements;
mod session;

pub mod screen;

pub use capabilities::{CapabilitySupport, TerminalCapabilities};
pub use input::{
    FocusEvent, InputEvent, InputStream, KeyCode, KeyEvent, Modifiers, MouseEvent, MouseKind, Size,
};
pub use requirements::TerminalRequirements;
pub use screen::{Frame, Screen, Surface};
pub use session::{TerminalOutput, TerminalSession, install_panic_hook};
