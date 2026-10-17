//! ParallaxCompare core: a UI-agnostic compare engine.
//!
//! Every comparable thing (local folder, archive, Git revision, Git worktree) is
//! exposed as a [`vfs::Vfs`] tree. The folder compare engine ([`compare`]), the
//! text diff ([`textdiff`]) and the highlighter ([`highlight`]) only ever see
//! those trees, so any source can be compared against any other source.

pub mod compare;
pub mod error;
pub mod highlight;
pub mod ops;
pub mod source;
pub mod text;
pub mod textdiff;
pub mod vfs;

pub use error::{Error, Result};
