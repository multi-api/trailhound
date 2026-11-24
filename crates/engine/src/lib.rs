//! Trailhound engine: a low-memory full-text search engine.
//!
//! Storage model: every index is a set of immutable, memory-mapped segment
//! files plus one small manifest. Writers build new segment files, fsync
//! them, and then atomically replace the manifest. A crash at any point
//! leaves either the old or the new manifest on disk, never a mix.

pub mod error;
pub mod filter;
pub mod index;
pub mod search;
pub mod segment;
pub mod settings;
pub mod tokenizer;
mod util;

pub use error::{Error, Result};
pub use index::{configure_indexing, Index, IndexStats};
pub use search::SearchQuery;
pub use settings::Settings;
pub use util::{atomic_write, now_rfc3339};
