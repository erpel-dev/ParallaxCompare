//! ParallaxCompare core: a UI-agnostic compare engine.
//!
//! Every comparable thing (local folder, archive, Git revision, Git worktree) is
//! exposed as a [`vfs::Vfs`] tree. The folder compare engine ([`compare`]), the
//! text diff ([`textdiff`]) and the highlighter ([`highlight`]) only ever see
//! those trees, so any source can be compared against any other source.

pub mod compare;
pub mod conflicts;
pub mod convert;
pub mod edit;
pub mod error;
pub mod highlight;
pub mod merge;
pub mod merge_session;
pub mod ops;
mod process;
pub mod progress;
pub mod pull_request;
pub mod remote;
pub mod search;
pub mod source;
pub mod source_preview;
pub mod syntax_merge;
pub mod text;
pub mod textdiff;
pub mod versions;
pub mod vfs;
pub mod watch;

pub use error::{Error, Result};
