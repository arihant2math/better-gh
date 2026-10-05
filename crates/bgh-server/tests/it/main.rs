//! All integration tests of `bgh-server`, linked into one test binary (`it`).
//! Add a new test file as `tests/it/<name>.rs` and declare it below;
//! shared helpers live in the helper modules (see docs/BACKEND_PATTERNS.md).

mod api_compat;
mod events;
mod infra;
mod server;
