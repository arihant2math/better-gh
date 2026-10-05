//! All integration tests of `bgh-repos`, linked into one test binary (`it`).
//! Add a new test file as `tests/it/<name>.rs` and declare it below;
//! shared helpers live in the helper modules (see docs/BACKEND_PATTERNS.md).

mod common;
mod gitwork;

mod api;
mod bench;
mod branches;
mod browse;
mod collaborators;
mod commits;
mod contents;
mod download;
mod forks;
mod git_transport;
mod gitdb;
mod import;
mod keys;
mod lfs;
mod protection;
mod push_hardening;
mod push_rules;
mod settings;
mod social;
mod ssh;
