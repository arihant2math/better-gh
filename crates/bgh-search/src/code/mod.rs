//! Code search: the trigram index (`index`) and `/search/code` (`search`).
//!
//! Why Postgres trigrams rather than tantivy: results must be filtered by
//! per-repository permissions that live in Postgres; a trigram GIN index
//! answers both substring (`ILIKE`) and regex (`~*`) queries, which a
//! token-based tantivy index can't without a second n-gram field; and it
//! keeps the index transactional (no separate on-disk state to back up,
//! rebuild or keep consistent across replicas). Blob contents are
//! deduplicated by SHA, so forks cost only their `code_files` rows.

pub mod index;
pub mod lang;
pub mod search;
