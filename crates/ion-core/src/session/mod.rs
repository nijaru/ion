//! Durable session topology: passive semantic history plus active lane state.
//! Operation execution and transition ownership live in `operation`.

pub(crate) mod lane;
pub(crate) mod tree;
