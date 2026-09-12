//! Durable session domain.
//!
//! The target session model owns retained agents/conversations, admitted
//! inputs, generic durable tasks, effects, and observations behind one
//! serialized mutation boundary. `lane` and the current tree helpers are
//! legacy implementation pieces retained only while equivalent target slices
//! are promoted; operation execution is not the long-term session model.

pub(crate) mod lane;
pub(crate) mod tree;
