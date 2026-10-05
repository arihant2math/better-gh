//! All integration tests of `bgh-actions`, linked into one test binary (`it`).
//! Add a new test file as `tests/it/<name>.rs` and declare it below;
//! shared helpers live in the helper modules (see docs/BACKEND_PATTERNS.md).

mod common;

mod cache;
mod deployments;
mod e2e;
mod reusable;
mod runner_groups;
mod runs;
mod security;
mod settings;
mod smoke;
mod triggers;
mod ui;
